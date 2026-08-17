//! Structured diagnostics and an in-memory command journal.
//! This crate must not create GPU or window objects.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use neon_protocol::{HealthStatus, RequestId, Revision, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EVENT_COMMAND_RECEIVED: &str = "command.received";
pub const EVENT_COMMAND_VALIDATED: &str = "command.validated";
pub const EVENT_COMMAND_ACCEPTED: &str = "command.accepted";
pub const EVENT_COMMAND_REJECTED: &str = "command.rejected";
pub const EVENT_COMMAND_COMPLETED: &str = "command.completed";
pub const EVENT_COMMAND_FAILED: &str = "command.failed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceRecord {
    pub sequence: u64,
    pub epoch: u64,
    pub timestamp_unix_ms: u64,
    pub service: ServiceName,
    pub level: TraceLevel,
    pub event: String,
    pub request_id: Option<RequestId>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub context_id: Option<String>,
    pub revision_before: Option<Revision>,
    pub revision_after: Option<Revision>,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Received,
    Validated,
    Accepted,
    Rejected,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandReceipt {
    pub request_id: RequestId,
    pub state: CommandState,
    pub revision_before: Option<Revision>,
    pub revision_after: Option<Revision>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugSnapshot {
    pub service: ServiceName,
    pub epoch: u64,
    pub revision: Revision,
    pub health: HealthStatus,
    pub capabilities: Vec<String>,
    pub active_jobs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalFilter {
    pub request_id: Option<RequestId>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub revision: Option<Revision>,
    pub event_id: Option<String>,
    pub pointer_id: Option<u64>,
    pub fragment_revision: Option<Revision>,
    pub composition_revision: Option<Revision>,
}

pub struct CommandJournal {
    service: ServiceName,
    epoch: u64,
    next_sequence: u64,
    capacity: usize,
    records: VecDeque<TraceRecord>,
}

impl CommandJournal {
    pub fn new(service: ServiceName, epoch: u64, capacity: usize) -> Self {
        assert!(capacity > 0, "journal capacity must be greater than zero");
        Self {
            service,
            epoch,
            next_sequence: 1,
            capacity,
            records: VecDeque::with_capacity(capacity),
        }
    }

    pub fn begin_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.next_sequence = 1;
    }

    pub fn append(
        &mut self,
        level: TraceLevel,
        event: impl Into<String>,
        request_id: Option<RequestId>,
        session_id: Option<String>,
        job_id: Option<String>,
        context_id: Option<String>,
        revision_before: Option<Revision>,
        revision_after: Option<Revision>,
        data: Value,
    ) -> TraceRecord {
        let record = TraceRecord {
            sequence: self.next_sequence,
            epoch: self.epoch,
            timestamp_unix_ms: current_unix_ms(),
            service: self.service.clone(),
            level,
            event: event.into(),
            request_id,
            session_id,
            job_id,
            context_id,
            revision_before,
            revision_after,
            data: redact_value(data),
        };
        self.next_sequence += 1;
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record.clone());
        record
    }

    pub fn query(&self, filter: &JournalFilter) -> Vec<TraceRecord> {
        self.records
            .iter()
            .filter(|record| matches_filter(record, filter))
            .cloned()
            .collect()
    }

    pub fn records(&self) -> Vec<TraceRecord> {
        self.records.iter().cloned().collect()
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn matches_filter(record: &TraceRecord, filter: &JournalFilter) -> bool {
    filter
        .request_id
        .as_ref()
        .is_none_or(|value| record.request_id.as_ref() == Some(value))
        && filter
            .session_id
            .as_ref()
            .is_none_or(|value| record.session_id.as_ref() == Some(value))
        && filter
            .job_id
            .as_ref()
            .is_none_or(|value| record.job_id.as_ref() == Some(value))
        && filter.revision.is_none_or(|value| {
            record.revision_before == Some(value) || record.revision_after == Some(value)
        })
        && filter
            .event_id
            .as_ref()
            .is_none_or(|value| record.data.get("event_id").and_then(Value::as_str) == Some(value))
        && filter.pointer_id.is_none_or(|value| {
            record.data.get("pointer_id").and_then(Value::as_u64) == Some(value)
        })
        && filter.fragment_revision.is_none_or(|value| {
            record.data.get("fragment_revision").and_then(Value::as_u64) == Some(value.0)
        })
        && filter.composition_revision.is_none_or(|value| {
            record
                .data
                .get("composition_revision")
                .and_then(Value::as_u64)
                == Some(value.0)
        })
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(&key) {
                        Value::String("[redacted]".into())
                    } else {
                        redact_value(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_value).collect()),
        value => value,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "token",
        "secret",
        "credential",
        "private_key",
        "access_key",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn journal(capacity: usize) -> CommandJournal {
        CommandJournal::new(ServiceName("test-runtime".into()), 4, capacity)
    }

    fn append(journal: &mut CommandJournal, request_id: &str, event: &str) -> TraceRecord {
        journal.append(
            TraceLevel::Info,
            event,
            Some(RequestId(request_id.into())),
            Some("session-1".into()),
            Some("job-1".into()),
            Some("terrain:12".into()),
            Some(Revision(1)),
            Some(Revision(2)),
            json!({"operation": "test"}),
        )
    }

    #[test]
    fn request_lifecycle_is_queryable() {
        let mut journal = journal(8);
        for event in [
            EVENT_COMMAND_RECEIVED,
            EVENT_COMMAND_VALIDATED,
            EVENT_COMMAND_ACCEPTED,
        ] {
            append(&mut journal, "request-1", event);
        }
        let records = journal.query(&JournalFilter {
            request_id: Some(RequestId("request-1".into())),
            ..JournalFilter::default()
        });
        assert_eq!(records.len(), 3);
        assert_eq!(records[2].event, EVENT_COMMAND_ACCEPTED);
    }

    #[test]
    fn sequence_is_monotonic_within_an_epoch() {
        let mut journal = journal(8);
        assert_eq!(
            append(&mut journal, "one", EVENT_COMMAND_RECEIVED).sequence,
            1
        );
        assert_eq!(
            append(&mut journal, "two", EVENT_COMMAND_COMPLETED).sequence,
            2
        );
    }

    #[test]
    fn a_new_epoch_restarts_sequence() {
        let mut journal = journal(8);
        append(&mut journal, "one", EVENT_COMMAND_RECEIVED);
        journal.begin_epoch(5);
        let record = append(&mut journal, "two", EVENT_COMMAND_RECEIVED);
        assert_eq!(record.epoch, 5);
        assert_eq!(record.sequence, 1);
    }

    #[test]
    fn capacity_discards_oldest_records() {
        let mut journal = journal(2);
        append(&mut journal, "one", EVENT_COMMAND_RECEIVED);
        append(&mut journal, "two", EVENT_COMMAND_VALIDATED);
        append(&mut journal, "three", EVENT_COMMAND_COMPLETED);
        let records = journal.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].request_id, Some(RequestId("two".into())));
        assert_eq!(records[1].request_id, Some(RequestId("three".into())));
    }

    #[test]
    fn sensitive_data_is_redacted_before_serialization() {
        let mut journal = journal(8);
        let record = journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_RECEIVED,
            None,
            None,
            None,
            None,
            None,
            None,
            json!({"token": "abc", "nested": {"password": "123"}, "safe": "visible"}),
        );
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("abc"));
        assert!(!encoded.contains("123"));
        assert!(encoded.contains("[redacted]"));
        assert!(encoded.contains("visible"));
    }

    #[test]
    fn filters_support_session_job_and_revision() {
        let mut journal = journal(8);
        append(&mut journal, "request-1", EVENT_COMMAND_RECEIVED);
        let records = journal.query(&JournalFilter {
            session_id: Some("session-1".into()),
            job_id: Some("job-1".into()),
            revision: Some(Revision(2)),
            ..JournalFilter::default()
        });
        assert_eq!(records.len(), 1);
    }
}
