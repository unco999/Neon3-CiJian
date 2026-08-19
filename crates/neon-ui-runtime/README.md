# neon-ui-runtime

Headless UI declaration runtime for Neon3.

Parses and compiles NUI Flow source, owns the active `UiProgram` / `UiInputSchema`
via the host adapter, validates external input frames, and forwards fragments to
the renderer. It contains no window or GPU code.

## License

MIT OR Apache-2.0
