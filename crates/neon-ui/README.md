# neon-ui

Unified windowless UI layer for Neon3.

Re-exports the full declarative UI schema (`UiFragment`, `UiInputFrame`,
`UiProgram`, …) and the UI runtime API (NUI Flow compiler, host adapter, runtime
entry). An external host depends on this single crate to reach every UI type and
start the windowless UI runtime via [`serve_forwarder`].

It contains no window or GPU code.

## License

MIT OR Apache-2.0
