//! Unified windowless UI layer for Neon3.
//!
//! This crate re-exports the full declarative UI schema surface and the UI
//! runtime API, so an external host (such as a game engine) can depend on a
//! single crate and reach every UI type, the NUI Flow compiler, the host
//! adapter, and the windowless runtime entry point.
//!
//! It contains no window or GPU code; rendering stays in the sole renderer
//! owner (`neon-wgpu-runtime`).

pub use neon_ui_runtime::*;
pub use neon_ui_schema::*;

use std::net::SocketAddr;

/// Starts the windowless UI runtime as a blocking RPC server.
///
/// * `endpoint` — loopback address the UI runtime listens on.
/// * `wgpu_endpoint` — the sole renderer owner.
/// * `domain_endpoint` — the optional domain host (may be unused for hosts that
///   submit NUI Flow source directly via `ui.flow.submit`).
/// * `eventd_endpoint` — optional event hub for `emitevent` variable events.
/// * `epoch` — the renderer epoch this UI runtime coordinates with.
pub fn serve_forwarder(
    endpoint: SocketAddr,
    wgpu_endpoint: SocketAddr,
    domain_endpoint: SocketAddr,
    eventd_endpoint: Option<SocketAddr>,
    epoch: u64,
) -> Result<(), neon_ipc::TransportError> {
    neon_ui_runtime::UiRuntime::serve_forwarder(
        endpoint,
        wgpu_endpoint,
        domain_endpoint,
        eventd_endpoint,
        epoch,
    )
}

/// Spawns the windowless UI runtime on a background thread, so a host can start
/// it from `main` and keep running its own loop (for example a game engine).
///
/// Returns the join handle; the thread runs until the process exits or the
/// runtime is shut down.
pub fn spawn_forwarder(
    endpoint: SocketAddr,
    wgpu_endpoint: SocketAddr,
    domain_endpoint: SocketAddr,
    eventd_endpoint: Option<SocketAddr>,
    epoch: u64,
) -> std::thread::JoinHandle<Result<(), neon_ipc::TransportError>> {
    std::thread::spawn(move || {
        serve_forwarder(
            endpoint,
            wgpu_endpoint,
            domain_endpoint,
            eventd_endpoint,
            epoch,
        )
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reexports_schema_types_and_runtime_api() {
        // Schema types are reachable through the single `neon-ui` crate.
        let _: Option<crate::UiFragment> = None;
        let _: Option<crate::UiInputFrame> = None;
        let _: Option<crate::UiProgramRevision> = None;
        // Runtime compiler and entry point are reachable too.
        let _ = crate::parse_nui_flow;
        let _ = crate::compile_nui_flow_program;
        let _ = crate::serve_forwarder;
    }
}
