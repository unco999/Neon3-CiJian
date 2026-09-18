//! End-to-end probe: spawn the real `rust-analyzer`, open a rust document,
//! and exercise diagnostics / hover / definition / symbols over the wire.
//! Run: `cargo run -p neon-editor --example lsp_probe`
use std::time::Duration;

use neon_editor::{LspClient, LspEndpoint, Position};

fn main() {
    let source = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() {\n    let x = add(1, 2);\n    println!(\"{}\", x);\n}\n";
    let uri = "file:///neon3/probe.rs";

    let mut lsp = match LspClient::connect(LspEndpoint::Stdio {
        command: "rust-analyzer".into(),
        args: Vec::new(),
        env: std::collections::HashMap::new(),
    }) {
        Ok(lsp) => lsp,
        Err(error) => {
            println!("SPAWN_FAIL: {error}");
            return;
        }
    };
    println!("CONNECTED");
    if let Err(error) = lsp.open_document(uri, "rust", source) {
        println!("OPEN_FAIL: {error}");
        return;
    }
    println!("DID_OPEN");

    // Give the server time to load the crate + publish diagnostics.
    std::thread::sleep(Duration::from_secs(10));
    let diagnostics = lsp.diagnostics(uri);
    println!(
        "DIAGNOSTICS({}): {}",
        diagnostics.len(),
        serde_json::to_string(&diagnostics).unwrap()
    );

    // Hover over `add` inside the call at line 2, col 13.
    match lsp.request_hover(uri, Position::new(2, 13)) {
        Ok(result) => println!("HOVER: {}", serde_json::to_string(&result).unwrap()),
        Err(error) => println!("HOVER_ERR: {error}"),
    }

    // Jump to definition of `add`.
    match lsp.request_definition(uri, Position::new(2, 13)) {
        Ok(locations) => println!(
            "DEFINITION({}): {}",
            locations.len(),
            serde_json::to_string(&locations).unwrap()
        ),
        Err(error) => println!("DEFINITION_ERR: {error}"),
    }

    // References of `add`.
    match lsp.request_references(uri, Position::new(0, 4)) {
        Ok(locations) => println!(
            "REFERENCES({}): {}",
            locations.len(),
            serde_json::to_string(&locations).unwrap()
        ),
        Err(error) => println!("REFERENCES_ERR: {error}"),
    }

    // Document outline.
    match lsp.request_symbols(uri) {
        Ok(symbols) => println!(
            "SYMBOLS({}): {}",
            symbols.len(),
            serde_json::to_string(&symbols).unwrap()
        ),
        Err(error) => println!("SYMBOLS_ERR: {error}"),
    }

    // Signature help inside the `add(1, 2)` call.
    match lsp.request_signature_help(uri, Position::new(2, 17)) {
        Ok(result) => println!(
            "SIGNATURE_HELP: {}",
            serde_json::to_string(&result).unwrap()
        ),
        Err(error) => println!("SIGNATURE_ERR: {error}"),
    }

    if let Err(error) = lsp.close_document(uri) {
        println!("CLOSE_ERR: {error}");
    } else {
        println!("DID_CLOSE");
    }
}
