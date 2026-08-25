# neon-wgpu-runtime

The Neon3 renderer owner. This crate owns the WGPU device/window, final UI
composition, render diagnostics, world camera input, and controlled external
surface interop.

The published crate uses a small open-source Latin fallback font. Applications
that need CJK or custom glyph coverage should upload a licensed font through
the project/asset font path rather than increasing the renderer crate archive.
