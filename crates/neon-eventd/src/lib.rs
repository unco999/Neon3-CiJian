//! Headless event hub for the Neon3 event protocol (`neon3.event`).
//!
//! `neon-eventd` is the single event transport authority: it validates event
//! names against registered namespaces, assigns a global monotonic sequence per
//! epoch, retains a bounded ring buffer for replay, and fans out deliveries to
//! matching subscribers. It never creates GPU/window objects and never holds
//! business truth; it is a notification switch, not an authority.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use neon_ipc::{RpcServer, TransportError};
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_RECEIVED, TraceLevel,
};
use neon_protocol::{
    EVENT_PROTOCOL, EventAck, EventAckStatus, EventDelivery, EventEnvelope, EventError,
    EventFilter, EventFrame, EventId, EventPublish, EventResponse, EventRetention, EventSnapshot,
    EventSubscribe, HealthStatus, PROTOCOL_VERSION, RPC_PROTOCOL, RequestId, Revision, RpcError,
    RpcRequest, RpcResponse, RpcStatus, ServiceDescription, ServiceEvent, ServiceHealth,
    ServiceName,
};
use serde_json::{Value, json};

pub const SERVICE_NAME: &str = "eventd";

pub const DEFAULT_RING_CAPACITY: usize = 4096;
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const RING_CAPACITY_FLOOR: usize = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventStats {
    pub published: u64,
    pub rejected: u64,
    pub delivered: u64,
    pub dropped: u64,
    pub subscriber_connections: u64,
    pub active_subscriptions: u64,
}

struct Subscriber {
    filters: Vec<EventFilter>,
    sender: mpsc::Sender<EventEnvelope>,
}

struct EventdCore {
    epoch: u64,
    next_sequence: u64,
    /// namespace prefix -> schema version
    namespaces: HashMap<String, u16>,
    /// fully-qualified registered names (name -> schema version)
    registered_names: HashMap<String, u16>,
    ring: VecDeque<EventEnvelope>,
    ring_capacity: usize,
    next_subscriber_id: u64,
    subscribers: HashMap<u64, Subscriber>,
    /// (publisher instance_id, idempotency_key) -> (event_id, sequence)
    idempotent: HashMap<(String, String), (EventId, u64)>,
    stats: EventStats,
    journal: CommandJournal,
}

impl EventdCore {
    fn new(epoch: u64, ring_capacity: usize) -> Self {
        assert!(
            ring_capacity >= RING_CAPACITY_FLOOR,
            "ring capacity must be at least {RING_CAPACITY_FLOOR}"
        );
        Self {
            epoch,
            next_sequence: 1,
            namespaces: HashMap::new(),
            registered_names: HashMap::new(),
            ring: VecDeque::with_capacity(ring_capacity),
            ring_capacity,
            next_subscriber_id: 1,
            subscribers: HashMap::new(),
            idempotent: HashMap::new(),
            stats: EventStats::default(),
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, ring_capacity),
        }
    }

    fn register_namespace(&mut self, prefix: &str, schema_version: u16) {
        self.namespaces.insert(prefix.into(), schema_version);
    }

    fn register_name(&mut self, name: &str, schema_version: u16) {
        self.registered_names.insert(name.into(), schema_version);
    }

    fn current_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    fn registered_namespaces(&self) -> Vec<String> {
        let mut names = self.namespaces.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    fn publish(&mut self, publish: EventPublish) -> EventAck {
        let request_id = publish.request_id.clone();
        let _ = self.journal.append(
            TraceLevel::Info,
            "event.publish.received",
            Some(request_id.clone()),
            None,
            None,
            Some(publish.name.clone()),
            None,
            None,
            json!({"name": publish.name}),
        );

        if let Err((code, message)) = self.validate(&publish) {
            self.stats.rejected += 1;
            return self.reject(request_id, &publish, code, message);
        }

        if let Some(key) = &publish.idempotency_key {
            let identity = (publish.publisher.instance_id.clone(), key.clone());
            if let Some((event_id, sequence)) = self.idempotent.get(&identity) {
                self.stats.dropped += 1;
                return EventAck {
                    protocol: publish.protocol.clone(),
                    version: publish.version,
                    request_id,
                    status: EventAckStatus::Accepted,
                    event_id: Some(event_id.clone()),
                    epoch: Some(self.epoch),
                    sequence: Some(*sequence),
                    current_sequence: Some(self.current_sequence()),
                    error: Some(EventError {
                        code: "event_duplicate_ignored".into(),
                        message: "相同 (publisher, idempotency_key) 已接受过".into(),
                        event_id: Some(event_id.clone()),
                        sequence: Some(*sequence),
                    }),
                };
            }
        }

        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let event_id = EventId(format!("evt-{}-{}", self.epoch, sequence));
        let envelope = EventEnvelope {
            protocol: publish.protocol.clone(),
            version: publish.version,
            event_id: event_id.clone(),
            name: publish.name.clone(),
            schema_version: publish.schema_version,
            epoch: self.epoch,
            sequence,
            timestamp_unix_ms: current_unix_ms(),
            publisher: publish.publisher.clone(),
            payload: publish.payload.clone(),
        };

        self.ring.push_back(envelope.clone());
        while self.ring.len() > self.ring_capacity {
            self.ring.pop_front();
        }

        if let Some(key) = &publish.idempotency_key {
            self.idempotent.insert(
                (publish.publisher.instance_id.clone(), key.clone()),
                (event_id.clone(), sequence),
            );
        }

        let mut dead = Vec::new();
        for (subscriber_id, subscriber) in self.subscribers.iter_mut() {
            if matches_any(&subscriber.filters, &envelope) {
                match subscriber.sender.send(envelope.clone()) {
                    Ok(()) => self.stats.delivered += 1,
                    Err(_) => dead.push(*subscriber_id),
                }
            }
        }
        for subscriber_id in dead {
            self.subscribers.remove(&subscriber_id);
            self.stats.active_subscriptions = self.stats.active_subscriptions.saturating_sub(1);
        }

        self.stats.published += 1;
        EventAck {
            protocol: publish.protocol,
            version: publish.version,
            request_id,
            status: EventAckStatus::Accepted,
            event_id: Some(event_id),
            epoch: Some(self.epoch),
            sequence: Some(sequence),
            current_sequence: Some(self.current_sequence()),
            error: None,
        }
    }

    fn validate(&self, publish: &EventPublish) -> Result<(), (&'static str, &'static str)> {
        if publish.payload.to_string().len() > DEFAULT_MAX_PAYLOAD_BYTES {
            return Err((
                "event_payload_too_large",
                "payload exceeds the event payload size limit",
            ));
        }
        if self.is_name_registered(&publish.name) {
            if let Some(expected) = self.schema_version_for(&publish.name) {
                if expected != publish.schema_version {
                    return Err((
                        "event_schema_mismatch",
                        "schema_version does not match the registered schema",
                    ));
                }
            }
            Ok(())
        } else {
            Err(("event_unknown_name", "事件名未注册，严格模式拒绝"))
        }
    }

    fn schema_version_for(&self, name: &str) -> Option<u16> {
        if let Some(version) = self.registered_names.get(name) {
            return Some(*version);
        }
        self.namespaces
            .iter()
            .filter(|(prefix, _)| name.starts_with(prefix.as_str()))
            .map(|(_, version)| *version)
            .next()
    }

    fn is_name_registered(&self, name: &str) -> bool {
        self.registered_names.contains_key(name)
            || self
                .namespaces
                .keys()
                .any(|prefix| name.starts_with(prefix.as_str()))
    }

    fn reject(
        &self,
        request_id: RequestId,
        publish: &EventPublish,
        code: &'static str,
        message: &'static str,
    ) -> EventAck {
        EventAck {
            protocol: publish.protocol.clone(),
            version: publish.version,
            request_id,
            status: EventAckStatus::Rejected,
            event_id: None,
            epoch: Some(self.epoch),
            sequence: None,
            current_sequence: Some(self.current_sequence()),
            error: Some(EventError {
                code: code.into(),
                message: message.into(),
                event_id: None,
                sequence: None,
            }),
        }
    }

    /// Register a subscription. Returns the ack, replayed envelopes, and the
    /// subscriber id used to route the caller-provided sender.
    fn subscribe(
        &mut self,
        subscribe: EventSubscribe,
    ) -> Result<(EventAck, Vec<EventEnvelope>, u64), EventAck> {
        if subscribe.filters.is_empty() {
            return Err(self.reject_subscribe(
                &subscribe,
                "event_subscribe_invalid",
                "过滤器不能为空",
            ));
        }

        let replay = match subscribe.replay_from_sequence {
            Some(from) => {
                let retained_from = self.ring.front().map_or(0, |envelope| envelope.sequence);
                if from < retained_from.saturating_sub(1) {
                    return Err(self.reject_subscribe(
                        &subscribe,
                        "event_replay_unavailable",
                        "请求的 sequence 早于环形缓冲起点",
                    ));
                }
                self.ring
                    .iter()
                    .filter(|envelope| envelope.sequence > from)
                    .filter(|envelope| matches_any(&subscribe.filters, envelope))
                    .cloned()
                    .collect()
            }
            None => Vec::new(),
        };

        let subscriber_id = self.next_subscriber_id;
        self.next_subscriber_id += 1;
        let (sender, _receiver) = mpsc::channel();
        self.subscribers.insert(
            subscriber_id,
            Subscriber {
                filters: subscribe.filters.clone(),
                sender,
            },
        );
        self.stats.subscriber_connections += 1;
        self.stats.active_subscriptions += 1;

        Ok((
            EventAck {
                protocol: subscribe.protocol,
                version: subscribe.version,
                request_id: subscribe.request_id,
                status: EventAckStatus::Accepted,
                event_id: None,
                epoch: Some(self.epoch),
                sequence: None,
                current_sequence: Some(self.current_sequence()),
                error: None,
            },
            replay,
            subscriber_id,
        ))
    }

    fn reject_subscribe(
        &self,
        subscribe: &EventSubscribe,
        code: &'static str,
        message: &'static str,
    ) -> EventAck {
        EventAck {
            protocol: subscribe.protocol.clone(),
            version: subscribe.version,
            request_id: subscribe.request_id.clone(),
            status: EventAckStatus::Rejected,
            event_id: None,
            epoch: Some(self.epoch),
            sequence: None,
            current_sequence: Some(self.current_sequence()),
            error: Some(EventError {
                code: code.into(),
                message: message.into(),
                event_id: None,
                sequence: None,
            }),
        }
    }

    fn replace_subscriber_sender(
        &mut self,
        subscriber_id: u64,
        sender: mpsc::Sender<EventEnvelope>,
    ) {
        if let Some(subscriber) = self.subscribers.get_mut(&subscriber_id) {
            subscriber.sender = sender;
        }
    }

    fn event_snapshot(&self) -> EventSnapshot {
        EventSnapshot {
            epoch: self.epoch,
            current_sequence: self.current_sequence(),
            registered_namespaces: self.registered_namespaces(),
        }
    }

    fn retention(&self) -> EventRetention {
        EventRetention {
            capacity: self.ring_capacity,
            retained: self.ring.len(),
        }
    }
}

/// Cloneable public handle to the event hub.
#[derive(Clone)]
pub struct Eventd {
    core: std::sync::Arc<std::sync::Mutex<EventdCore>>,
    endpoint: String,
    /// Immutable service epoch, copied out of `EventdCore` so that
    /// `service_description`/`debug_snapshot` can read it without re-locking
    /// the core (which would deadlock when called from `handle_rpc`).
    epoch: u64,
}

impl Eventd {
    pub fn new(epoch: u64, ring_capacity: usize) -> Self {
        let mut core = EventdCore::new(epoch, ring_capacity);
        for namespace in [
            "flow.",
            "nui.variable.",
            "nui.intent.",
            "terrain.tool.",
            "terrain.preview.",
            "resource.import.",
            "service.up",
            "service.down",
            "camera.pose.",
            "selection.",
            "ui.file_drop.",
        ] {
            core.register_namespace(namespace, 1);
        }
        Self {
            core: std::sync::Arc::new(std::sync::Mutex::new(core)),
            endpoint: "headless://eventd".into(),
            epoch,
        }
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription {
            service: ServiceName(SERVICE_NAME.into()),
            protocol_version: PROTOCOL_VERSION,
            endpoint: self.endpoint.clone(),
            epoch: self.epoch(),
            capabilities: vec![
                "event.stream.v1".into(),
                "event.replay.v1".into(),
                "event.fastpath.v1".into(),
            ],
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        let core = self.core.lock().expect("eventd core lock");
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: core.epoch,
            revision: Revision(0),
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: core
                .subscribers
                .iter()
                .map(|(id, _)| format!("subscriber-{id}"))
                .collect(),
        }
    }

    pub fn register_namespace(&self, prefix: &str, schema_version: u16) {
        self.core
            .lock()
            .expect("eventd core lock")
            .register_namespace(prefix, schema_version);
    }

    pub fn register_name(&self, name: &str, schema_version: u16) {
        self.core
            .lock()
            .expect("eventd core lock")
            .register_name(name, schema_version);
    }

    pub fn stats(&self) -> EventStats {
        self.core.lock().expect("eventd core lock").stats.clone()
    }

    /// Publish an event. Returns an ack; delivery to subscribers is async.
    pub fn publish(&self, publish: EventPublish) -> EventAck {
        self.core.lock().expect("eventd core lock").publish(publish)
    }

    pub fn snapshot(&self) -> EventSnapshot {
        self.core.lock().expect("eventd core lock").event_snapshot()
    }

    pub fn retention(&self) -> EventRetention {
        self.core.lock().expect("eventd core lock").retention()
    }

    pub fn handle_rpc(&self, request: RpcRequest) -> RpcResponse {
        let request_id = request.request_id.clone();
        let mut guard = self.core.lock().expect("eventd core lock");
        guard.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_RECEIVED,
            Some(request_id.clone()),
            None,
            None,
            None,
            None,
            None,
            json!({"method": request.method}),
        );
        let result = (|| -> Result<Value, (&'static str, &'static str)> {
            match request.method.as_str() {
                "service.health" => Ok(json!(ServiceHealth {
                    service: ServiceName(SERVICE_NAME.into()),
                    status: HealthStatus::Healthy,
                    epoch: guard.epoch,
                })),
                "service.describe" => Ok(json!(self.service_description())),
                "service.shutdown" => Ok(json!({"state": "accepted"})),
                "debug.snapshot.get" => Ok(json!(DebugSnapshot {
                    service: ServiceName(SERVICE_NAME.into()),
                    epoch: guard.epoch,
                    revision: Revision(0),
                    health: HealthStatus::Healthy,
                    capabilities: self.service_description().capabilities,
                    active_jobs: guard
                        .subscribers
                        .iter()
                        .map(|(id, _)| format!("subscriber-{id}"))
                        .collect(),
                })),
                "debug.health.check" => Ok(json!(ServiceHealth {
                    service: ServiceName(SERVICE_NAME.into()),
                    status: HealthStatus::Healthy,
                    epoch: guard.epoch,
                })),
                "debug.diagnostics.get" => Ok(json!({
                    "epoch": guard.epoch,
                    "current_sequence": guard.current_sequence(),
                    "retained": guard.ring.len(),
                    "capacity": guard.ring_capacity,
                    "active_subscriptions": guard.subscribers.len(),
                    "stats": guard.stats,
                })),
                "debug.command.get" => {
                    let id = request
                        .params
                        .get("request_id")
                        .and_then(Value::as_str)
                        .map(|value| RequestId(value.into()));
                    match id {
                        Some(id) => Ok(json!(CommandReceipt {
                            request_id: id.clone(),
                            state: CommandState::Accepted,
                            revision_before: None,
                            revision_after: None,
                            error_code: None,
                        })),
                        None => Err(("invalid_request", "request_id is required")),
                    }
                }
                "debug.trace.query" | "debug.journal.query" => Ok(json!(guard.journal.records())),
                "event.snapshot" => Ok(json!(guard.event_snapshot())),
                "event.schema.register" => {
                    let name = request
                        .params
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_request", "name is required"))?;
                    let schema_version = request
                        .params
                        .get("schema_version")
                        .and_then(Value::as_u64)
                        .unwrap_or(1) as u16;
                    guard.register_name(name, schema_version);
                    Ok(json!({"name": name, "schema_version": schema_version, "registered": true}))
                }
                "event.schema.get" => {
                    let name = request
                        .params
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(("invalid_request", "name is required"))?;
                    guard
                        .registered_names
                        .get(name)
                        .map(|version| json!({"name": name, "schema_version": version}))
                        .ok_or(("event_unknown_name", "事件名未注册"))
                }
                "event.schema.list" => {
                    let mut names = guard
                        .registered_names
                        .iter()
                        .map(|(name, version)| json!({"name": name, "schema_version": version}))
                        .collect::<Vec<_>>();
                    names.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                    Ok(json!(names))
                }
                "event.retention.get" => Ok(json!(guard.retention())),
                "event.retention.set" => {
                    let capacity = request
                        .params
                        .get("capacity")
                        .and_then(Value::as_u64)
                        .ok_or(("invalid_request", "capacity is required"))?
                        as usize;
                    if capacity < RING_CAPACITY_FLOOR {
                        Err(("event_retention_invalid", "capacity must be positive"))
                    } else {
                        guard.ring_capacity = capacity;
                        while guard.ring.len() > capacity {
                            guard.ring.pop_front();
                        }
                        Ok(json!(guard.retention()))
                    }
                }
                "event.stats" => Ok(json!(guard.stats)),
                "event.fastpath.status" => Ok(json!({
                    "available": false,
                    "reason": "fastpath is not enabled in V1; TCP framing is the default transport",
                })),
                "service.subscribe" => Ok(json!(SubscriptionPoll {
                    epoch: guard.epoch,
                    current_sequence: guard.current_sequence(),
                    events: guard
                        .ring
                        .iter()
                        .map(|envelope| ServiceEvent {
                            epoch: envelope.epoch,
                            sequence: envelope.sequence,
                            payload: json!(envelope),
                        })
                        .collect(),
                })),
                _ => Err(("unsupported_method", "method is not supported")),
            }
        })();

        match result {
            Ok(result) => {
                guard.journal.append(
                    TraceLevel::Info,
                    "event.rpc.accepted",
                    Some(request_id.clone()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    json!({"method": request.method}),
                );
                RpcResponse {
                    request_id,
                    status: RpcStatus::Accepted,
                    revision: None,
                    result: Some(result),
                    snapshot: None,
                    error: None,
                }
            }
            Err((code, message)) => {
                guard.journal.append(
                    TraceLevel::Warn,
                    "event.rpc.rejected",
                    Some(request_id.clone()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    json!({"method": request.method, "code": code}),
                );
                RpcResponse {
                    request_id,
                    status: RpcStatus::Rejected,
                    revision: None,
                    result: None,
                    snapshot: None,
                    error: Some(RpcError {
                        code: code.into(),
                        message: message.into(),
                        current_revision: None,
                        object_id: None,
                    }),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscriptionPoll {
    epoch: u64,
    current_sequence: u64,
    events: Vec<ServiceEvent>,
}

fn matches_any(filters: &[EventFilter], envelope: &EventEnvelope) -> bool {
    filters
        .iter()
        .any(|filter| matches_filter(filter, envelope))
}

fn matches_filter(filter: &EventFilter, envelope: &EventEnvelope) -> bool {
    if filter
        .name
        .as_ref()
        .is_some_and(|name| envelope.name == *name)
    {
        return true;
    }
    if filter
        .name_prefix
        .as_ref()
        .is_some_and(|prefix| envelope.name.starts_with(prefix.as_str()))
    {
        return true;
    }
    if let Some(kinds) = &filter.publisher_kinds {
        let kind = serde_json::to_value(&envelope.publisher.kind)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned));
        if kind.as_ref().is_some_and(|kind| kinds.contains(kind)) {
            return true;
        }
    }
    false
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{
        ClientIdentity, ClientKind, EventAckStatus, EventFilter, EventFrame, EventPublish,
        EventSubscribe, RpcStatus,
    };
    use serde_json::json;

    fn publisher() -> ClientIdentity {
        ClientIdentity {
            kind: ClientKind::UiRuntime,
            instance_id: "ui-test".into(),
            pid: 1234,
            origin: "test".into(),
        }
    }

    fn publish(service: &Eventd, id: &str, name: &str) -> EventAck {
        service.publish(EventPublish {
            protocol: "neon3.event".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId(format!("req-{id}")),
            publisher: publisher(),
            name: name.into(),
            schema_version: 1,
            payload: json!({"variable_key": "brush_size", "new_value": 8}),
            idempotency_key: Some(id.into()),
        })
    }

    #[test]
    fn publishes_assign_global_monotonic_sequences() {
        let service = Eventd::new(1, 8);
        let first = publish(&service, "a", "nui.variable.changed");
        let second = publish(&service, "b", "nui.variable.changed");
        assert_eq!(first.status, EventAckStatus::Accepted);
        assert_eq!(first.sequence, Some(1));
        assert_eq!(second.sequence, Some(2));
        assert!(first.event_id != second.event_id);
        let snapshot = service.snapshot();
        assert_eq!(snapshot.current_sequence, 2);
        let stats = service.stats();
        assert_eq!(stats.published, 2);
    }

    #[test]
    fn strict_mode_rejects_unregistered_event_names() {
        let service = Eventd::new(1, 8);
        let ack = service.publish(EventPublish {
            protocol: "neon3.event".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-unknown".into()),
            publisher: publisher(),
            name: "unknown.namespace.event".into(),
            schema_version: 1,
            payload: json!({}),
            idempotency_key: None,
        });
        assert_eq!(ack.status, EventAckStatus::Rejected);
        assert_eq!(ack.error.unwrap().code, "event_unknown_name");
        assert_eq!(service.stats().rejected, 1);
    }

    #[test]
    fn schema_mismatch_is_rejected() {
        let service = Eventd::new(1, 8);
        let ack = service.publish(EventPublish {
            protocol: "neon3.event".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-schema".into()),
            publisher: publisher(),
            name: "nui.variable.changed".into(),
            schema_version: 2,
            payload: json!({}),
            idempotency_key: None,
        });
        assert_eq!(ack.error.unwrap().code, "event_schema_mismatch");
    }

    #[test]
    fn idempotency_key_returns_the_original_event() {
        let service = Eventd::new(1, 8);
        let first = publish(&service, "same-key", "nui.variable.changed");
        let second = publish(&service, "same-key", "nui.variable.changed");
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.sequence, second.sequence);
        assert_eq!(service.stats().published, 1);
        assert_eq!(service.stats().dropped, 1);
    }

    #[test]
    fn subscription_receives_matching_live_events() {
        let service = Eventd::new(1, 8);
        let (ack, live, _subscriber_id) = {
            let mut core = service.core.lock().unwrap();
            core.subscribe(EventSubscribe {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("sub-1".into()),
                client: publisher(),
                filters: vec![EventFilter {
                    name: None,
                    name_prefix: Some("nui.variable.".into()),
                    publisher_kinds: None,
                }],
                replay_from_sequence: None,
                max_rate_hz: None,
            })
            .map(|(ack, _replay, id)| {
                let (sender, receiver) = mpsc::channel();
                core.replace_subscriber_sender(id, sender);
                (ack, receiver, id)
            })
            .unwrap()
        };
        assert_eq!(ack.status, EventAckStatus::Accepted);
        let publish_ack = publish(&service, "live", "nui.variable.changed");
        let delivered = live.recv().unwrap();
        assert_eq!(delivered.event_id, publish_ack.event_id.unwrap());
        assert_eq!(delivered.name, "nui.variable.changed");
    }

    #[test]
    fn subscription_filter_misses_non_matching_events() {
        let service = Eventd::new(1, 8);
        let live = {
            let mut core = service.core.lock().unwrap();
            core.subscribe(EventSubscribe {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("sub-2".into()),
                client: publisher(),
                filters: vec![EventFilter {
                    name: Some("project.opened".into()),
                    name_prefix: None,
                    publisher_kinds: None,
                }],
                replay_from_sequence: None,
                max_rate_hz: None,
            })
            .map(|(_ack, _replay, id)| {
                let (sender, receiver) = mpsc::channel();
                core.replace_subscriber_sender(id, sender);
                receiver
            })
            .unwrap()
        };
        publish(&service, "miss", "nui.variable.changed");
        assert!(
            live.recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
    }

    #[test]
    fn replay_delivers_retained_events_in_order() {
        let service = Eventd::new(1, 8);
        publish(&service, "one", "nui.variable.changed");
        publish(&service, "two", "nui.variable.changed");
        let (ack, replay, _id) = {
            let mut core = service.core.lock().unwrap();
            core.subscribe(EventSubscribe {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("sub-3".into()),
                client: publisher(),
                filters: vec![EventFilter {
                    name: None,
                    name_prefix: Some("nui.variable.".into()),
                    publisher_kinds: None,
                }],
                replay_from_sequence: Some(1),
                max_rate_hz: None,
            })
            .unwrap()
        };
        assert_eq!(ack.status, EventAckStatus::Accepted);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].sequence, 2);
    }

    #[test]
    fn replay_older_than_ring_start_is_unavailable() {
        let service = Eventd::new(1, 2);
        publish(&service, "one", "nui.variable.changed");
        publish(&service, "two", "nui.variable.changed");
        publish(&service, "three", "nui.variable.changed");
        let result = {
            let mut core = service.core.lock().unwrap();
            core.subscribe(EventSubscribe {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("sub-4".into()),
                client: publisher(),
                filters: vec![EventFilter {
                    name: None,
                    name_prefix: Some("nui.variable.".into()),
                    publisher_kinds: None,
                }],
                replay_from_sequence: Some(0),
                max_rate_hz: None,
            })
        };
        assert_eq!(
            result.unwrap_err().error.unwrap().code,
            "event_replay_unavailable"
        );
    }

    #[test]
    fn rpc_control_plane_exposes_snapshot_schema_and_stats() {
        let service = Eventd::new(7, 8);
        let rpc = |method: &str, params: Value| {
            service.handle_rpc(RpcRequest {
                protocol: "neon3.rpc".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId(format!("rpc-{method}")),
                client: publisher(),
                target: ServiceName(SERVICE_NAME.into()),
                method: method.into(),
                params,
                expected_revision: None,
                idempotency_key: None,
            })
        };
        let health = rpc("service.health", json!({}));
        assert_eq!(health.status, RpcStatus::Accepted);
        assert_eq!(health.result.unwrap()["epoch"], 7);

        let snapshot = rpc("event.snapshot", json!({}));
        assert_eq!(snapshot.result.unwrap()["current_sequence"], 0);

        let registered = rpc(
            "event.schema.register",
            json!({"name": "project.opened", "schema_version": 1}),
        );
        assert_eq!(registered.status, RpcStatus::Accepted);

        let list = rpc("event.schema.list", json!({}));
        assert!(
            list.result
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"] == "project.opened")
        );

        publish(&service, "rpc-pub", "project.opened");
        let diagnostics = rpc("debug.diagnostics.get", json!({}));
        assert_eq!(diagnostics.result.as_ref().unwrap()["current_sequence"], 1);
        assert_eq!(
            diagnostics.result.as_ref().unwrap()["stats"]["published"],
            1
        );
    }

    #[test]
    fn rpc_rejects_unknown_method_with_stable_code() {
        let service = Eventd::new(1, 8);
        let response = service.handle_rpc(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("rpc-bad".into()),
            client: publisher(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "terrain.tool.select".into(),
            params: json!({}),
            expected_revision: None,
            idempotency_key: None,
        });
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "unsupported_method");
    }

    #[test]
    fn loopback_event_stream_publishes_and_delivers() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        let _server = std::thread::spawn(move || serve(endpoint, 1));

        // Wait for the server to bind.
        let mut client = loop {
            match neon_ipc::EventClient::connect(endpoint) {
                Ok(client) => break client,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        let ack = client
            .publish(&EventPublish {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("tcp-pub".into()),
                publisher: publisher(),
                name: "nui.variable.changed".into(),
                schema_version: 1,
                payload: json!({"new_value": 8}),
                idempotency_key: None,
            })
            .unwrap();
        assert_eq!(ack.status, EventAckStatus::Accepted);

        // Subscribe on a fresh connection and receive the live event.
        let mut subscriber = neon_ipc::EventClient::connect(endpoint).unwrap();
        subscriber
            .send_value(
                &serde_json::to_value(EventFrame::Subscribe(EventSubscribe {
                    protocol: "neon3.event".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("tcp-sub".into()),
                    client: publisher(),
                    filters: vec![EventFilter {
                        name: None,
                        name_prefix: Some("nui.variable.".into()),
                        publisher_kinds: None,
                    }],
                    replay_from_sequence: None,
                    max_rate_hz: None,
                }))
                .unwrap(),
            )
            .unwrap();
        let response: EventResponse =
            serde_json::from_value(subscriber.recv_value().unwrap()).unwrap();
        match response {
            EventResponse::Ack(ack) => assert_eq!(ack.status, EventAckStatus::Accepted),
            _ => panic!("expected subscribe ack"),
        }

        let mut publisher_client = neon_ipc::EventClient::connect(endpoint).unwrap();
        publisher_client
            .publish(&EventPublish {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId("tcp-live".into()),
                publisher: publisher(),
                name: "nui.variable.changed".into(),
                schema_version: 1,
                payload: json!({"new_value": 12}),
                idempotency_key: None,
            })
            .unwrap();
        let delivery: EventResponse =
            serde_json::from_value(subscriber.recv_value().unwrap()).unwrap();
        match delivery {
            EventResponse::Delivery(delivery) => {
                assert_eq!(delivery.event.name, "nui.variable.changed");
                assert_eq!(delivery.event.payload["new_value"], 12);
            }
            _ => panic!("expected delivery"),
        }
        drop(subscriber);
        drop(publisher_client);
        // `serve` runs an accept loop forever; it outlives this test. The
        // server thread exits when the test harness terminates the process.
    }
}

/// Serve the event hub on one loopback endpoint. Each connection is routed by
/// the frame `protocol` field: `neon3.rpc` goes to the control plane and
/// `neon3.event` to the event plane. Event connections are persistent streams
/// driven until the client closes or sends `unsubscribe`.
pub fn serve(endpoint: SocketAddr, epoch: u64) -> Result<(), TransportError> {
    let server = RpcServer::bind(endpoint)?;
    let service = Eventd::new(epoch, DEFAULT_RING_CAPACITY)
        .with_endpoint(format!("tcp://{}", server.local_addr()?));
    loop {
        let stream = server.accept()?;
        let service = service.clone();
        std::thread::spawn(move || {
            let _ = handle_connection(&service, stream);
        });
    }
}

/// A frame written to a connected client. RPC control-plane responses and
/// event-plane responses share one socket writer.
enum Outbound {
    Rpc(RpcResponse),
    Event(EventResponse),
}

fn handle_connection(service: &Eventd, stream: std::net::TcpStream) -> Result<(), TransportError> {
    use std::io::{BufReader, BufWriter, Write};
    let reader_stream = stream.try_clone().map_err(TransportError::Io)?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);

    // The writer channel carries both the RPC control-plane responses and the
    // event-plane responses so a single writer thread owns the socket.
    let (write_tx, write_rx) = mpsc::channel::<Outbound>();
    let writer_thread = std::thread::spawn(move || {
        while let Ok(outbound) = write_rx.recv() {
            let written = match outbound {
                Outbound::Rpc(response) => neon_ipc::write_json_frame(
                    &mut writer,
                    &response,
                    neon_ipc::DEFAULT_MAX_FRAME_SIZE,
                ),
                Outbound::Event(response) => neon_ipc::write_json_frame(
                    &mut writer,
                    &response,
                    neon_ipc::DEFAULT_MAX_FRAME_SIZE,
                ),
            };
            if written.is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    // The active subscription belongs to this connection. On unsubscribe or
    // disconnect it must be removed so its forwarder thread can exit and the
    // shared writer channel can close.
    let mut active_subscriber_id: Option<u64> = None;

    loop {
        // Read the raw frame and dispatch by protocol field: `neon3.rpc` goes
        // to the control plane, `neon3.event` to the event plane.
        let value: Value =
            match neon_ipc::read_json_frame(&mut reader, neon_ipc::DEFAULT_MAX_FRAME_SIZE) {
                Ok(value) => value,
                Err(TransportError::ConnectionClosed) | Err(TransportError::Timeout) => break,
                Err(error) => return Err(error),
            };
        match value.get("protocol").and_then(Value::as_str) {
            Some(RPC_PROTOCOL) => {
                let request = match serde_json::from_value::<RpcRequest>(value) {
                    Ok(request) => request,
                    Err(_) => continue,
                };
                let response = service.handle_rpc(request);
                if write_tx.send(Outbound::Rpc(response)).is_err() {
                    break;
                }
            }
            Some(EVENT_PROTOCOL) => {
                let frame: EventFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(_) => continue,
                };
                dispatch_event_frame(service, frame, &write_tx, &mut active_subscriber_id)?;
            }
            _ => {
                // Unknown protocol; close the connection rather than looping.
                break;
            }
        }
    }
    cleanup(service, write_tx, writer_thread, active_subscriber_id)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_event_frame(
    service: &Eventd,
    frame: EventFrame,
    write_tx: &mpsc::Sender<Outbound>,
    active_subscriber_id: &mut Option<u64>,
) -> Result<(), TransportError> {
    match frame {
        EventFrame::Publish(publish) => {
            let ack = service.publish(publish);
            if write_tx
                .send(Outbound::Event(EventResponse::Ack(ack)))
                .is_err()
            {
                return Err(TransportError::ConnectionClosed);
            }
        }
        EventFrame::Subscribe(subscribe) => {
            // Perform the subscription registration under the lock, then
            // release it before sending frames or returning through cleanup.
            let outcome: (
                EventAck,
                Vec<EventEnvelope>,
                Option<(u64, mpsc::Receiver<EventEnvelope>)>,
            ) = {
                let mut core = service.core.lock().expect("eventd core lock");
                match core.subscribe(subscribe) {
                    Ok((ack, replay, subscriber_id)) => {
                        let (sender, receiver) = mpsc::channel();
                        core.replace_subscriber_sender(subscriber_id, sender);
                        *active_subscriber_id = Some(subscriber_id);
                        (ack, replay, Some((subscriber_id, receiver)))
                    }
                    Err(ack) => (ack, Vec::new(), None),
                }
            };
            let (ack, replay, live) = outcome;
            // Ack first, then replay, then live events.
            if write_tx
                .send(Outbound::Event(EventResponse::Ack(ack)))
                .is_err()
            {
                return Err(TransportError::ConnectionClosed);
            }
            for envelope in replay {
                if write_tx
                    .send(Outbound::Event(EventResponse::Delivery(EventDelivery {
                        protocol: envelope.protocol.clone(),
                        version: envelope.version,
                        event: envelope,
                    })))
                    .is_err()
                {
                    return Err(TransportError::ConnectionClosed);
                }
            }
            if let Some((_subscriber_id, receiver)) = live {
                // A dedicated forwarder drains the subscription receiver into
                // the shared writer channel so live events are pushed async.
                let write_tx = write_tx.clone();
                std::thread::spawn(move || {
                    while let Ok(envelope) = receiver.recv() {
                        if write_tx
                            .send(Outbound::Event(EventResponse::Delivery(EventDelivery {
                                protocol: envelope.protocol.clone(),
                                version: envelope.version,
                                event: envelope,
                            })))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        }
        EventFrame::Unsubscribe => {
            return Err(TransportError::ConnectionClosed);
        }
        EventFrame::Heartbeat => {
            let snapshot = service.snapshot();
            if write_tx
                .send(Outbound::Event(EventResponse::Ack(EventAck {
                    protocol: "neon3.event".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("heartbeat".into()),
                    status: EventAckStatus::Accepted,
                    event_id: None,
                    epoch: Some(snapshot.epoch),
                    sequence: Some(snapshot.current_sequence),
                    current_sequence: Some(snapshot.current_sequence),
                    error: None,
                })))
                .is_err()
            {
                return Err(TransportError::ConnectionClosed);
            }
        }
    }
    Ok(())
}

fn cleanup(
    service: &Eventd,
    write_tx: mpsc::Sender<Outbound>,
    writer_thread: std::thread::JoinHandle<()>,
    active_subscriber_id: Option<u64>,
) -> Result<(), TransportError> {
    if let Some(subscriber_id) = active_subscriber_id {
        let mut core = service.core.lock().expect("eventd core lock");
        if core.subscribers.remove(&subscriber_id).is_some() {
            core.stats.active_subscriptions = core.stats.active_subscriptions.saturating_sub(1);
        }
        drop(core);
    }
    // Dropping our sender lets the forwarder thread observe a closed receiver
    // only if the subscriber is gone; after removal the receiver closes and the
    // forwarder exits, releasing its clone of write_tx. Then the writer thread
    // sees the channel close and terminates.
    drop(write_tx);
    let _ = writer_thread.join();
    Ok(())
}
