# Changelog

All notable changes to Neon3 are recorded in this file.

## v0.2.3 — 2026-09-01

### Added

- Added the `neon-gpu-ecs` workspace crate: a serializable IR-driven GPU ECS
  runtime that generates one multi-entry-point WGSL module, performs GPU-side
  query compaction/sorting, and dispatches scheduled systems indirectly. The
  crate is compute-only; `neon-wgpu-runtime` remains the sole owner of the
  WGPU device and queue.
- Added typed `TextRef::Rich` / `UiRichTextSpan` data to the public UI schema.
  Rich spans carry only bounded text, RGBA color, and scale; they do not cross
  renderer handles, executable markup, or resource paths across the UI boundary.
- Added NUI Flow support for JSON-encoded `rich` spans on `text` nodes.
- Added renderer-final layout diagnostics to the WGPU runtime debug snapshot,
  including each node's final logical bounds and clip bounds after flex,
  visibility, scroll, and clipping resolution.
- Added Windows native image selection with `Ctrl+O`, and enabled the documented
  Explorer drop messages for elevated renderer windows. Selected files continue
  through the existing file-drop publication path.

### Changed

- Tooltips may now use image resources and nine-slice frames, matching the
  supported panel/image decoration path.
- Rendered rich text is laid out span-by-span with its own color and scale while
  retaining inherited opacity, world scale, and clipping.
- Flow children with an authored `x` or `y` are now absolute children of the
  parent content box; they no longer consume a row/column flex track or shift
  their flow siblings. Zero-offset children continue to participate in normal
  flex layout.
- Slider interaction and chrome now share one predictable full-width inset track,
  so minimum, midpoint, and maximum values map to the left, center, and right
  of the same geometry.
- Drag previews are composed in the final screen-space layer, above later
  panels, popups, modals, and component chrome without mutating the canonical
  UI tree.
- Tooltip image batches use a dedicated GPU buffer and hidden tooltips are
  excluded from the popup-image pass.
- Default styles for containers, labels, images, and render surfaces are now
  transparent instead of receiving unintended component chrome.
- Text clipping has a small bounded glyph safety allowance to prevent the final
  glyph from being shaved at the nominal right edge.

### Verification

- Schema suite: 31 passed.
- `canvas_panel_probe` emitted JSONL producer/consumer callbacks and passed:
  `frame_sequence=1`, `canvas_data_version=1`, `line_count=2`,
  `point_count=1`, `graph_revision=1`, and matching consumer fragment data.
- `neon-gpu-ecs` full suite (IR validation, WGSL generation, control flow,
  sorting vs CPU reference, execution/change filters, structural replay,
  render data): 66 passed.

### Release status

- Published as GitHub release **v0.2.3** with the Windows x86_64 runtime
  bundle `neon3-runtime-windows-x86_64-v0.2.3.zip` (`neon-eventd`,
  `neon-ui-runtime`, `neon-wgpu-runtime`, font assets, release manifest).
- `neon-gpu-ecs` is a workspace crate only; crates.io registry publication
  (`scripts/publish-crates.ps1`) is a separate step and was not run for this tag.
