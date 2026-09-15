//! Revisioned, headless document service for the NUI Flow code editor.
//!
//! This crate owns editor document sessions and reliable control-plane state.
//! It does not create windows, WGPU resources, files, or UI layout.
//! WGPU embeds `neon-editor-core` separately for frame-local input and sends
//! bounded ChangeSets here for authoritative revision handling.

use std::collections::HashMap;

use neon_editor_core::grammar::nui_flow_default;
use neon_editor_core::{ChangeSet, EditOp, EditorCore, Position};
use neon_observability::{
    CommandJournal, DebugSnapshot, EVENT_COMMAND_ACCEPTED, EVENT_COMMAND_RECEIVED,
    EVENT_COMMAND_REJECTED, EVENT_COMMAND_VALIDATED, TraceLevel,
};
use neon_protocol::{
    HealthStatus, PROTOCOL_VERSION, RequestId, Revision, RpcError, RpcRequest, RpcResponse,
    RpcStatus, ServiceDescription, ServiceHealth, ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SERVICE_NAME: &str = "editor-runtime";
pub const EDITOR_DOCUMENT_CAPABILITY: &str = "editor.document.v1";
pub const EDITOR_CHANGESET_CAPABILITY: &str = "editor.changeset.v1";
pub const EDITOR_COMPLETION_CAPABILITY: &str = "editor.completion.v1";
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DOCUMENT_LINES: usize = 100_000;
pub const MAX_CHANGESET_OPS: usize = 256;
pub const MAX_INSERT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorChangeKind {
    Draft,
    Commit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSelection {
    pub anchor: Position,
    pub active: Position,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDocumentSnapshot {
    pub document_id: String,
    pub session_id: String,
    pub language: String,
    pub epoch: u64,
    pub revision: Revision,
    pub committed_revision: Revision,
    pub dirty: bool,
    pub line_count: u32,
    pub byte_length: u64,
    pub source_hash: String,
    pub source: String,
    pub diagnostics: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDocumentOpen {
    pub document_id: String,
    pub session_id: String,
    pub language: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorChangeApply {
    pub document_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub change_set: ChangeSet,
    pub kind: EditorChangeKind,
    #[serde(default)]
    pub cursor: Option<Position>,
    #[serde(default)]
    pub selection: Option<EditorSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDocumentRef {
    pub document_id: String,
    pub session_id: String,
    pub epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorOperationResult {
    pub state: String,
    pub snapshot: EditorDocumentSnapshot,
    pub applied_ops: usize,
    pub cursor: Option<Position>,
    pub selection: Option<EditorSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorCompletionRequest {
    pub document_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub document_revision: Revision,
    pub position: Position,
    pub trigger_kind: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorCompletionResult {
    pub document_id: String,
    pub document_revision: Revision,
    pub position: Position,
    pub items: Vec<neon_editor_core::CompletionItem>,
}

struct DocumentRecord {
    session_id: String,
    language: String,
    revision: Revision,
    committed_revision: Revision,
    dirty: bool,
    core: EditorCore,
}

impl DocumentRecord {
    fn snapshot(&self, document_id: &str, epoch: u64) -> EditorDocumentSnapshot {
        let source = self.core.buffer().text();
        EditorDocumentSnapshot {
            document_id: document_id.into(),
            session_id: self.session_id.clone(),
            language: self.language.clone(),
            epoch,
            revision: self.revision,
            committed_revision: self.committed_revision,
            dirty: self.dirty,
            line_count: self.core.buffer().line_count(),
            byte_length: source.len() as u64,
            source_hash: source_hash(&source),
            source,
            diagnostics: Vec::new(),
        }
    }
}

pub struct EditorRuntime {
    epoch: u64,
    documents: HashMap<String, DocumentRecord>,
    idempotent: HashMap<String, RpcResponse>,
    journal: CommandJournal,
}

impl EditorRuntime {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            documents: HashMap::new(),
            idempotent: HashMap::new(),
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 256),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn service_health(&self) -> ServiceHealth {
        ServiceHealth {
            service: ServiceName(SERVICE_NAME.into()),
            status: HealthStatus::Healthy,
            epoch: self.epoch,
        }
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription {
            service: ServiceName(SERVICE_NAME.into()),
            protocol_version: PROTOCOL_VERSION,
            endpoint: "headless://editor-runtime".into(),
            epoch: self.epoch,
            capabilities: vec![
                EDITOR_DOCUMENT_CAPABILITY.into(),
                EDITOR_CHANGESET_CAPABILITY.into(),
                EDITOR_COMPLETION_CAPABILITY.into(),
            ],
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: self.epoch,
            revision: Revision(self.documents.len() as u64),
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: Vec::new(),
        }
    }

    pub fn journal(&self) -> Vec<neon_observability::TraceRecord> {
        self.journal.records()
    }

    /// Handles one public neon3.rpc request. The bool controls the server loop.
    pub fn handle(&mut self, request: RpcRequest) -> (RpcResponse, bool) {
        let request_id = request.request_id.clone();
        let requires_idempotency = matches!(
            request.method.as_str(),
            "editor.document.open"
                | "editor.document.change.apply"
                | "editor.document.change.commit"
                | "editor.document.close"
        );
        let idempotency = request
            .idempotency_key
            .as_ref()
            .map(|key| format!("{}:{key}", request.client.instance_id));
        if let Some(key) = &idempotency
            && let Some(response) = self.idempotent.get(key)
        {
            let mut duplicate = response.clone();
            // Idempotency reuses the accepted result, but every RPC response
            // must still correlate to the current request envelope.
            duplicate.request_id = request_id;
            return (duplicate, request.method != "service.shutdown");
        }

        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_RECEIVED,
            Some(request_id.clone()),
            None,
            None,
            Some(SERVICE_NAME.into()),
            request.expected_revision,
            None,
            json!({"method": request.method, "client": request.client.instance_id}),
        );

        if requires_idempotency && idempotency.is_none() {
            let response = self.reject(
                request_id,
                "invalid_request",
                "mutating editor requests require idempotency_key",
                None,
            );
            return (response, request.method != "service.shutdown");
        }

        let (response, keep_serving) = match request.method.as_str() {
            "service.health" => (
                self.accept(request_id, json!(self.service_health()), None),
                true,
            ),
            "service.describe" => (
                self.accept(request_id, json!(self.service_description()), None),
                true,
            ),
            "debug.snapshot.get" => (
                self.accept(request_id, json!(self.debug_snapshot()), None),
                true,
            ),
            "debug.trace.query" => (self.accept(request_id, json!(self.journal()), None), true),
            "editor.document.open" => (self.open(request_id, request.params), true),
            "editor.document.snapshot.get" => (self.snapshot(request_id, request.params), true),
            "editor.document.change.apply" => (self.apply_change(request_id, request.params), true),
            "editor.document.change.commit" => (
                self.commit(request_id, request.params, request.expected_revision),
                true,
            ),
            "editor.completion.request" => (self.completion(request_id, request.params), true),
            "editor.document.close" => (self.close(request_id, request.params), true),
            "service.shutdown" => (
                self.accept(request_id, json!({"state": "accepted"}), None),
                false,
            ),
            _ => (
                self.reject(
                    request_id,
                    "unsupported_method",
                    "editor method is not supported",
                    None,
                ),
                true,
            ),
        };
        if let Some(key) = idempotency {
            self.idempotent.insert(key, response.clone());
        }
        (response, keep_serving)
    }

    fn open(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let open: EditorDocumentOpen = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(request_id, "editor_open_invalid", &error.to_string(), None);
            }
        };
        if open.document_id.trim().is_empty() || open.session_id.trim().is_empty() {
            return self.reject(
                request_id,
                "editor_open_invalid",
                "document_id and session_id are required",
                None,
            );
        }
        if open.language != "nui_flow" {
            return self.reject(
                request_id,
                "editor_language_unsupported",
                "only nui_flow is supported",
                None,
            );
        }
        if let Some(error) = validate_source(&open.source) {
            return self.reject(request_id, error.0, error.1, None);
        }
        if let Some(record) = self.documents.get(&open.document_id) {
            let snapshot = record.snapshot(&open.document_id, self.epoch);
            return self.accept(
                request_id,
                json!({"state": "already_open", "snapshot": snapshot}),
                Some(record.revision),
            );
        }
        let record = DocumentRecord {
            session_id: open.session_id,
            language: open.language,
            revision: Revision(1),
            committed_revision: Revision(1),
            dirty: false,
            core: EditorCore::new(&open.source, nui_flow_default()),
        };
        let snapshot = record.snapshot(&open.document_id, self.epoch);
        let revision = record.revision;
        self.documents.insert(open.document_id.clone(), record);
        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_VALIDATED,
            Some(request_id.clone()),
            None,
            None,
            Some(open.document_id.clone()),
            None,
            Some(revision),
            json!({"language": "nui_flow", "byte_length": snapshot.byte_length}),
        );
        self.accept(
            request_id,
            json!({"state": "opened", "snapshot": snapshot}),
            Some(revision),
        )
    }

    fn snapshot(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let reference: EditorDocumentRef = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(
                    request_id,
                    "editor_reference_invalid",
                    &error.to_string(),
                    None,
                );
            }
        };
        if reference.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "document reference belongs to a previous editor epoch",
                None,
            );
        }
        let Some(record) = self.documents.get(&reference.document_id) else {
            return self.reject(
                request_id,
                "editor_document_not_found",
                "document is not open",
                None,
            );
        };
        if record.session_id != reference.session_id {
            let revision = record.revision;
            return self.reject(
                request_id,
                "editor_session_mismatch",
                "document session does not match",
                Some(revision),
            );
        }
        let revision = record.revision;
        let snapshot = record.snapshot(&reference.document_id, self.epoch);
        self.accept(
            request_id,
            json!({"state": "ready", "snapshot": snapshot}),
            Some(revision),
        )
    }

    fn apply_change(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let change: EditorChangeApply = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(
                    request_id,
                    "editor_changeset_invalid",
                    &error.to_string(),
                    None,
                );
            }
        };
        if change.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "change belongs to a previous editor epoch",
                None,
            );
        }
        let Some((current_revision, session_matches, language, current_source)) =
            self.documents.get(&change.document_id).map(|record| {
                (
                    record.revision,
                    record.session_id == change.session_id,
                    record.language.clone(),
                    record.core.buffer().text(),
                )
            })
        else {
            return self.reject(
                request_id,
                "editor_document_not_found",
                "document is not open",
                None,
            );
        };
        if !session_matches {
            return self.reject(
                request_id,
                "editor_session_mismatch",
                "document session does not match",
                Some(current_revision),
            );
        }
        if change.change_set.base_revision != current_revision.0 {
            return self.reject(
                request_id,
                "editor_revision_conflict",
                "change base revision is stale",
                Some(current_revision),
            );
        }
        if language != "nui_flow" {
            return self.reject(
                request_id,
                "editor_language_unsupported",
                "document language is unsupported",
                Some(current_revision),
            );
        }
        if change.change_set.ops.is_empty() || change.change_set.ops.len() > MAX_CHANGESET_OPS {
            return self.reject(
                request_id,
                "editor_change_set_invalid",
                "change set must contain 1..256 operations",
                Some(current_revision),
            );
        }
        let mut candidate = EditorCore::new(&current_source, nui_flow_default());
        for operation in &change.change_set.ops {
            match operation {
                EditOp::Insert {
                    line, column, text, ..
                } => {
                    if text.as_bytes().len() > MAX_INSERT_BYTES || text.contains('\0') {
                        return self.reject(
                            request_id,
                            "editor_change_set_overflow",
                            "insert text exceeds the bounded limit",
                            Some(current_revision),
                        );
                    }
                    if !candidate
                        .buffer()
                        .is_valid_position(Position::new(*line, *column))
                    {
                        return self.reject(
                            request_id,
                            "editor_change_set_invalid",
                            "insert position is outside the document",
                            Some(current_revision),
                        );
                    }
                    candidate.insert(Position::new(*line, *column), text);
                }
                EditOp::Delete { start, end, .. } => {
                    if end <= start
                        || !candidate.buffer().is_valid_position(*start)
                        || !candidate.buffer().is_valid_position(*end)
                    {
                        return self.reject(
                            request_id,
                            "editor_change_set_invalid",
                            "delete range must be non-empty",
                            Some(current_revision),
                        );
                    }
                    candidate.delete(*start, *end);
                }
            }
        }
        if let Some(error) = validate_source(&candidate.buffer().text()) {
            return self.reject(request_id, error.0, error.1, Some(current_revision));
        }
        let record = self
            .documents
            .get_mut(&change.document_id)
            .expect("document was validated");
        record.core = candidate;
        record.revision = Revision(record.revision.0.saturating_add(1));
        record.dirty = true;
        if matches!(change.kind, EditorChangeKind::Commit) {
            record.committed_revision = record.revision;
            record.dirty = false;
        }
        let snapshot = record.snapshot(&change.document_id, self.epoch);
        let revision = record.revision;
        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_ACCEPTED,
            Some(request_id.clone()),
            Some(change.session_id.clone()),
            None,
            Some(change.document_id.clone()),
            Some(Revision(change.change_set.base_revision)),
            Some(revision),
            json!({"kind": change.kind, "op_count": change.change_set.ops.len(), "source_hash": snapshot.source_hash}),
        );
        self.accept(
            request_id,
            json!(EditorOperationResult {
                state: if matches!(change.kind, EditorChangeKind::Commit) {
                    "committed".into()
                } else {
                    "draft".into()
                },
                snapshot,
                applied_ops: change.change_set.ops.len(),
                cursor: change.cursor,
                selection: change.selection,
            }),
            Some(revision),
        )
    }

    fn completion(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let completion: EditorCompletionRequest = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(
                    request_id,
                    "editor_completion_invalid",
                    &error.to_string(),
                    None,
                );
            }
        };
        if completion.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "completion belongs to a previous editor epoch",
                None,
            );
        }
        if !matches!(
            completion.trigger_kind.as_str(),
            "automatic" | "invoked" | "trigger_character"
        ) {
            return self.reject(
                request_id,
                "editor_completion_invalid",
                "trigger_kind is invalid",
                None,
            );
        }
        let Some((revision, session_matches, items)) =
            self.documents.get(&completion.document_id).map(|record| {
                (
                    record.revision,
                    record.session_id == completion.session_id,
                    record.core.completions(completion.position),
                )
            })
        else {
            return self.reject(
                request_id,
                "editor_document_not_found",
                "document is not open",
                None,
            );
        };
        if !session_matches {
            return self.reject(
                request_id,
                "editor_session_mismatch",
                "document session does not match",
                Some(revision),
            );
        }
        if completion.document_revision != revision {
            return self.reject(
                request_id,
                "editor_completion_stale",
                "completion request revision is stale",
                Some(revision),
            );
        }
        self.accept(
            request_id,
            json!(EditorCompletionResult {
                document_id: completion.document_id,
                document_revision: revision,
                position: completion.position,
                items,
            }),
            Some(revision),
        )
    }

    fn commit(
        &mut self,
        request_id: RequestId,
        params: Value,
        expected: Option<Revision>,
    ) -> RpcResponse {
        let reference: EditorDocumentRef = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(
                    request_id,
                    "editor_reference_invalid",
                    &error.to_string(),
                    None,
                );
            }
        };
        if reference.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "commit belongs to a previous editor epoch",
                None,
            );
        }
        let Some(current) = self.documents.get(&reference.document_id) else {
            return self.reject(
                request_id,
                "editor_document_not_found",
                "document is not open",
                None,
            );
        };
        if current.session_id != reference.session_id {
            return self.reject(
                request_id,
                "editor_session_mismatch",
                "document session does not match",
                Some(current.revision),
            );
        }
        if expected != Some(current.revision) {
            return self.reject(
                request_id,
                "editor_revision_conflict",
                "commit revision is stale",
                Some(current.revision),
            );
        }
        let record = self
            .documents
            .get_mut(&reference.document_id)
            .expect("document was validated");
        record.committed_revision = record.revision;
        record.dirty = false;
        let snapshot = record.snapshot(&reference.document_id, self.epoch);
        let revision = record.revision;
        self.accept(
            request_id,
            json!({"state": "committed", "snapshot": snapshot}),
            Some(revision),
        )
    }

    fn close(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let reference: EditorDocumentRef = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(
                    request_id,
                    "editor_reference_invalid",
                    &error.to_string(),
                    None,
                );
            }
        };
        if reference.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "close belongs to a previous editor epoch",
                None,
            );
        }
        let Some(current) = self.documents.get(&reference.document_id) else {
            return self.reject(
                request_id,
                "editor_document_not_found",
                "document is not open",
                None,
            );
        };
        if current.session_id != reference.session_id {
            return self.reject(
                request_id,
                "editor_session_mismatch",
                "document session does not match",
                Some(current.revision),
            );
        }
        let revision = current.revision;
        self.documents.remove(&reference.document_id);
        self.accept(
            request_id,
            json!({"state": "closed", "document_id": reference.document_id}),
            Some(revision),
        )
    }

    fn accept(
        &mut self,
        request_id: RequestId,
        result: Value,
        revision: Option<Revision>,
    ) -> RpcResponse {
        RpcResponse {
            request_id,
            status: RpcStatus::Accepted,
            revision,
            result: Some(result),
            snapshot: None,
            error: None,
        }
    }

    fn reject(
        &mut self,
        request_id: RequestId,
        code: &str,
        message: &str,
        revision: Option<Revision>,
    ) -> RpcResponse {
        self.journal.append(
            TraceLevel::Warn,
            EVENT_COMMAND_REJECTED,
            Some(request_id.clone()),
            None,
            None,
            Some(SERVICE_NAME.into()),
            revision,
            revision,
            json!({"code": code}),
        );
        RpcResponse {
            request_id,
            status: RpcStatus::Rejected,
            revision,
            result: None,
            snapshot: None,
            error: Some(RpcError {
                code: code.into(),
                message: message.into(),
                current_revision: revision,
                object_id: None,
            }),
        }
    }
}

fn validate_source(source: &str) -> Option<(&'static str, &'static str)> {
    if source.contains('\0') {
        return Some(("editor_invalid_source", "document contains NUL"));
    }
    if source.len() > MAX_DOCUMENT_BYTES {
        return Some(("editor_document_too_large", "document exceeds 4 MiB"));
    }
    if source.lines().count() > MAX_DOCUMENT_LINES {
        return Some(("editor_document_too_large", "document exceeds 100000 lines"));
    }
    None
}

fn source_hash(source: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

pub fn serve(endpoint: std::net::SocketAddr, epoch: u64) -> Result<(), neon_ipc::TransportError> {
    let server = neon_ipc::RpcServer::bind(endpoint)?;
    let mut runtime = EditorRuntime::new(epoch);
    server.serve_until(|request| runtime.handle(request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion};

    fn request(
        method: &str,
        id: &str,
        params: Value,
        revision: Option<Revision>,
        key: &str,
    ) -> RpcRequest {
        RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId(id.into()),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "editor-test".into(),
                pid: 1,
                origin: "editor-test".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: method.into(),
            params,
            expected_revision: revision,
            idempotency_key: Some(key.into()),
        }
    }

    #[test]
    fn change_revision_conflict_and_idempotency_are_deterministic() {
        let mut runtime = EditorRuntime::new(7);
        let open = request(
            "editor.document.open",
            "open",
            json!(EditorDocumentOpen {
                document_id: "doc".into(),
                session_id: "session".into(),
                language: "nui_flow".into(),
                source: "surface root w 100 h 100".into(),
            }),
            None,
            "open",
        );
        let (response, _) = runtime.handle(open);
        assert_eq!(response.status, RpcStatus::Accepted);
        let apply = request(
            "editor.document.change.apply",
            "apply",
            json!(EditorChangeApply {
                document_id: "doc".into(),
                session_id: "session".into(),
                epoch: 7,
                change_set: ChangeSet {
                    base_revision: 1,
                    ops: vec![EditOp::Insert {
                        line: 0,
                        column: 0,
                        end: Position::new(0, 1),
                        text: "#".into(),
                    }],
                },
                kind: EditorChangeKind::Draft,
                cursor: None,
                selection: None,
            }),
            None,
            "apply",
        );
        let (first, _) = runtime.handle(apply.clone());
        let (duplicate, _) = runtime.handle(apply);
        assert_eq!(first, duplicate);
        assert_eq!(first.revision, Some(Revision(2)));
        let stale = request(
            "editor.document.change.apply",
            "stale",
            json!(EditorChangeApply {
                document_id: "doc".into(),
                session_id: "session".into(),
                epoch: 7,
                change_set: ChangeSet {
                    base_revision: 1,
                    ops: vec![EditOp::Insert {
                        line: 0,
                        column: 0,
                        end: Position::new(0, 1),
                        text: "!".into()
                    }]
                },
                kind: EditorChangeKind::Draft,
                cursor: None,
                selection: None,
            }),
            None,
            "stale",
        );
        let (response, _) = runtime.handle(stale);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("editor_revision_conflict")
        );
    }
}
