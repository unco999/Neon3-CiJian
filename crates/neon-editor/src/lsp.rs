//! LSP client bridge.
//!
//! The kernel itself stays free of language intelligence for non-Flow
//! languages: completions / diagnostics / hover are delegated to a standard
//! LSP server (tsserver, rust-analyzer, clangd, ...). This module provides
//! the wire side: JSON-RPC over stdio or TCP using [`lsp_types`] messages,
//! and conversion of `textDocument/completion` results into the kernel's
//! [`CompletionItem`].

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::buffer::Position;
use crate::completion::{CompletionItem, CompletionKind, CompletionSource};

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
            responders: std::collections::HashMap::new(),
        }));

        let pending_reader = Arc::clone(&pending);
        let reader_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let Some(line) = read_lsp_message(&mut reader) else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                // Route responses by id; notifications are dropped.
                if let Some(id) = value.get("id") {
                    let responder = {
                        let mut guard = pending_reader.lock().unwrap();
                        guard.responders.remove(&id.to_string())
                    };
                    if let Some(tx) = responder {
                        let _ = tx.send(Ok(value));
                    }
                }
            }
        });

        let mut client = LspClient {
            writer: Some(Mutex::new(writer)),
            _reader_thread: reader_thread,
            pending,
            server_process,
            initialized: false,
        };

        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": null,
                "capabilities": {},
            }),
        )?;
        client.notify(
            "initialized",
            json!({}),
        )?;
        client.initialized = true;
        Ok(client)
    }

    /// Send a request and block for its response.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
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
        match rx.recv() {
            Ok(Ok(value)) => {
                if let Some(error) = value.get("error") {
                    return Err(LspError::Protocol(error.to_string()));
                }
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(LspError::ServerShutdown),
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
}
