# neon-world-bridge

Renderer-neutral world and camera synchronization contract for Neon3.

Carries revisioned world information, latest-value camera frames, and world-space
UI anchors. It never exposes GPU resources, renderer-local matrices, or pointers;
the sole renderer owner projects these samples to screen space.

## License

MIT OR Apache-2.0
