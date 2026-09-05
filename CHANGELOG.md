# Changelog

All notable changes to Neon3 are recorded in this file.

## v0.2.5 — 2026-09-04

### Added

- **Android Host GPU surface export.** The Android Host foreground service now
  runs the GPU-backed headless server (`spawn_headless_external_server`) instead
  of the no-GPU protocol server. SDKs can call `render.surface.open` and
  `render.surface.capture_png` on the device, producing a real PNG artifact
  (verified: 1280x720, 9082 bytes, valid PNG signature).
- **Cross-platform shared surfaces.** `SharedSurface` is now backend-agnostic:
  the wgpu texture, size, and frame sequence are available on every platform,
  while the DX12 shared texture/fence interop handles are Windows-only. The
  headless GPU exporter (`HeadlessExternalGpu`) no longer requires Windows:
  non-Windows hosts create ordinary wgpu textures (with `TEXTURE_BINDING` for
  readback sampling) and rotate ring slots with an atomic cursor instead of
  consumer fences.
- **`render.surface.capture_png`** on the headless external server: mirrors the
  latest completed shared surface into an offscreen render target and writes a
  PNG artifact (debug builds), enabling automated visual acceptance for the
  shared surface path on every platform.
- **`neon-android-host` workspace crate** with the `android-platform-probe` bin
  and the JNI `hostStart`/`hostStop` contract. `hostStart` joins the GPU
  headless server until `service.shutdown`, then invokes
  `onHostServerStopped()` so the foreground service can stop itself.
- **Single-endpoint UI session methods on the headless host**:
  `ui.flow.submit`, `debug.ui.host.snapshot`, `ui.host.inbound`, and
  `ui.input.frame`, so the Node/Python SDK `UiSession` flow works against the
  Android host without a local ui-runtime process.
- `--headless-external-server <endpoint>` entry point in
  `neon-wgpu-runtime` (Windows only).

### Changed

- `neon-wgpu-runtime` builds as both `rlib` and `cdylib` (needed by the
  Android host JNI library).
- The headless external instance uses minimal `InstanceFlags`
  (`VALIDATION_INDIRECT_CALL`, no `DEBUG`) to avoid SwiftShader/ranchu crashes
  in `SetDebugUtilsObjectNameEXT`.
- The headless render loop wraps `gpu.render` in `catch_unwind` so a render
  panic cannot poison the shared gpu mutex (previously every later handler
  failed with `handler_panicked`).

### Verification

- `cargo check` passes for both Windows and `x86_64-linux-android` targets.
- `neon-wgpu-runtime` single-endpoint UI session contract test passes.
- Windows: `render.surface.open` (d3d12_shared_texture_v1) ->
  `render.surface.capture_png` produces a 704-byte valid PNG.
- Android emulator (API 36 x86_64, SwiftShader): `ui.flow.submit` ->
  `render.surface.open` -> `render.surface.capture_png` (frame_sequence=1) ->
  `service.shutdown`; PNG pulled back via `run-as` and verified.
- Node SDK 77 tests and Python SDK 92 tests pass with integration gates;
  desktop behavior unchanged.

## v0.2.4 — 2026-09-02

### Added

- Renderer text-input diagnostic state to the window input snapshot.

### Fixed

- UI host cache now uses the renderer-accepted fragment revision for
  subsequent control events instead of a stale revision.
- Dropdown and combo popup pointer handling: option selection, toggling, and
  dismiss resolve before generic hit routing.
- DataGrid text-input focus, caret, and selection cleanup after pointer and
  keyboard cancellation.
- Slider hit geometry aligned with the visible full-width inset track.

### Verification

- neon-ui-runtime library tests: 114 passed.
- component-gallery launched as a real windowed multi-process session.
- Windows x86_64 release binaries built.

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
## v0.2.6 — 2026-09-06

- 修复 NUI Flow 动态状态重挂后的完整 input 恢复。
- 增加 TextInput 提交事件的 typed 文本传递，支持外部 host 发送聊天消息。
- 修复窗口输入失焦提交、聊天发送按钮和多控件布局问题。
- 增加可执行 Flow submit probe。
