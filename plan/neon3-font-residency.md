# Neon3 Font Residency Decision

## Scope

`assets/fonts/SarasaUiSC-Light.ttf` is the licensed P0 test fixture and
bootstrap font source. `neon-projectd` remains the authority that returns
revisioned `AssetBytes`; only `neon-wgpu-runtime` parses font bytes or owns
font GPU resources.

## P0 Coverage Atlas

The existing renderer uses `fontdue` to parse the TTF once per accepted
preload and creates one private coverage atlas, sampler, bind group, and
glyph cache in the WGPU process. It prewarms printable ASCII and rasterizes
additional glyphs only when a submitted UI fragment needs them.

This is intentional. A 23 MiB TTF is not a GPU renderable format, and
uploading the raw file provides no glyph lookup, shaping, or coverage data to
the shader. Pre-rasterizing all Sarasa CJK glyphs into the current 1024x1024
atlas would overflow it and impose unacceptable startup latency and VRAM use.

## Product Text Path

The successor to P0 is a versioned `font/gpk` project asset produced by an
offline font-pack job. The pack compiler validates a TTF, records licensing
metadata and source hash, then emits explicit glyph lookup, metrics, contour,
and segment sections. `neon-projectd` persists and serves the revisioned pack
as bytes. The renderer validates the pack and, once per `(AssetRef, device)`:

1. creates immutable GPU storage buffers for lookup, metrics, contours, and
   segments with one bulk upload;
2. creates a private multi-page SDF or MTSDF atlas and bind group;
3. generates atlas tiles only for glyphs used by visible text, retaining them
   under a bounded residency policy;
4. reports `loading`, `ready`, `failed`, atlas pressure, and generation under
   the originating request/job ID.

Neon2's `mmap` belongs at the local pack reader: it maps a precompiled GPK
file to avoid a second CPU copy before the one-time buffer uploads. It is not
a cross-process transport and must not expose a file path, mapped pointer, or
GPU handle through the Neon3 protocol. For remote or owner-served asset bytes,
the same parser operates over an owned byte slice rather than requiring mmap.

This document is a design decision, not a Neon2 code migration. P0 remains a
coverage-atlas implementation until a dedicated GPK contract, pack job,
schema/ABI tests, bounded atlas policy, and GPU acceptance scenario are added.
