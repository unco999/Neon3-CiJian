# neon-editor

Language-agnostic headless code editor kernel, split out of [Neon3](https://github.com/unco999/Neon3-CiJian).

Pure Rust, no I/O, no window, no GPU — a library embedders link into their own
editor surfaces (NUI Flow `code_editor` component, desktop hosts, CLIs).

## Capabilities

- **Document model** — text buffer, `Position`/`Selection`, atomic `ChangeSet`
  ops, revisioned undo/redo, edit sessions (buffer, edits, lib).
- **Highlight** — incremental line token cache with a shared `Span` /
  `TokenClass` / `LineTokens` representation consumed by renderers.
- **Languages** — one abstraction, two sources:
  - NUI Flow: built-in table-driven grammar (`FlowGrammar`, token rules +
    completion tables).
  - TypeScript / Rust / C++: [tree-sitter](https://tree-sitter.github.io/)
    grammars compiled in; the CST is converted onto the same `LineTokens`
    representation so the highlight/rendering pipeline is reused unchanged.
  - Resolve by extension: `Language::from_extension("ts" | "rs" | "cpp" | "nui")`.
- **LSP client bridge** — JSON-RPC 2.0 over stdio or TCP using `lsp-types`
  messages: `initialize` / `didOpen` / `didChange` / `textDocument/completion`
  (converted to kernel `CompletionItem`s). Plug in tsserver, rust-analyzer or
  clangd for completions/diagnostics/hover on non-Flow languages.
- **Symbols / completions** — NUI Flow symbol index and grammar-driven
  completion candidates; non-Flow languages delegate completion to LSP.

## Usage

```rust
use neon_editor::{EditorCore, Language, Position};

// NUI Flow (table-driven, built-in)
let nui = EditorCore::from_language(
    "version 1\nsurface root w 400 h 300\n  text t value \"hi\"\n",
    Language::nui_flow(),
);

// Rust (tree-sitter)
let rust = EditorCore::from_language(
    "// note\nfn main() { let x = 1; }\n",
    Language::from_extension("rs").unwrap(),
);

// LSP-backed completions
let mut lsp = neon_editor::LspClient::connect(
    neon_editor::LspEndpoint::Stdio {
        command: "rust-analyzer".into(),
        args: vec![],
    }
)?;
lsp.open_document("file:///main.rs", "rust", source)?;
let items = lsp.request_completion("file:///main.rs", Position::new(0, 4))?;
```

## Features

- `tree-sitter` (default) — compile in the TS/Rust/C++ grammars.
- `lsp` (default) — `lsp-types` + client bridge.

The kernel itself stays free of any Neon3 runtime dependency (only `serde` +
the language crates), so it can be published and consumed independently.

## License

MIT OR Apache-2.0
