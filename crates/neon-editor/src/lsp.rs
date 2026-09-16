//! LSP client bridge.
//!
//! The kernel itself stays free of language intelligence for non-Flow
//! languages: completions / diagnostics / hover are delegated to a standard
//! LSP server (tsserver, rust-analyzer, clangd, ...). This module provides
//! the wire side: JSON-RPC over stdio or TCP using [`lsp_types`] messages,
//! and conversion of `textDocument/completion` results into the kernel's
//! [`CompletionItem`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::buffer::Position;
use crate::completion::{CompletionItem, CompletionKind, CompletionSource};

/// A 1-based or 0-based line/character position inside an LSP range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// A zero-based LSP range (start inclusive, end exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// A structured diagnostic pushed by `textDocument/publishDiagnostics`.
/// Severity follows LSP: 1 = Error, 2 = Warning, 3 = Information, 4 = Hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub range: LspRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub message: String,
}

/// A location inside a document (`textDocument/definition`, `references`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

/// One entry of `textDocument/documentSymbol` (flat or nested).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspSymbol {
    pub name: String,
    /// LSP SymbolKind integer (1 = File, 2 = Module, 3 = Namespace, 4 = Package,
    /// 5 = Class, 6 = Method, 7 = Property, 8 = Field, 9 = Constructor,
    /// 10 = Enum, 11 = Interface, 12 = Function, 13 = Variable, 14 = Constant,
    /// 15 = String, 16 = Number, 17 = Boolean, 18 = Array, ...).
    pub kind: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub range: LspRange,
    pub selection_range: LspRange,
    #[serde(default)]
    pub children: Vec<LspSymbol>,
}

/// Where an LSP server lives.
#[derive(Clone, Debug)]
pub enum LspEndpoint {
    /// Spawn a language server process and speak JSON-RPC over its stdin /
    /// stdout.
    Stdio { command: String, args: Vec<String> },
    /// Connect to an already-running server over TCP (JSON-RPC framing).
    Tcp { address: String },
}

/// Errors surfaced by the LSP bridge.
#[derive(Debug)]
pub enum LspError {
    Io(std::io::Error),
    Protocol(String),
    ServerShutdown,
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspError::Io(error) => write!(f, "lsp io: {error}"),
            LspError::Protocol(message) => write!(f, "lsp protocol: {message}"),
            LspError::ServerShutdown => write!(f, "lsp server closed"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(value: std::io::Error) -> Self {
        LspError::Io(value)
    }
}

type ResponseSender = Sender<Result<Value, LspError>>;

struct Pending {
    next_id: u64,
    responders: std::collections::HashMap<String, ResponseSender>,
}

/// A minimal JSON-RPC 2.0 client for one LSP server.
pub struct LspClient {
    writer: Option<Mutex<Box<dyn Write + Send>>>,
    /// stdout reader thread end (kept alive for the client's lifetime).
    _reader_thread: std::thread::JoinHandle<()>,
    pending: Arc<Mutex<Pending>>,
    /// Latest `textDocument/publishDiagnostics` per URI, written by the reader
    /// thread and read by [`LspClient::diagnostics`].
    diagnostics: Arc<Mutex<HashMap<String, Vec<LspDiagnostic>>>>,
    server_process: Option<Child>,
    initialized: bool,
}

impl LspClient {
    /// Connect (spawn or dial) and run `initialize` / `initialized`.
    pub fn connect(endpoint: LspEndpoint) -> Result<Self, LspError> {
        let (writer, reader, server_process): (
            Box<dyn Write + Send>,
            Box<dyn Read + Send>,
            Option<Child>,
        ) = match endpoint {
            LspEndpoint::Stdio { command, args } => {
                let mut child = Command::new(&command)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()?;
                // Fast-fail: some toolchains ship a launcher shim (e.g. the
                // rustup `rust-analyzer` proxy) that exits immediately when
                // the actual component is missing. Detecting that here keeps
                // document open fast instead of blocking on an initialize
                // timeout.
                std::thread::sleep(std::time::Duration::from_millis(800));
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(LspError::Protocol(format!(
                        "language server exited immediately ({status})"
                    )));
                }
                let stdin = child.stdin.take().expect("lsp child stdin");
                let stdout = child.stdout.take().expect("lsp child stdout");
                (Box::new(stdin), Box::new(stdout), Some(child))
            }
            LspEndpoint::Tcp { address } => {
                let stream = std::net::TcpStream::connect(&address)?;
                let reader = stream.try_clone()?;
                (Box::new(stream), Box::new(reader), None)
            }
        };

        let pending = Arc::new(Mutex::new(Pending {
            next_id: 0,
            responders: HashMap::new(),
        }));
        let diagnostics = Arc::new(Mutex::new(HashMap::<String, Vec<LspDiagnostic>>::new()));

        let pending_reader = Arc::clone(&pending);
        let diagnostics_reader = Arc::clone(&diagnostics);
        let reader_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let Some(line) = read_lsp_message(&mut reader) else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                // Route responses by id; notifications are handled here.
                if let Some(id) = value.get("id") {
                    let responder = {
                        let mut guard = pending_reader.lock().unwrap();
                        guard.responders.remove(&id.to_string())
                    };
                    if let Some(tx) = responder {
                        let _ = tx.send(Ok(value));
                    }
                } else if let Some(method) = value.get("method").and_then(Value::as_str) {
                    if method == "textDocument/publishDiagnostics" {
                        collect_diagnostics(&diagnostics_reader, &value);
                    }
                }
            }
        });

        let mut client = LspClient {
            writer: Some(Mutex::new(writer)),
            _reader_thread: reader_thread,
            pending,
            diagnostics,
            server_process,
            initialized: false,
        };

        client.request_with_timeout(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": null,
                "capabilities": {},
            }),
            std::time::Duration::from_secs(5),
        )?;
        client.notify(
            "initialized",
            json!({}),
        )?;
        client.initialized = true;
        Ok(client)
    }

    /// Send a request and block for its response (15s cap).
    fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        self.request_with_timeout(method, params, std::time::Duration::from_secs(15))
    }

    /// Send a request and block up to `timeout` for its response.
    fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, LspError> {
        if !self.initialized && method != "initialize" {
            return Err(LspError::Protocol("client not initialized".into()));
        }
        let id = {
            let mut guard = self.pending.lock().unwrap();
            guard.next_id += 1;
            guard.next_id
        };
        let (tx, rx) = channel();
        {
            let mut guard = self.pending.lock().unwrap();
            guard.responders.insert(id.to_string(), tx);
        }
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(value)) => {
                if let Some(error) = value.get("error") {
                    return Err(LspError::Protocol(error.to_string()));
                }
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                let _ = self.pending.lock().map(|mut guard| guard.responders.remove(&id.to_string()));
                Err(LspError::Protocol("request timed out".into()))
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send(&mut self, message: Value) -> Result<(), LspError> {
        let body = serde_json::to_string(&message).map_err(|e| {
            LspError::Protocol(format!("serialize request: {e}"))
        })?;
        let mutex = self
            .writer
            .take()
            .ok_or_else(|| LspError::Protocol("writer already taken".into()))?;
        let mut writer = mutex.lock().map_err(|_| {
            LspError::Protocol("lsp writer poisoned".into())
        })?;
        write!(*writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        writer.flush()?;
        drop(writer);
        self.writer = Some(mutex);
        Ok(())
    }

    /// Open a document in the server (textDocument/didOpen).
    pub fn open_document(&mut self, uri: &str, language_id: &str, text: &str) -> Result<(), LspError> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Request completions at a position, converted to kernel items.
    pub fn request_completion(
        &mut self,
        uri: &str,
        position: Position,
    ) -> Result<Vec<CompletionItem>, LspError> {
        let result = self.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": {
                    "line": position.line as u64,
                    "character": position.column as u64,
                },
                "context": { "triggerKind": 1 },
            }),
        )?;
        Ok(parse_completion_result(result))
    }

    /// Notify a document edit (textDocument/didChange, full sync).
    pub fn change_document(
        &mut self,
        uri: &str,
        version: i64,
        text: &str,
    ) -> Result<(), LspError> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        )
    }

    /// Notify document close (textDocument/didClose).
    pub fn close_document(&mut self, uri: &str) -> Result<(), LspError> {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Latest published diagnostics for `uri` (empty when none arrived).
    pub fn diagnostics(&self, uri: &str) -> Vec<LspDiagnostic> {
        self.diagnostics
            .lock()
            .map(|guard| guard.get(uri).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// `textDocument/hover` — raw LSP result (markdown/plaintext contents).
    pub fn request_hover(
        &mut self,
        uri: &str,
        position: Position,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": lsp_position(position),
            }),
        )
    }

    /// `textDocument/definition` — resolved jump targets.
    pub fn request_definition(
        &mut self,
        uri: &str,
        position: Position,
    ) -> Result<Vec<LspLocation>, LspError> {
        let result = self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": lsp_position(position),
            }),
        )?;
        Ok(parse_locations(result))
    }

    /// `textDocument/references` — all references of the symbol at `position`.
    pub fn request_references(
        &mut self,
        uri: &str,
        position: Position,
    ) -> Result<Vec<LspLocation>, LspError> {
        let result = self.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": lsp_position(position),
                "context": { "includeDeclaration": true },
            }),
        )?;
        Ok(parse_locations(result))
    }

    /// `textDocument/documentSymbol` — outline of the document.
    pub fn request_symbols(&mut self, uri: &str) -> Result<Vec<LspSymbol>, LspError> {
        let result = self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )?;
        Ok(parse_symbols(result))
    }

    /// `textDocument/signatureHelp` — raw LSP result.
    pub fn request_signature_help(
        &mut self,
        uri: &str,
        position: Position,
    ) -> Result<Value, LspError> {
        self.request(
            "textDocument/signatureHelp",
            json!({
                "textDocument": { "uri": uri },
                "position": lsp_position(position),
            }),
        )
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if self.initialized {
            let _ = self.notify("shutdown", json!({}));
            let _ = self.notify("exit", json!({}));
        }
        if let Some(mut child) = self.server_process.take() {
            let _ = child.kill();
        }
    }
}

/// Converts a kernel position to an LSP (0-based) position object.
fn lsp_position(position: Position) -> Value {
    json!({ "line": position.line, "character": position.column })
}

/// Parses a `textDocument/publishDiagnostics` notification and stores it in
/// the shared slot, keyed by URI.
fn collect_diagnostics(slot: &Arc<Mutex<HashMap<String, Vec<LspDiagnostic>>>>, value: &Value) {
    let uri = value
        .get("params")
        .and_then(|params| params.get("uri"))
        .and_then(Value::as_str);
    let Some(uri) = uri else { return };
    let Some(diagnostics) = value
        .get("params")
        .and_then(|params| params.get("diagnostics"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let parsed: Vec<LspDiagnostic> = diagnostics
        .iter()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect();
    if let Ok(mut guard) = slot.lock() {
        guard.insert(uri.to_string(), parsed);
    }
}

/// Normalizes a single location (Location | LocationLink) into our shape.
fn parse_locations(result: Value) -> Vec<LspLocation> {
    let entries = match result {
        Value::Null => return Vec::new(),
        Value::Array(items) => items,
        Value::Object(map) => match map.get("uri") {
            // Single Location object.
            Some(_) => vec![Value::Object(map)],
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            let target = match entry.get("targetUri") {
                Some(Value::String(uri)) => {
                    // LocationLink form.
                    let range = entry.get("targetRange")?;
                    Some((uri.clone(), range.clone()))
                }
                _ => {
                    let uri = entry.get("uri")?.as_str()?.to_string();
                    let range = entry.get("range")?.clone();
                    Some((uri, range))
                }
            };
            let (uri, range) = target?;
            let range: LspRange = serde_json::from_value(range).ok()?;
            Some(LspLocation { uri, range })
        })
        .collect()
}

/// Parses `textDocument/documentSymbol` (either `SymbolInformation[]` or a
/// nested `DocumentSymbol[]` hierarchy).
fn parse_symbols(result: Value) -> Vec<LspSymbol> {
    let Value::Array(entries) = result else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            // DocumentSymbol form: has name + range + selectionRange.
            if entry.get("selectionRange").is_some() {
                let name = entry.get("name")?.as_str()?.to_string();
                let kind = entry.get("kind").and_then(Value::as_u64).unwrap_or(0) as u32;
                let detail = entry.get("detail").and_then(Value::as_str).map(String::from);
                let range: LspRange = serde_json::from_value(entry.get("range")?.clone()).ok()?;
                let selection_range: LspRange =
                    serde_json::from_value(entry.get("selectionRange")?.clone()).ok()?;
                let children = entry
                    .get("children")
                    .and_then(Value::as_array)
                    .map(|children| {
                        children
                            .iter()
                            .filter_map(|child| serde_json::from_value(child.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                Some(LspSymbol {
                    name,
                    kind,
                    detail,
                    range,
                    selection_range,
                    children,
                })
            } else {
                // SymbolInformation form: name + location.
                let name = entry.get("name")?.as_str()?.to_string();
                let kind = entry.get("kind").and_then(Value::as_u64).unwrap_or(0) as u32;
                let detail = entry.get("containerName").and_then(Value::as_str).map(String::from);
                let location = entry.get("location")?;
                let _uri = location.get("uri")?.as_str()?.to_string();
                let range: LspRange =
                    serde_json::from_value(location.get("range")?.clone()).ok()?;
                Some(LspSymbol {
                    name,
                    kind,
                    detail,
                    selection_range: range,
                    range,
                    children: Vec::new(),
                })
            }
        })
        .collect()
}

/// Reads one LSP message (Content-Length framed) from a buffered reader.
fn read_lsp_message(reader: &mut impl BufRead) -> Option<String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    String::from_utf8(body).ok()
}

/// Convert an LSP `textDocument/completion` result into kernel items.
fn parse_completion_result(result: Value) -> Vec<CompletionItem> {
    let list = match result {
        Value::Array(items) => items,
        Value::Object(map) => match map.get("items") {
            Some(Value::Array(items)) => items.clone(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    list.into_iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            let text_edit = item.get("textEdit").or_else(|| item.get("textEditText"));
            let insert_text = match text_edit {
                Some(Value::Object(edit)) => edit
                    .get("newText")
                    .and_then(|value| value.as_str())
                    .unwrap_or(&label)
                    .to_string(),
                Some(Value::String(text)) => text.clone(),
                _ => label.clone(),
            };
            let (replace_start, replace_end) = match text_edit {
                Some(Value::Object(edit)) => {
                    let range = edit.get("range");
                    let start = range.and_then(|r| r.get("start"));
                    let end = range.and_then(|r| r.get("end"));
                    let to_position = |p: Option<&Value>| Position::new(
                        p.and_then(|v| v["line"].as_u64()).unwrap_or(0) as u32,
                        p.and_then(|v| v["character"].as_u64()).unwrap_or(0) as u32,
                    );
                    (to_position(start), to_position(end))
                }
                _ => (Position::START, Position::START),
            };
            Some(CompletionItem {
                item_id: format!("lsp:{}", label),
                label: label.clone(),
                kind: CompletionKind::Value,
                detail: item
                    .get("detail")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                insert_text,
                replace_start,
                replace_end,
                sort_text: label.clone(),
                source: CompletionSource::Lsp,
                commit_characters: Vec::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_result_parses_array() {
        let result = json!([
            {"label": "alpha", "kind": 6, "detail": "var"},
            {"label": "beta", "textEdit": {"newText": "beta()", "range": {
                "start": {"line": 1, "character": 2},
                "end": {"line": 1, "character": 6}
            }}}
        ]);
        let items = parse_completion_result(result);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "alpha");
        assert_eq!(items[1].insert_text, "beta()");
        assert_eq!(items[1].replace_start, Position::new(1, 2));
        assert_eq!(items[1].source, CompletionSource::Lsp);
    }

    #[test]
    fn completion_result_parses_object() {
        let result = json!({"isIncomplete": false, "items": [{"label": "x"}]});
        let items = parse_completion_result(result);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "x");
    }

    #[test]
    fn empty_on_other_shapes() {
        assert!(parse_completion_result(Value::Null).is_empty());
        assert!(parse_completion_result(json!({"nope": 1})).is_empty());
    }

    #[test]
    fn diagnostics_collect_and_parse() {
        let slot = Arc::new(Mutex::new(HashMap::<String, Vec<LspDiagnostic>>::new()));
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///a.rs",
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
                    "severity": 1,
                    "code": "E0308",
                    "source": "rustc",
                    "message": "mismatched types"
                }]
            }
        });
        collect_diagnostics(&slot, &notification);
        let guard = slot.lock().unwrap();
        let diags = guard.get("file:///a.rs").expect("diagnostics stored");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(1));
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
        assert_eq!(diags[0].range.start.line, 0);
        assert_eq!(diags[0].range.end.character, 9);
    }

    #[test]
    fn parse_locations_handles_single_and_links() {
        let single = json!({
            "uri": "file:///a.rs", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4}}
        });
        let locs = parse_locations(single);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///a.rs");
        assert_eq!(locs[0].range.start.line, 1);

        let links = json!([{
            "targetUri": "file:///b.rs",
            "targetRange": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 2}}
        }]);
        let locs = parse_locations(links);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///b.rs");
        assert_eq!(locs[0].range.end.character, 2);
    }

    #[test]
    fn parse_symbols_nested_document_symbols() {
        let result = json!([{
            "name": "main",
            "kind": 12,
            "detail": "fn",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 1}},
            "selectionRange": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}},
            "children": [{
                "name": "x",
                "kind": 13,
                "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 8}},
                "selectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 3}}
            }]
        }]);
        let symbols = parse_symbols(result);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "main");
        assert_eq!(symbols[0].kind, 12);
        assert_eq!(symbols[0].children.len(), 1);
        assert_eq!(symbols[0].children[0].name, "x");
    }

    #[test]
    fn parse_symbols_flat_symbol_information() {
        let result = json!([{
            "name": "helper",
            "kind": 12,
            "containerName": "mod",
            "location": {
                "uri": "file:///a.rs",
                "range": {"start": {"line": 4, "character": 0}, "end": {"line": 6, "character": 1}}
            }
        }]);
        let symbols = parse_symbols(result);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "helper");
        assert_eq!(symbols[0].detail.as_deref(), Some("mod"));
        assert_eq!(symbols[0].selection_range.start.line, 4);
    }
}
