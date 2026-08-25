//! Async multiplex transport for the Neon3 control plane.
//!
//! This module keeps the existing `neon3.rpc` JSON envelope and wire framing
//! (4-byte big-endian length prefix) intact, but replaces the synchronous
//! one-request-per-connection model with:
//!
//! - a persistent, multiplexed client connection with `request_id` demux,
//!   per-request timeout/cancellation and a bounded outbound queue;
//! - a server accept/read loop that never blocks on a single handler, with a
//!   global concurrency bound (semaphore) and a per-connection writer task.
//!
//! The synchronous `RpcClient` / `RpcServer` / `EventClient` in this crate are
//! left untouched as the compatibility surface. Async and sync peers can talk to
//! each other because they share the same framing and JSON contract.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use neon_protocol::{RequestId, RpcError, RpcRequest, RpcResponse, RpcStatus};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};

use crate::{DEFAULT_MAX_FRAME_SIZE, TransportError, encode_frame, ensure_loopback, map_io_error};

/// Transport-independent control-plane contract. `neon-protocol` types and the
/// `neon3.rpc` semantics are unchanged; this trait only abstracts the transport
/// so future adapters (Windows named pipe, Unix domain socket, remote tonic) can
/// implement the same surface without touching protocol semantics.
pub trait RpcTransport: Send + Sync {
    fn call(
        &self,
        request: &RpcRequest,
    ) -> impl Future<Output = Result<RpcResponse, TransportError>> + Send;

    fn call_with_timeout(
        &self,
        request: &RpcRequest,
        timeout: Duration,
    ) -> impl Future<Output = Result<RpcResponse, TransportError>> + Send;
}

/// Read one length-prefixed frame from an async reader using the same
/// 4-byte big-endian length framing as the synchronous transport.
async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_frame_size: usize,
) -> Result<Vec<u8>, TransportError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await.map_err(map_io_error)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > max_frame_size {
        return Err(TransportError::FrameTooLarge {
            size: length,
            max: max_frame_size,
        });
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(map_io_error)?;
    Ok(payload)
}

/// Shared state between the public [`AsyncRpcClient`] handle and its background
/// reader task.
struct ClientShared {
    pending: Mutex<HashMap<RequestId, oneshot::Sender<Result<RpcResponse, TransportError>>>>,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    orphan_responses: AtomicU64,
    shutdown: Notify,
}

/// A persistent, multiplexing RPC client.
///
/// Unlike the synchronous `RpcClient` (one request per connection), this client
/// keeps a single loopback connection open and demultiplexes responses by
/// `request_id`, so many requests can be in flight at once without reconnecting.
/// Clones share the same underlying connection.
#[derive(Clone)]
pub struct AsyncRpcClient {
    shared: Arc<ClientShared>,
    max_frame_size: usize,
    default_timeout: Option<Duration>,
}

impl AsyncRpcClient {
    pub async fn connect(endpoint: SocketAddr) -> Result<Self, TransportError> {
        Self::connect_with(endpoint, DEFAULT_MAX_FRAME_SIZE, 256).await
    }

    pub async fn connect_with(
        endpoint: SocketAddr,
        max_frame_size: usize,
        outbound_capacity: usize,
    ) -> Result<Self, TransportError> {
        ensure_loopback(endpoint)?;
        let stream = TcpStream::connect(endpoint).await.map_err(map_io_error)?;
        stream.set_nodelay(true).ok();

        let (read, write) = stream.into_split();
        let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(outbound_capacity);
        let shared = Arc::new(ClientShared {
            pending: Mutex::new(HashMap::new()),
            outbound_tx,
            orphan_responses: AtomicU64::new(0),
            shutdown: Notify::new(),
        });

        // Writer task: drains the bounded outbound queue onto the socket.
        {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut write = write;
                let mut rx = outbound_rx;
                loop {
                    tokio::select! {
                        _ = shared.shutdown.notified() => break,
                        frame = rx.recv() => match frame {
                            Some(frame) => {
                                if write.write_all(&frame).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        },
                    }
                }
            });
        }

        // Reader task: demultiplexes inbound frames to the pending map.
        {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut read = read;
                loop {
                    tokio::select! {
                        _ = shared.shutdown.notified() => break,
                        frame = read_frame(&mut read, max_frame_size) => {
                            let payload = match frame {
                                Ok(payload) => payload,
                                Err(_) => break,
                            };
                            let response: RpcResponse = match serde_json::from_slice(&payload) {
                                Ok(response) => response,
                                Err(_) => continue,
                            };
                            let entry = shared
                                .pending
                                .lock()
                                .unwrap()
                                .remove(&response.request_id);
                            match entry {
                                Some(tx) => {
                                    let _ = tx.send(Ok(response));
                                }
                                None => {
                                    // Late response for a timed-out/cancelled request.
                                    shared.orphan_responses.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
                // Connection closed: fail every still-pending request.
                let drained = std::mem::take(&mut *shared.pending.lock().unwrap());
                for (_, tx) in drained {
                    let _ = tx.send(Err(TransportError::ConnectionClosed));
                }
            });
        }

        Ok(Self {
            shared,
            max_frame_size,
            default_timeout: None,
        })
    }

    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = Some(timeout);
        self
    }

    pub async fn call(&self, request: &RpcRequest) -> Result<RpcResponse, TransportError> {
        self.call_impl(request, self.default_timeout).await
    }

    pub async fn call_with_timeout(
        &self,
        request: &RpcRequest,
        timeout: Duration,
    ) -> Result<RpcResponse, TransportError> {
        self.call_impl(request, Some(timeout)).await
    }

    async fn call_impl(
        &self,
        request: &RpcRequest,
        timeout: Option<Duration>,
    ) -> Result<RpcResponse, TransportError> {
        let request_id = request.request_id.clone();
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);

        let payload = serde_json::to_vec(request)?;
        let frame = encode_frame(&payload, self.max_frame_size)?;
        if self.shared.outbound_tx.send(frame).await.is_err() {
            self.shared.pending.lock().unwrap().remove(&request_id);
            return Err(TransportError::ConnectionClosed);
        }

        match timeout {
            Some(timeout) => match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(Ok(response))) => Ok(response),
                Ok(Ok(Err(error))) => Err(error),
                Ok(Err(_)) => Err(TransportError::ConnectionClosed),
                Err(_) => {
                    self.shared.pending.lock().unwrap().remove(&request_id);
                    Err(TransportError::Timeout)
                }
            },
            None => match rx.await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(TransportError::ConnectionClosed),
            },
        }
    }

    /// Cancel an in-flight request. The waiter receives `TransportError::Cancelled`.
    pub fn cancel(&self, request_id: &RequestId) {
        if let Some(tx) = self.shared.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(Err(TransportError::Cancelled));
        }
    }

    /// Number of requests currently awaiting a response on this connection.
    pub fn in_flight(&self) -> usize {
        self.shared.pending.lock().unwrap().len()
    }

    /// Number of responses received that had no matching pending request
    /// (late responses for timed-out or cancelled requests).
    pub fn orphan_responses(&self) -> u64 {
        self.shared.orphan_responses.load(Ordering::Relaxed)
    }

    /// Close the connection and fail all pending requests.
    pub fn shutdown(&self) {
        self.shared.shutdown.notify_waiters();
        let drained = std::mem::take(&mut *self.shared.pending.lock().unwrap());
        for (_, tx) in drained {
            let _ = tx.send(Err(TransportError::ConnectionClosed));
        }
    }
}

impl RpcTransport for AsyncRpcClient {
    fn call(
        &self,
        request: &RpcRequest,
    ) -> impl Future<Output = Result<RpcResponse, TransportError>> + Send {
        async move { AsyncRpcClient::call(self, request).await }
    }

    fn call_with_timeout(
        &self,
        request: &RpcRequest,
        timeout: Duration,
    ) -> impl Future<Output = Result<RpcResponse, TransportError>> + Send {
        async move { AsyncRpcClient::call_with_timeout(self, request, timeout).await }
    }
}

/// An async RPC server that dispatches requests concurrently.
///
/// Handlers never run inline on the accept/read loop, so a slow request cannot
/// block `service.health`, `debug.*`, or other connections. A global semaphore
/// bounds in-flight work; the per-connection writer serializes responses.
pub struct AsyncRpcServer {
    listener: TcpListener,
    max_frame_size: usize,
    max_concurrent: usize,
}

struct ServerStop {
    requested: AtomicBool,
    notified: Notify,
}

impl AsyncRpcServer {
    pub async fn bind(endpoint: SocketAddr) -> Result<Self, TransportError> {
        Self::bind_with(endpoint, DEFAULT_MAX_FRAME_SIZE, 256).await
    }

    pub async fn bind_with(
        endpoint: SocketAddr,
        max_frame_size: usize,
        max_concurrent: usize,
    ) -> Result<Self, TransportError> {
        ensure_loopback(endpoint)?;
        let listener = TcpListener::bind(endpoint).await.map_err(map_io_error)?;
        Ok(Self {
            listener,
            max_frame_size,
            max_concurrent,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.listener.local_addr().map_err(map_io_error)
    }

    /// Serve with a synchronous handler. The handler is run on the blocking
    /// thread pool so a slow or blocking handler never stalls async tasks.
    pub async fn serve<F>(self, handler: F) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> RpcResponse + Send + Sync + Clone + 'static,
    {
        self.serve_until(handler, |_| false).await
    }

    /// Serve with a synchronous handler and a stop predicate. When `should_stop`
    /// returns true for a handled request, the accept loop stops after that
    /// request is responded to (mirrors the legacy `serve_until` shutdown).
    pub async fn serve_until<F, S>(self, handler: F, should_stop: S) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> RpcResponse + Send + Sync + Clone + 'static,
        S: Fn(&RpcRequest) -> bool + Send + Sync + Clone + 'static,
    {
        let handler = Arc::new(handler);
        self.serve_inner(
            move |request| {
                let handler = Arc::clone(&handler);
                let request_id = request.request_id.clone();
                async move {
                    match tokio::task::spawn_blocking(move || handler(request)).await {
                        Ok(response) => response,
                        Err(_) => failed_response(request_id, "handler_panicked"),
                    }
                }
            },
            should_stop,
        )
        .await
    }

    /// Serve with an asynchronous handler.
    pub async fn serve_async<F, Fut>(self, handler: F) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = RpcResponse> + Send + 'static,
    {
        self.serve_inner(handler, |_| false).await
    }

    async fn serve_inner<F, Fut, S>(self, handler: F, should_stop: S) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = RpcResponse> + Send + 'static,
        S: Fn(&RpcRequest) -> bool + Send + Sync + Clone + 'static,
    {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        let stop = Arc::new(ServerStop {
            requested: AtomicBool::new(false),
            notified: Notify::new(),
        });
        loop {
            if stop.requested.load(Ordering::Relaxed) {
                break;
            }
            let (stream, _) = tokio::select! {
                _ = stop.notified.notified() => {
                    if stop.requested.load(Ordering::Relaxed) {
                        break;
                    }
                    continue;
                }
                accepted = self.listener.accept() => accepted.map_err(map_io_error)?,
            };
            let handler = handler.clone();
            let should_stop = should_stop.clone();
            let semaphore = Arc::clone(&semaphore);
            let stop = Arc::clone(&stop);
            let max_frame_size = self.max_frame_size;
            tokio::spawn(async move {
                let _ = handle_connection(
                    stream,
                    max_frame_size,
                    handler,
                    should_stop,
                    stop,
                    semaphore,
                )
                .await;
            });
        }
        Ok(())
    }
}

async fn handle_connection<F, Fut, S>(
    stream: TcpStream,
    max_frame_size: usize,
    handler: F,
    should_stop: S,
    stop: Arc<ServerStop>,
    semaphore: Arc<Semaphore>,
) -> Result<(), TransportError>
where
    F: Fn(RpcRequest) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = RpcResponse> + Send + 'static,
    S: Fn(&RpcRequest) -> bool + Send + Sync + Clone + 'static,
{
    let (mut read, write) = stream.into_split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(256);

    let writer = tokio::spawn(async move {
        let mut write = write;
        while let Some(frame) = outbound_rx.recv().await {
            if write.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    loop {
        let payload = match read_frame(&mut read, max_frame_size).await {
            Ok(payload) => payload,
            Err(_) => break,
        };
        let request: RpcRequest = match serde_json::from_slice(&payload) {
            Ok(request) => request,
            Err(_) => continue,
        };
        let stop_requested = should_stop(&request);

        let handler = handler.clone();
        let semaphore = Arc::clone(&semaphore);
        let stop = Arc::clone(&stop);
        let outbound_tx = outbound_tx.clone();
        tokio::spawn(async move {
            let permit = match semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            };
            let response = handler(request).await;
            drop(permit);
            if let Ok(payload) = serde_json::to_vec(&response) {
                if let Ok(frame) = encode_frame(&payload, max_frame_size) {
                    let _ = outbound_tx.send(frame).await;
                }
            }
            if stop_requested {
                stop.requested.store(true, Ordering::Relaxed);
                stop.notified.notify_one();
            }
        });
    }

    drop(outbound_tx);
    let _ = writer.await;
    Ok(())
}

// ===== Blocking (synchronous) compatibility wrappers =====
//
// Neon3 services are synchronous today. These wrappers encapsulate a tokio
// runtime inside `neon-ipc` so callers can adopt the async multiplex transport
// without adding a tokio dependency or managing a runtime themselves. The
// synchronous `RpcClient`/`RpcServer` in the parent module remain available;
// these are the drop-in replacements for per-request reconnects and the
// serial `serve_until` loop.

/// A synchronous RPC server handle that owns its tokio runtime so `bind` and
/// `serve` run on the **same** reactor. The listener is registered to this
/// runtime and stays valid for the lifetime of the serve loop.
///
/// This is the synchronous replacement for `RpcServer::bind` + `serve_until`.
pub struct BlockingRpcServer {
    runtime: tokio::runtime::Runtime,
    server: AsyncRpcServer,
}

impl BlockingRpcServer {
    pub fn bind(endpoint: SocketAddr) -> Result<Self, TransportError> {
        Self::bind_with(endpoint, DEFAULT_MAX_FRAME_SIZE, 256)
    }

    pub fn bind_with(
        endpoint: SocketAddr,
        max_frame_size: usize,
        max_concurrent: usize,
    ) -> Result<Self, TransportError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(map_io_error)?;
        let server = runtime.block_on(AsyncRpcServer::bind_with(
            endpoint,
            max_frame_size,
            max_concurrent,
        ))?;
        Ok(Self { runtime, server })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.server.local_addr()
    }

    /// Serve synchronously with a synchronous handler. Dispatches each request
    /// to its own task so a slow handler cannot block health/debug/shutdown.
    pub fn serve<F>(self, handler: F) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> RpcResponse + Send + Sync + Clone + 'static,
    {
        let Self { runtime, server } = self;
        runtime.block_on(server.serve(handler))
    }

    /// Serve synchronously with a stop predicate, mirroring the legacy
    /// `serve_until` shutdown semantics (`service.shutdown` stops the loop).
    pub fn serve_until<F, S>(self, handler: F, should_stop: S) -> Result<(), TransportError>
    where
        F: Fn(RpcRequest) -> RpcResponse + Send + Sync + Clone + 'static,
        S: Fn(&RpcRequest) -> bool + Send + Sync + Clone + 'static,
    {
        let Self { runtime, server } = self;
        runtime.block_on(server.serve_until(handler, should_stop))
    }
}

/// A synchronous handle over a persistent, multiplexing [`AsyncRpcClient`].
///
/// Encapsulates its own tokio runtime so synchronous callers can reuse a single
/// connection (no per-request reconnect, no per-request TCP/JSON setup) without
/// adding a tokio dependency. `call` keeps the same signature as the legacy
/// `RpcClient::call`, so call sites only change the constructor.
pub struct BlockingRpcClient {
    runtime: tokio::runtime::Runtime,
    inner: AsyncRpcClient,
}

impl BlockingRpcClient {
    pub fn connect(endpoint: SocketAddr) -> Result<Self, TransportError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(map_io_error)?;
        let inner = runtime.block_on(AsyncRpcClient::connect(endpoint))?;
        Ok(Self { runtime, inner })
    }

    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.with_default_timeout(timeout);
        self
    }

    pub fn call(&self, request: &RpcRequest) -> Result<RpcResponse, TransportError> {
        self.runtime.block_on(self.inner.call(request))
    }

    pub fn call_with_timeout(
        &self,
        request: &RpcRequest,
        timeout: Duration,
    ) -> Result<RpcResponse, TransportError> {
        self.runtime
            .block_on(self.inner.call_with_timeout(request, timeout))
    }

    pub fn in_flight(&self) -> usize {
        self.inner.in_flight()
    }

    pub fn orphan_responses(&self) -> u64 {
        self.inner.orphan_responses()
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

fn failed_response(request_id: RequestId, code: &str) -> RpcResponse {
    RpcResponse {
        request_id,
        status: RpcStatus::Failed,
        revision: None,
        result: None,
        snapshot: None,
        error: Some(RpcError {
            code: code.to_owned(),
            message: code.to_owned(),
            current_revision: None,
            object_id: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, ServiceName};
    use serde_json::json;
    use std::time::Instant;

    fn request(id: &str, method: &str) -> RpcRequest {
        RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId(id.into()),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "test".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName("test-service".into()),
            method: method.into(),
            params: json!({}),
            expected_revision: None,
            idempotency_key: None,
        }
    }

    fn echo(request: RpcRequest) -> RpcResponse {
        RpcResponse {
            request_id: request.request_id.clone(),
            status: RpcStatus::Accepted,
            revision: None,
            result: Some(json!({ "echo": request.method })),
            snapshot: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn multiplexes_many_inflight_requests_over_one_connection() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(echo));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let mut handles = Vec::new();
        for i in 0..64 {
            let client = client.clone();
            let id = format!("req-{i}");
            handles.push(tokio::spawn(async move {
                let response = client.call(&request(&id, "service.health")).await.unwrap();
                (id, response)
            }));
        }
        for handle in handles {
            let (id, response) = handle.await.unwrap();
            assert_eq!(response.request_id.0, id);
            assert_eq!(response.status, RpcStatus::Accepted);
        }
        assert_eq!(client.in_flight(), 0);
        assert_eq!(client.orphan_responses(), 0);
    }

    #[tokio::test]
    async fn slow_handler_does_not_block_fast_request() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(|req| {
            if req.method == "slow" {
                std::thread::sleep(Duration::from_millis(200));
            }
            echo(req)
        }));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let slow = {
            let client = client.clone();
            tokio::spawn(async move {
                let start = Instant::now();
                client.call(&request("slow", "slow")).await.unwrap();
                start.elapsed()
            })
        };

        // Give the slow request a head start so it occupies a handler first.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let fast_start = Instant::now();
        let response = client.call(&request("fast", "fast")).await.unwrap();
        let fast_elapsed = fast_start.elapsed();
        assert_eq!(response.request_id.0, "fast");
        assert!(
            fast_elapsed < Duration::from_millis(100),
            "fast request was blocked by the slow handler: {fast_elapsed:?}"
        );

        let slow_elapsed = slow.await.unwrap();
        assert!(slow_elapsed >= Duration::from_millis(200));
    }

    #[tokio::test]
    async fn timeout_leaves_no_pending_and_late_response_is_orphaned() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(|req| {
            std::thread::sleep(Duration::from_millis(200));
            echo(req)
        }));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let result = client
            .call_with_timeout(&request("t", "slow"), Duration::from_millis(10))
            .await;
        assert!(matches!(result, Err(TransportError::Timeout)));
        assert_eq!(client.in_flight(), 0);

        // Wait for the late response; it must be counted as an orphan.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(client.orphan_responses(), 1);
    }

    #[tokio::test]
    async fn cancel_returns_cancelled_error() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(|req| {
            std::thread::sleep(Duration::from_millis(200));
            echo(req)
        }));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let handle = {
            let client = client.clone();
            tokio::spawn(async move { client.call(&request("c", "slow")).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        client.cancel(&RequestId("c".into()));

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(TransportError::Cancelled)));
        assert_eq!(client.in_flight(), 0);
    }

    #[tokio::test]
    async fn async_client_talks_to_sync_server() {
        let server = crate::RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = std::thread::spawn(move || server.serve_one(echo));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let response = client.call(&request("x", "sync")).await.unwrap();
        assert_eq!(response.request_id.0, "x");
        assert_eq!(response.status, RpcStatus::Accepted);
        thread.join().unwrap().unwrap();
    }

    #[tokio::test]
    async fn sync_client_talks_to_async_server() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(echo));

        // The legacy client blocks its caller. Run it on Tokio's blocking pool
        // so the spawned async server can accept and answer the request.
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let mut client = crate::RpcClient::connect(endpoint)?;
                client.call(&request("y", "sync"))
            }),
        )
        .await
        .expect("sync client request timed out")
        .expect("sync client task panicked")
        .expect("sync client request failed");
        assert_eq!(response.request_id.0, "y");
        assert_eq!(response.status, RpcStatus::Accepted);
    }

    #[tokio::test]
    async fn shutdown_fails_all_pending_requests() {
        let server = AsyncRpcServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let endpoint = server.local_addr().unwrap();
        tokio::spawn(server.serve(|req| {
            std::thread::sleep(Duration::from_millis(300));
            echo(req)
        }));

        let client = AsyncRpcClient::connect(endpoint).await.unwrap();
        let handle = {
            let client = client.clone();
            tokio::spawn(async move { client.call(&request("s", "slow")).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        client.shutdown();

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(TransportError::ConnectionClosed)));
    }
}
