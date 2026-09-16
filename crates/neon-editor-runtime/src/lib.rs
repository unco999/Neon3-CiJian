//! Revisioned, headless document service for the NUI Flow code editor.
//!
//! This crate owns editor document sessions and reliable control-plane state.
//! It does not create windows, WGPU resources, files, or UI layout.
//! WGPU embeds `neon-editor-core` separately for frame-local input and sends
//! bounded ChangeSets here for authoritative revision handling.

use std::collections::HashMap;

use neon_editor::languages::LanguageKind;
use neon_editor::{
    ChangeSet, EditOp, EditorCore, Language, LspClient, LspDiagnostic, LspEndpoint, LspLocation,
    LspSymbol, Position,
};
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
pub const EDITOR_LANGUAGE_CAPABILITY: &str = "editor.language.v1";
pub const EDITOR_LSP_CAPABILITY: &str = "editor.lsp.v1";
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
    pub items: Vec<neon_editor::CompletionItem>,
}

/// Position inside a document for LSP introspection requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorLspPosition {
    pub document_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub document_revision: Revision,
    pub position: Position,
}

/// Document reference for LSP requests that do not need a cursor position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorLspRef {
    pub document_id: String,
    pub session_id: String,
    pub epoch: u64,
    pub document_revision: Revision,
}

/// Result of `editor.lsp.diagnostics`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorLspDiagnosticsResult {
    pub document_id: String,
    pub document_revision: Revision,
    pub diagnostics: Vec<LspDiagnostic>,
    /// Present when the language server could not be started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_unavailable: Option<String>,
}

/// Result of `editor.lsp.definition` / `editor.lsp.references`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorLspLocationsResult {
    pub document_id: String,
    pub document_revision: Revision,
    pub locations: Vec<LspLocation>,
}

/// Result of `editor.lsp.symbols`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorLspSymbolsResult {
    pub document_id: String,
    pub document_revision: Revision,
    pub symbols: Vec<LspSymbol>,
}

/// Result of `editor.lsp.hover` / `editor.lsp.signature_help` — the raw LSP
/// payload, plus the server availability state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorLspRawResult {
    pub document_id: String,
    pub document_revision: Revision,
    pub result: Value,
}

struct DocumentRecord {
    session_id: String,
    language: String,
    language_kind: LanguageKind,
    revision: Revision,
    committed_revision: Revision,
    dirty: bool,
    core: EditorCore,
    /// Live LSP server for non-Flow documents (spawned on open when the
    /// language server binary is available).
    lsp: Option<LspClient>,
    /// LSP document URI (`file:///neon3/{document_id}.{ext}`).
    lsp_uri: String,
    /// Set when the language server could not be started (missing binary).
    lsp_unavailable: Option<String>,
}

impl DocumentRecord {
    fn snapshot(&self, document_id: &str, epoch: u64) -> EditorDocumentSnapshot {
        let source = self.core.buffer().text();
        let diagnostics = match &self.lsp {
            Some(lsp) => lsp.diagnostics(&self.lsp_uri),
            None => Vec::new(),
        };
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
            diagnostics: diagnostics
                .into_iter()
                .map(|diagnostic| serde_json::to_value(diagnostic).unwrap_or(Value::Null))
                .collect(),
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
                EDITOR_LANGUAGE_CAPABILITY.into(),
                EDITOR_LSP_CAPABILITY.into(),
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
            "editor.lsp.diagnostics" => (self.lsp_diagnostics(request_id, request.params), true),
            "editor.lsp.hover" => (self.lsp_hover(request_id, request.params), true),
            "editor.lsp.definition" => (self.lsp_definition(request_id, request.params), true),
            "editor.lsp.references" => (self.lsp_references(request_id, request.params), true),
            "editor.lsp.symbols" => (self.lsp_symbols(request_id, request.params), true),
            "editor.lsp.signature_help" => (self.lsp_signature_help(request_id, request.params), true),
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
        let Some(language_kind) = language_from_name(&open.language) else {
            return self.reject(
                request_id,
                "editor_language_unsupported",
                "supported languages: nui_flow, typescript, rust, cpp",
                None,
            );
        };
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
        let language = Language { kind: language_kind };
        let lsp_uri = lsp_uri_for(&open.document_id, language_kind);
        let (lsp, lsp_unavailable) = match language_kind {
            LanguageKind::NuiFlow => (None, None),
            _ => spawn_lsp(language_kind, &lsp_uri, &open.source),
        };
        let record = DocumentRecord {
            session_id: open.session_id,
            language: open.language,
            language_kind,
            revision: Revision(1),
            committed_revision: Revision(1),
            dirty: false,
            core: EditorCore::from_language(&open.source, language),
            lsp,
            lsp_uri,
            lsp_unavailable,
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
            json!({"language": language_kind.name(), "byte_length": snapshot.byte_length}),
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
        let Some((current_revision, session_matches, language_kind, current_source)) =
            self.documents.get(&change.document_id).map(|record| {
                (
                    record.revision,
                    record.session_id == change.session_id,
                    record.language_kind,
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
        if change.change_set.ops.is_empty() || change.change_set.ops.len() > MAX_CHANGESET_OPS {
            return self.reject(
                request_id,
                "editor_change_set_invalid",
                "change set must contain 1..256 operations",
                Some(current_revision),
            );
        }
        let mut candidate =
            EditorCore::from_language(&current_source, Language { kind: language_kind });
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
        // Keep the language server in sync with the authoritative buffer.
        if let Some(lsp) = &mut record.lsp {
            let uri = record.lsp_uri.clone();
            let text = record.core.buffer().text();
            let version = record.revision.0 as i64;
            if let Err(error) = lsp.change_document(&uri, version, &text) {
                // LSP is best-effort: a dead server never fails the edit.
                record.lsp = None;
                record.lsp_unavailable = Some(format!("lsp server closed: {error}"));
            }
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
        let Some((revision, session_matches, language_kind)) =
            self.documents.get(&completion.document_id).map(|record| {
                (
                    record.revision,
                    record.session_id == completion.session_id,
                    record.language_kind,
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
        let items = match language_kind {
            LanguageKind::NuiFlow => self
                .documents
                .get(&completion.document_id)
                .map(|record| record.core.completions(completion.position))
                .unwrap_or_default(),
            _ => {
                let record = self
                    .documents
                    .get_mut(&completion.document_id)
                    .expect("document was validated");
                match &mut record.lsp {
                    Some(lsp) => {
                        let uri = record.lsp_uri.clone();
                        match lsp.request_completion(&uri, completion.position) {
                            Ok(items) => items,
                            Err(error) => {
                                record.lsp = None;
                                record.lsp_unavailable =
                                    Some(format!("lsp server closed: {error}"));
                                Vec::new()
                            }
                        }
                    }
                    None => Vec::new(),
                }
            }
        };
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

    fn lsp_diagnostics(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let reference: EditorLspRef = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(request_id, "editor_lsp_invalid", &error.to_string(), None);
            }
        };
        if reference.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "lsp request belongs to a previous editor epoch",
                None,
            );
        }
        let Some((revision, session_matches, unavailable)) = self
            .documents
            .get(&reference.document_id)
            .map(|record| {
                (
                    record.revision,
                    record.session_id == reference.session_id,
                    record.lsp_unavailable.clone(),
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
        if reference.document_revision != revision {
            return self.reject(
                request_id,
                "editor_lsp_stale",
                "lsp request revision is stale",
                Some(revision),
            );
        }
        if let Some(reason) = unavailable {
            return self.accept(
                request_id,
                json!(EditorLspDiagnosticsResult {
                    document_id: reference.document_id,
                    document_revision: revision,
                    diagnostics: Vec::new(),
                    server_unavailable: Some(reason),
                }),
                Some(revision),
            );
        }
        let record = self
            .documents
            .get(&reference.document_id)
            .expect("document was validated");
        let diagnostics = match &record.lsp {
            Some(lsp) => lsp.diagnostics(&record.lsp_uri),
            None => Vec::new(),
        };
        self.accept(
            request_id,
            json!(EditorLspDiagnosticsResult {
                document_id: reference.document_id,
                document_revision: revision,
                diagnostics,
                server_unavailable: None,
            }),
            Some(revision),
        )
    }

    fn lsp_hover(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        self.lsp_position_request(request_id, params, LspKind::Hover)
    }

    fn lsp_definition(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        self.lsp_position_request(request_id, params, LspKind::Definition)
    }

    fn lsp_references(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        self.lsp_position_request(request_id, params, LspKind::References)
    }

    fn lsp_signature_help(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        self.lsp_position_request(request_id, params, LspKind::SignatureHelp)
    }

    fn lsp_symbols(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let reference: EditorLspRef = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(request_id, "editor_lsp_invalid", &error.to_string(), None);
            }
        };
        if reference.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "lsp request belongs to a previous editor epoch",
                None,
            );
        }
        let Some((revision, session_matches)) = self
            .documents
            .get(&reference.document_id)
            .map(|record| {
                (
                    record.revision,
                    record.session_id == reference.session_id,
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
        if reference.document_revision != revision {
            return self.reject(
                request_id,
                "editor_lsp_stale",
                "lsp request revision is stale",
                Some(revision),
            );
        }
        let record = self
            .documents
            .get_mut(&reference.document_id)
            .expect("document was validated");
        match &mut record.lsp {
            Some(lsp) => {
                let uri = record.lsp_uri.clone();
                match lsp.request_symbols(&uri) {
                    Ok(symbols) => self.accept(
                        request_id,
                        json!(EditorLspSymbolsResult {
                            document_id: reference.document_id,
                            document_revision: revision,
                            symbols,
                        }),
                        Some(revision),
                    ),
                    Err(error) => {
                        record.lsp = None;
                        record.lsp_unavailable =
                            Some(format!("lsp server closed: {error}"));
                        self.accept(
                            request_id,
                            json!(EditorLspSymbolsResult {
                                document_id: reference.document_id,
                                document_revision: revision,
                                symbols: Vec::new(),
                            }),
                            Some(revision),
                        )
                    }
                }
            }
            None => self.accept(
                request_id,
                json!(EditorLspSymbolsResult {
                    document_id: reference.document_id,
                    document_revision: revision,
                    symbols: Vec::new(),
                }),
                Some(revision),
            ),
        }
    }

    fn lsp_position_request(
        &mut self,
        request_id: RequestId,
        params: Value,
        kind: LspKind,
    ) -> RpcResponse {
        let request: EditorLspPosition = match serde_json::from_value(params) {
            Ok(value) => value,
            Err(error) => {
                return self.reject(request_id, "editor_lsp_invalid", &error.to_string(), None);
            }
        };
        if request.epoch != self.epoch {
            return self.reject(
                request_id,
                "editor_epoch_stale",
                "lsp request belongs to a previous editor epoch",
                None,
            );
        }
        let Some((revision, session_matches)) = self
            .documents
            .get(&request.document_id)
            .map(|record| {
                (
                    record.revision,
                    record.session_id == request.session_id,
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
        if request.document_revision != revision {
            return self.reject(
                request_id,
                "editor_lsp_stale",
                "lsp request revision is stale",
                Some(revision),
            );
        }
        let record = self
            .documents
            .get_mut(&request.document_id)
            .expect("document was validated");
        match &mut record.lsp {
            Some(lsp) => {
                let uri = record.lsp_uri.clone();
                let outcome = match kind {
                    LspKind::Hover => lsp
                        .request_hover(&uri, request.position)
                        .map(|result| json!(EditorLspRawResult {
                            document_id: request.document_id.clone(),
                            document_revision: revision,
                            result,
                        })),
                    LspKind::SignatureHelp => lsp
                        .request_signature_help(&uri, request.position)
                        .map(|result| json!(EditorLspRawResult {
                            document_id: request.document_id.clone(),
                            document_revision: revision,
                            result,
                        })),
                    LspKind::Definition => lsp
                        .request_definition(&uri, request.position)
                        .map(|locations| json!(EditorLspLocationsResult {
                            document_id: request.document_id.clone(),
                            document_revision: revision,
                            locations,
                        })),
                    LspKind::References => lsp
                        .request_references(&uri, request.position)
                        .map(|locations| json!(EditorLspLocationsResult {
                            document_id: request.document_id.clone(),
                            document_revision: revision,
                            locations,
                        })),
                };
                match outcome {
                    Ok(result) => self.accept(request_id, result, Some(revision)),
                    Err(error) => {
                        record.lsp = None;
                        record.lsp_unavailable = Some(format!("lsp server closed: {error}"));
                        let fallback = match kind {
                            LspKind::Definition | LspKind::References => json!(
                                EditorLspLocationsResult {
                                    document_id: request.document_id,
                                    document_revision: revision,
                                    locations: Vec::new(),
                                }
                            ),
                            _ => json!(EditorLspRawResult {
                                document_id: request.document_id,
                                document_revision: revision,
                                result: Value::Null,
                            }),
                        };
                        self.accept(request_id, fallback, Some(revision))
                    }
                }
            }
            None => {
                let fallback = match kind {
                    LspKind::Definition | LspKind::References => json!(
                        EditorLspLocationsResult {
                            document_id: request.document_id,
                            document_revision: revision,
                            locations: Vec::new(),
                        }
                    ),
                    _ => json!(EditorLspRawResult {
                        document_id: request.document_id,
                        document_revision: revision,
                        result: Value::Null,
                    }),
                };
                self.accept(request_id, fallback, Some(revision))
            }
        }
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
        if let Some(record) = self.documents.remove(&reference.document_id) {
            if let Some(mut lsp) = record.lsp {
                let _ = lsp.close_document(&record.lsp_uri);
            }
        }
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

/// Which LSP introspection a position-scoped request wants.
#[derive(Clone, Copy)]
enum LspKind {
    Hover,
    Definition,
    References,
    SignatureHelp,
}

/// Resolve the canonical language identifier the runtime accepts.
fn language_from_name(name: &str) -> Option<LanguageKind> {
    match name {
        "nui_flow" => Some(LanguageKind::NuiFlow),
        "typescript" => Some(LanguageKind::Typescript),
        "rust" => Some(LanguageKind::Rust),
        "cpp" => Some(LanguageKind::Cpp),
        _ => None,
    }
}

/// Stable LSP URI for a document, derived from its id and language.
fn lsp_uri_for(document_id: &str, kind: LanguageKind) -> String {
    let extension = match kind {
        LanguageKind::NuiFlow => "nui",
        LanguageKind::Typescript => "ts",
        LanguageKind::Rust => "rs",
        LanguageKind::Cpp => "cpp",
    };
    format!("file:///neon3/{document_id}.{extension}")
}

/// Language-server command for a language. Each language can be overridden
/// with `NEON3_LSP_RUST` / `NEON3_LSP_TYPESCRIPT` / `NEON3_LSP_CPP`; the
/// defaults are the standard server binaries (`rust-analyzer`,
/// `typescript-language-server --stdio`, `clangd`).
fn lsp_command_for(kind: LanguageKind) -> Option<(String, Vec<String>)> {
    let (env_key, command, args) = match kind {
        LanguageKind::NuiFlow => return None,
        LanguageKind::Rust => ("NEON3_LSP_RUST", "rust-analyzer", Vec::new()),
        LanguageKind::Typescript => (
            "NEON3_LSP_TYPESCRIPT",
            "typescript-language-server",
            vec!["--stdio".into()],
        ),
        LanguageKind::Cpp => ("NEON3_LSP_CPP", "clangd", Vec::new()),
    };
    if let Ok(override_command) = std::env::var(env_key) {
        if !override_command.trim().is_empty() {
            return Some((override_command, args));
        }
    }
    Some((command.into(), args))
}

/// Spawn the language server for a non-Flow document and open it. Returns
/// `(client, unavailable_reason)`; a missing binary degrades to
/// `(None, Some(reason))` so the document stays fully editable with
/// tree-sitter highlighting.
fn spawn_lsp(
    kind: LanguageKind,
    uri: &str,
    source: &str,
) -> (Option<LspClient>, Option<String>) {
    let Some((command, args)) = lsp_command_for(kind) else {
        return (None, None);
    };
    let endpoint = LspEndpoint::Stdio {
        command: command.clone(),
        args,
    };
    match LspClient::connect(endpoint) {
        Ok(mut lsp) => {
            let language_id = match kind {
                LanguageKind::Rust => "rust",
                LanguageKind::Typescript => "typescript",
                LanguageKind::Cpp => "cpp",
                LanguageKind::NuiFlow => "plaintext",
            };
            if let Err(error) = lsp.open_document(uri, language_id, source) {
                return (None, Some(format!("lsp didOpen failed: {error}")));
            }
            (Some(lsp), None)
        }
        Err(error) => (None, Some(format!("{command} unavailable: {error}"))),
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

    #[test]
    fn rust_document_opens_edits_and_lsp_degrades() {
        // Deterministic degradation: even if rust-analyzer is installed on
        // this machine, force the spawn to fail so the test exercises the
        // server_unavailable path instead of blocking on a real server.
        unsafe { std::env::set_var("NEON3_LSP_RUST", "neon3-no-such-lsp-binary") };
        let mut runtime = EditorRuntime::new(7);
        let open = request(
            "editor.document.open",
            "open-rs",
            json!(EditorDocumentOpen {
                document_id: "doc-rs".into(),
                session_id: "session".into(),
                language: "rust".into(),
                source: "// hi\nfn main() { let x = 1; }\n".into(),
            }),
            None,
            "open-rs",
        );
        let (response, _) = runtime.handle(open);
        assert_eq!(response.status, RpcStatus::Accepted);
        let snapshot = &response.result.as_ref().unwrap()["snapshot"];
        assert_eq!(snapshot["language"], "rust");
        assert_eq!(snapshot["line_count"], 3);

        // Edit the rust document: language gate must be open.
        let apply = request(
            "editor.document.change.apply",
            "apply-rs",
            json!(EditorChangeApply {
                document_id: "doc-rs".into(),
                session_id: "session".into(),
                epoch: 7,
                change_set: ChangeSet {
                    base_revision: 1,
                    ops: vec![EditOp::Insert {
                        line: 1,
                        column: 0,
                        end: Position::new(1, 6),
                        text: "pub ".into(),
                    }],
                },
                kind: EditorChangeKind::Draft,
                cursor: None,
                selection: None,
            }),
            None,
            "apply-rs",
        );
        let (response, _) = runtime.handle(apply);
        assert_eq!(response.status, RpcStatus::Accepted);
        let snapshot = &response.result.as_ref().unwrap()["snapshot"];
        assert!(snapshot["source"]
            .as_str()
            .unwrap()
            .contains("pub fn main()"));

        // No LSP server installed in this environment: introspection must
        // degrade to a structured server_unavailable, not fail the document.
        let diagnostics = request(
            "editor.lsp.diagnostics",
            "diag-rs",
            json!(EditorLspRef {
                document_id: "doc-rs".into(),
                session_id: "session".into(),
                epoch: 7,
                document_revision: Revision(2),
            }),
            None,
            "diag-rs",
        );
        let (response, _) = runtime.handle(diagnostics);
        assert_eq!(response.status, RpcStatus::Accepted);
        let result = response.result.as_ref().unwrap();
        assert!(result["server_unavailable"]
            .as_str()
            .is_some_and(|reason| reason.contains("unavailable")));
    }

    #[test]
    fn unknown_language_is_rejected() {
        let mut runtime = EditorRuntime::new(7);
        let open = request(
            "editor.document.open",
            "open-py",
            json!(EditorDocumentOpen {
                document_id: "doc-py".into(),
                session_id: "session".into(),
                language: "python".into(),
                source: "print(1)".into(),
            }),
            None,
            "open-py",
        );
        let (response, _) = runtime.handle(open);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("editor_language_unsupported")
        );
    }

    #[test]
    fn lsp_hover_on_unavailable_server_returns_null_without_error() {
        let mut runtime = EditorRuntime::new(7);
        let open = request(
            "editor.document.open",
            "open-ts",
            json!(EditorDocumentOpen {
                document_id: "doc-ts".into(),
                session_id: "session".into(),
                language: "typescript".into(),
                source: "const x: number = 1;\n".into(),
            }),
            None,
            "open-ts",
        );
        let (response, _) = runtime.handle(open);
        assert_eq!(response.status, RpcStatus::Accepted);
        let hover = request(
            "editor.lsp.hover",
            "hover-ts",
            json!(EditorLspPosition {
                document_id: "doc-ts".into(),
                session_id: "session".into(),
                epoch: 7,
                document_revision: Revision(1),
                position: Position::new(0, 6),
            }),
            None,
            "hover-ts",
        );
        let (response, _) = runtime.handle(hover);
        assert_eq!(response.status, RpcStatus::Accepted);
        assert!(response.result.as_ref().unwrap()["result"].is_null());
    }
}
