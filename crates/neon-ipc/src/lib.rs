//! Loopback transport helpers for length-prefixed JSON RPC.
//! This crate must not create GPU or window objects.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use neon_protocol::{RpcRequest, RpcResponse};

pub const DEFAULT_MAX_FRAME_SIZE: usize = 1024 * 1024;

#[derive(Debug)]
pub enum TransportError {
    FrameTooLarge { size: usize, max: usize },
    InvalidFrameLength,
    Serialization(serde_json::Error),
    RequestIdMismatch,
    ConnectionClosed,
    Timeout,
    Io(io::Error),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { size, max } => {
                write!(formatter, "frame_too_large: {size} exceeds {max}")
            }
            Self::InvalidFrameLength => write!(formatter, "invalid_frame_length"),
            Self::Serialization(error) => write!(formatter, "invalid_json: {error}"),
            Self::RequestIdMismatch => write!(formatter, "request_id_mismatch"),
            Self::ConnectionClosed => write!(formatter, "connection_closed"),
            Self::Timeout => write!(formatter, "timeout"),
            Self::Io(error) => write!(formatter, "transport_io: {error}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<serde_json::Error> for TransportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

pub fn encode_frame(payload: &[u8], max_frame_size: usize) -> Result<Vec<u8>, TransportError> {
    if payload.len() > max_frame_size {
        return Err(TransportError::FrameTooLarge {
            size: payload.len(),
            max: max_frame_size,
        });
    }

    let length = u32::try_from(payload.len()).map_err(|_| TransportError::FrameTooLarge {
        size: payload.len(),
        max: u32::MAX as usize,
    })?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn decode_frames(
    buffer: &mut Vec<u8>,
    max_frame_size: usize,
) -> Result<Vec<Vec<u8>>, TransportError> {
    let mut frames = Vec::new();
    let mut consumed = 0;

    while buffer.len().saturating_sub(consumed) >= 4 {
        let header = &buffer[consumed..consumed + 4];
        let length = u32::from_be_bytes(
            header
                .try_into()
                .map_err(|_| TransportError::InvalidFrameLength)?,
        ) as usize;
        if length > max_frame_size {
            return Err(TransportError::FrameTooLarge {
                size: length,
                max: max_frame_size,
            });
        }
        let frame_end = consumed
            .checked_add(4)
            .and_then(|offset| offset.checked_add(length))
            .ok_or(TransportError::InvalidFrameLength)?;
        if buffer.len() < frame_end {
            break;
        }
        frames.push(buffer[consumed + 4..frame_end].to_vec());
        consumed = frame_end;
    }

    buffer.drain(..consumed);
    Ok(frames)
}

pub struct RpcClient {
    stream: TcpStream,
    max_frame_size: usize,
}

impl RpcClient {
    pub fn connect(endpoint: SocketAddr) -> Result<Self, TransportError> {
        ensure_loopback(endpoint)?;
        let stream = TcpStream::connect(endpoint).map_err(map_io_error)?;
        Ok(Self {
            stream,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        })
    }

    pub fn with_timeout(self, timeout: Duration) -> Result<Self, TransportError> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(map_io_error)?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(map_io_error)?;
        Ok(self)
    }

    pub fn call(&mut self, request: &RpcRequest) -> Result<RpcResponse, TransportError> {
        write_json_frame(&mut self.stream, request, self.max_frame_size)?;
        let response: RpcResponse = read_json_frame(&mut self.stream, self.max_frame_size)?;
        if response.request_id != request.request_id {
            return Err(TransportError::RequestIdMismatch);
        }
        Ok(response)
    }
}

pub struct RpcServer {
    listener: TcpListener,
    max_frame_size: usize,
}

impl RpcServer {
    pub fn bind(endpoint: SocketAddr) -> Result<Self, TransportError> {
        ensure_loopback(endpoint)?;
        let listener = TcpListener::bind(endpoint).map_err(map_io_error)?;
        Ok(Self {
            listener,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.listener.local_addr().map_err(map_io_error)
    }

    pub fn serve_one<F>(&self, handler: F) -> Result<(), TransportError>
    where
        F: FnOnce(RpcRequest) -> RpcResponse,
    {
        let (mut stream, _) = self.listener.accept().map_err(map_io_error)?;
        let request = read_json_frame(&mut stream, self.max_frame_size)?;
        let response = handler(request);
        write_json_frame(&mut stream, &response, self.max_frame_size)
    }
}

fn ensure_loopback(endpoint: SocketAddr) -> Result<(), TransportError> {
    if endpoint.ip().is_loopback() {
        Ok(())
    } else {
        Err(TransportError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "endpoint must bind to loopback",
        )))
    }
}

fn write_json_frame<T: serde::Serialize>(
    stream: &mut TcpStream,
    value: &T,
    max: usize,
) -> Result<(), TransportError> {
    let payload = serde_json::to_vec(value)?;
    let frame = encode_frame(&payload, max)?;
    stream.write_all(&frame).map_err(map_io_error)
}

fn read_json_frame<T: serde::de::DeserializeOwned>(
    stream: &mut TcpStream,
    max: usize,
) -> Result<T, TransportError> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).map_err(map_io_error)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > max {
        return Err(TransportError::FrameTooLarge { size: length, max });
    }
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).map_err(map_io_error)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn map_io_error(error: io::Error) -> TransportError {
    match error.kind() {
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::NotConnected => TransportError::ConnectionClosed,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => TransportError::Timeout,
        _ => TransportError::Io(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{
        ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcStatus, ServiceName,
    };
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr};
    use std::thread;

    fn request(id: &str) -> RpcRequest {
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
            method: "service.health".into(),
            params: json!({}),
            expected_revision: None,
            idempotency_key: None,
        }
    }

    fn accepted(request: RpcRequest) -> RpcResponse {
        RpcResponse {
            request_id: request.request_id,
            status: RpcStatus::Accepted,
            revision: None,
            result: None,
            snapshot: None,
            error: None,
        }
    }

    #[test]
    fn decodes_complete_half_and_joined_frames() {
        let first = encode_frame(b"first", 64).unwrap();
        let second = encode_frame(b"second", 64).unwrap();
        let mut buffer = first[..6].to_vec();
        assert!(decode_frames(&mut buffer, 64).unwrap().is_empty());
        buffer.extend_from_slice(&first[6..]);
        buffer.extend_from_slice(&second);
        assert_eq!(
            decode_frames(&mut buffer, 64).unwrap(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn rejects_oversized_frames() {
        assert!(matches!(
            encode_frame(&[0; 65], 64),
            Err(TransportError::FrameTooLarge { .. })
        ));
        let mut buffer = 65_u32.to_be_bytes().to_vec();
        assert!(matches!(
            decode_frames(&mut buffer, 64),
            Err(TransportError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn loopback_health_request_round_trips() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || server.serve_one(accepted));
        let mut client = RpcClient::connect(endpoint).unwrap();
        assert_eq!(
            client.call(&request("health-001")).unwrap().status,
            RpcStatus::Accepted
        );
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn rejects_mismatched_response_request_id() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || server.serve_one(|_| accepted(request("wrong-id"))));
        let mut client = RpcClient::connect(endpoint).unwrap();
        assert!(matches!(
            client.call(&request("expected-id")),
            Err(TransportError::RequestIdMismatch)
        ));
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn concurrent_requests_do_not_cross_responses() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            server.serve_one(accepted)?;
            server.serve_one(accepted)
        });
        let first = thread::spawn(move || {
            let mut client = RpcClient::connect(endpoint).unwrap();
            client.call(&request("first")).unwrap().request_id.0
        });
        let second_endpoint = endpoint;
        let second = thread::spawn(move || {
            let mut client = RpcClient::connect(second_endpoint).unwrap();
            client.call(&request("second")).unwrap().request_id.0
        });
        let ids = [first.join().unwrap(), second.join().unwrap()];
        assert!(ids.contains(&"first".to_owned()));
        assert!(ids.contains(&"second".to_owned()));
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn rejects_non_loopback_endpoints() {
        let endpoint = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 4000);
        assert!(matches!(
            RpcClient::connect(endpoint),
            Err(TransportError::Io(_))
        ));
        assert!(matches!(
            RpcServer::bind(endpoint),
            Err(TransportError::Io(_))
        ));
    }

    #[test]
    fn reports_connection_close() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            let _stream = server.accept().unwrap();
        });
        let mut client = RpcClient::connect(endpoint).unwrap();
        assert!(matches!(
            client.call(&request("closed")),
            Err(TransportError::ConnectionClosed)
        ));
        thread.join().unwrap();
    }

    #[test]
    fn reports_read_timeout() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            let (_stream, _) = server.accept().unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let mut client = RpcClient::connect(endpoint)
            .unwrap()
            .with_timeout(Duration::from_millis(10))
            .unwrap();
        assert!(matches!(
            client.call(&request("timeout")),
            Err(TransportError::Timeout)
        ));
        thread.join().unwrap();
    }
}
