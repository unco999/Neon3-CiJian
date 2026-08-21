//! Event publisher for UI variable changes declared with `emitevent`.
//!
//! `neon-ui-runtime` owns UI declaration and input semantics. When a UI input
//! variable that declared `emitevent` changes, the change is an *observation*
//! forwarded to `neon-eventd` as a directed `flow.<flow_name>.<variable_key>`
//! event on the dedicated `neon3.event` protocol. Undeclared variables stay
//! silent: the publisher never broadcasts every input mutation.
//!
//! Publishing is best-effort and non-authoritative: a variable change event is
//! not a domain command, carries no renderer identity, and never authorizes a
//! receiver to mutate authoritative state. Delivery failures are reported but
//! do not roll back the already-applied UI input frame.

use std::net::SocketAddr;

use neon_ipc::EventClient;
use neon_protocol::{
    ClientIdentity, EventAck, EventAckStatus, EventPublish, PROTOCOL_VERSION, RequestId,
};
use serde_json::json;

use crate::UiVariableChange;

/// Legacy generic event name. Retained for compatibility; directed `emitevent`
/// declarations use `flow.<flow_name>.<variable_key>` instead.
pub const EVENT_VARIABLE_CHANGED: &str = "nui.variable.changed";

/// Event name prefix for directed `emitevent` declarations.
pub const FLOW_EVENT_PREFIX: &str = "flow.";

#[derive(Clone, Debug)]
pub struct UiVariableEventPublisher {
    eventd_endpoint: Option<SocketAddr>,
    client: ClientIdentity,
    module: String,
    surface: String,
    flow_name: String,
    emit_event_keys: std::collections::BTreeSet<String>,
}

impl UiVariableEventPublisher {
    /// `endpoint` is the `neon-eventd` loopback endpoint. Pass `None` to run
    /// without event forwarding (headless tests, default demo domains).
    /// `flow_name` and `emit_event_keys` come from the Flow declaration:
    /// only variables that declared `emitevent` produce directed events.
    pub fn new(
        endpoint: Option<SocketAddr>,
        client: ClientIdentity,
        module: impl Into<String>,
        surface: impl Into<String>,
        flow_name: impl Into<String>,
        emit_event_keys: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            eventd_endpoint: endpoint,
            client,
            module: module.into(),
            surface: surface.into(),
            flow_name: flow_name.into(),
            emit_event_keys: emit_event_keys.into_iter().map(Into::into).collect(),
        }
    }

    pub fn disabled(module: impl Into<String>, surface: impl Into<String>) -> Self {
        Self {
            eventd_endpoint: None,
            client: ClientIdentity {
                kind: neon_protocol::ClientKind::UiRuntime,
                instance_id: "ui-runtime".into(),
                pid: std::process::id(),
                origin: "neon-ui-runtime".into(),
            },
            module: module.into(),
            surface: surface.into(),
            flow_name: String::new(),
            emit_event_keys: std::collections::BTreeSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.eventd_endpoint.is_some()
    }

    /// Event name for a variable, or `None` when the variable did not declare
    /// `emitevent` or the Flow document has no `flow <name>` declaration.
    pub fn event_name_for(&self, key: &str) -> Option<String> {
        if self.emit_event_keys.contains(key) && !self.flow_name.is_empty() {
            Some(format!("{FLOW_EVENT_PREFIX}{}.{}", self.flow_name, key))
        } else {
            None
        }
    }

    /// Publishes directed `flow.<flow_name>.<variable_key>` events for changes
    /// whose variable declared `emitevent`. Each publish is a separate loopback
    /// connection; failures are collected but never roll back the applied frame.
    pub fn publish_variable_changes(
        &self,
        changes: &[UiVariableChange],
    ) -> Vec<Result<EventAck, String>> {
        let Some(endpoint) = self.eventd_endpoint else {
            return Vec::new();
        };
        let mut results = Vec::with_capacity(changes.len());
        for change in changes {
            let Some(event_name) = self.event_name_for(&change.key) else {
                continue;
            };
            results.push(self.publish_one(endpoint, change, &event_name));
        }
        results
    }

    fn publish_one(
        &self,
        endpoint: SocketAddr,
        change: &UiVariableChange,
        event_name: &str,
    ) -> Result<EventAck, String> {
        let mut client = EventClient::connect(endpoint).map_err(|error| error.to_string())?;
        let publish = EventPublish {
            protocol: "neon3.event".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId(format!("ui-var-{}-{}", self.client.instance_id, change.key)),
            publisher: self.client.clone(),
            name: event_name.into(),
            schema_version: 1,
            payload: json!({
                "module": self.module,
                "surface": self.surface,
                "variable_key": change.key,
                "kind": change.kind,
                "old_value": change.old_value,
                "new_value": change.new_value,
            }),
            idempotency_key: None,
        };
        let ack = client
            .publish(&publish)
            .map_err(|error| error.to_string())?;
        if ack.status == EventAckStatus::Accepted {
            Ok(ack)
        } else {
            Err(ack
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "event publish rejected".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::ClientKind;

    fn change(key: &str, kind: &str, new_value: serde_json::Value) -> UiVariableChange {
        UiVariableChange {
            key: key.into(),
            kind: kind.into(),
            old_value: None,
            new_value,
        }
    }

    #[test]
    fn disabled_publisher_returns_no_results() {
        let publisher = UiVariableEventPublisher::disabled("module", "surface");
        assert!(!publisher.enabled());
        assert!(
            publisher
                .publish_variable_changes(&[change("brush_size", "i32", json!({"value": 8}))])
                .is_empty()
        );
    }

    #[test]
    fn event_name_is_flow_prefixed_and_filtered_by_emitevent() {
        let publisher = UiVariableEventPublisher::new(
            None,
            ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-test".into(),
                pid: 1,
                origin: "test".into(),
            },
            "module",
            "surface",
            "terrain-workbench",
            ["brush_size", "can_commit"],
        );
        assert_eq!(
            publisher.event_name_for("brush_size"),
            Some("flow.terrain-workbench.brush_size".into())
        );
        assert_eq!(publisher.event_name_for("unrelated"), None);
        assert_eq!(
            publisher.event_name_for("can_commit"),
            Some("flow.terrain-workbench.can_commit".into())
        );
    }

    #[test]
    fn event_name_requires_a_flow_declaration() {
        let publisher = UiVariableEventPublisher::new(
            None,
            ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-test".into(),
                pid: 1,
                origin: "test".into(),
            },
            "module",
            "surface",
            "",
            ["brush_size"],
        );
        assert_eq!(publisher.event_name_for("brush_size"), None);
    }

    #[test]
    fn enabled_publisher_fails_cleanly_when_eventd_is_down() {
        // Reserve a loopback port then release it so no listener exists.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        let publisher = UiVariableEventPublisher::new(
            Some(endpoint),
            ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-test".into(),
                pid: 1,
                origin: "test".into(),
            },
            "module",
            "surface",
            "terrain-workbench",
            ["brush_size"],
        );
        let results =
            publisher.publish_variable_changes(&[change("brush_size", "i32", json!({"value": 8}))]);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    #[test]
    fn undeclared_changes_are_not_published() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        let publisher = UiVariableEventPublisher::new(
            Some(endpoint),
            ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-test".into(),
                pid: 1,
                origin: "test".into(),
            },
            "module",
            "surface",
            "terrain-workbench",
            ["brush_size"],
        );
        let results = publisher.publish_variable_changes(&[
            change("brush_size", "i32", json!({"value": 8})),
            change("unrelated", "bool", json!({"value": true})),
        ]);
        // Only the declared variable is attempted; the undeclared one is skipped.
        assert_eq!(results.len(), 1);
    }
}
