# NUI Geometry And Custom Shader Contract

## Status

This document defines the next renderer capability for the Pulse music-player
case. It is an implementation contract, not a promise that arbitrary shader
code can run inside a client process.

The capability has four owners:

```text
NUI Flow                 declares geometry and a stable shader key
Node SDK                 registers a bounded shader package and submits Flow
neon-ui-runtime          validates the declaration and resolves resource keys
neon-wgpu-runtime        compiles, caches, binds, and draws the shader
```

`neon-ui-runtime` and Node never receive a GPU handle, texture pointer, bind
group, pipeline object, or HWND. The WGPU runtime remains the only GPU owner.

## Visual Target

The supplied Music Player design uses:

- diagonal panel corners and cut tabs;
- neon edge light and narrow rim highlights;
- translucent white and dark glass surfaces;
- animated light sweeps on selected controls;
- image panels with clipped, skewed silhouettes;
- waveform and equalizer accents.

These effects are not all the same problem. Geometry must affect layout,
clipping, and hit testing consistently. A material shader may affect pixels but
must not change event routing or domain state.

## Rendering Model

The renderer composes a node in this order:

```text
logical bounds
  -> geometry transform and clip
  -> material/shader evaluation
  -> premultiplied-alpha output
  -> local UI blend state
  -> final window/composition surface
```

### Material Draw Layer

A `material` declaration does not replace the host panel. The runtime creates a
transparent draw layer that is stacked at the **same z-order directly above** the
host panel. The layer:

- has the same logical position and size as the host panel by default;
- never participates in layout measurement;
- never participates in hit testing or pointer routing;
- is the only surface the custom shader paints onto;
- receives the expanded bounds as its shader input.

This keeps the material authoring surface separate from the interaction
surface. A glow, halo, or light sweep can paint outside the panel without
changing where clicks land.

The layer can be larger than the host panel through an explicit `overflow`
declaration:

```text
material pulse-glass overflow 24 8 24 16
```

`overflow` is `[left, top, right, bottom]` in logical pixels. The draw bounds
passed to the shader are the host bounds expanded by those amounts:

```json
{
  "material": {
    "package_id": "pulse-glass",
    "version": 1,
    "overflow": [24, 8, 24, 16],
    "parameters": {"rim_strength": 0.18}
  }
}
```

The standard UI path remains available for every node. A custom shader is an
optional material attached to a node or a skin slot. If compilation, validation,
or capability negotiation fails, the renderer uses the declared fallback
material and returns a structured diagnostic. It never silently renders a
blank node.

## Geometry

Geometry is declarative and renderer-independent. The first version supports a
bounded affine cut transform:

```text
geometry cut
  top-left 18
  top-right 10
  bottom-right 18
  bottom-left 10
```

The values are logical pixels. They define the amount removed from each corner
of a rectangular node. The renderer creates a deterministic convex polygon,
clips children to the same polygon, and uses that polygon for hit testing.

Future geometry forms may include `skew`, `bevel`, and `path`, but they must
define both a visual clip and an interaction region. A shader-only diagonal
discard is not sufficient because it would leave pointer hit testing rectangular.

Proposed IR:

```json
{
  "geometry": {
    "kind": "cut",
    "insets": [18, 10, 18, 10]
  }
}
```

The geometry declaration is independent from `UiStyle`. `UiStyle` continues to
own color, opacity, border width, and radius. Geometry values must be finite,
non-negative, bounded by the node size, and stable across frames.

## Shader Packages

NUI Flow references a stable shader key, never a source file path:

```text
shader pulse-glass version 1 fallback standard_ui
  source pulse-glass.wgsl
  entry material
  parameter light_position f32 default 0.35 range 0 1
  parameter rim_strength f32 default 0.18 range 0 1
  parameter sweep_speed f32 default 0.25 range 0 2

surface surface.music-player-demo revision 1
  panel hero-card x 24 y 88 w 746 h 230
    geometry cut top-left 20 top-right 16 bottom-right 20 bottom-left 16
    material pulse-glass overflow 24 8 24 16 parameter rim_strength 0.22
```

`source` is resolved by the Node shader registry and becomes an immutable
package reference in the public UI IR. The renderer does not open arbitrary
paths received from a fragment. A package must be registered before a Flow can
refer to it.

The first shader ABI is deliberately small:

```text
material(input):
  input.local_position: vec2<f32>
  input.bounds: vec4<f32>
  input.base_color: vec4<f32>
  input.border_color: vec4<f32>
  input.time_seconds: f32
  input.opacity: f32
  input.geometry_edge: f32
  input.state_flags: u32

output.color: vec4<f32>       // premultiplied RGBA
```

The runtime owns viewport, time, texture bindings, clipping, and blend state.
Client shaders may use declared image resources through stable resource keys,
but cannot request a new texture, sampler, storage buffer, or external handle.

Shader restrictions:

- WGSL only in the first version;
- one fragment/material entry point per package;
- no compute, storage textures, atomics, recursion, or dynamic resource access;
- bounded instruction and source-size budgets;
- no filesystem access, IPC, window access, or host callbacks;
- explicit color space and premultiplied-alpha contract;
- deterministic fallback material;
- shader key and package version are part of diagnostics and frame traces.

## Node SDK API

The Node SDK owns package registration before `NeonApp.start` submits a Flow:

```ts
const app = await NeonApp.start({
  mode: "windowed",
  window: { chrome: "borderless", initialSize: [1280, 800] },
  shaders: {
    root: resolve("src/shaders"),
    packages: ["pulse-glass.wgsl", "pulse-equalizer.wgsl"],
  },
});
```

The SDK sends a typed control-plane request equivalent to:

```text
wgpu.shader.register
  package_id: pulse-glass
  version: 1
  source_digest: sha256:...
  source_bytes: bounded binary payload, never a JSON GPU handle
  entry_point: material
  fallback: standard_ui
```

The runtime replies with `registered`, `rejected`, or `fallback`. A Flow that
uses an unregistered shader is rejected with a stable error code. Node can then
submit a revised Flow after registration succeeds.

For development, the SDK may read a local file and calculate the digest. The
runtime remains responsible for the final source validation and compilation.
Production packages should be immutable and signed or digest-pinned.

## NUI Flow Rules

Allowed:

```text
shader pulse-glass
material pulse-glass
material pulse-glass overflow 24 8 24 16 parameter rim_strength 0.22
geometry cut top-left 18 top-right 12 bottom-right 18 bottom-left 12
parameter sweep_speed $glass_sweep_speed
```

Forbidden:

```text
shader "C:\\some\\arbitrary\\file.wgsl"
shader_source "...source code..."
callback draw(...)
texture_handle 0x1234
```

Shader parameters may bind only to declared typed presentation inputs with a
bounded range. They cannot bind directly to domain objects, filesystem paths,
element IDs, or pointer coordinates.

## Pulse Material Set

The first Pulse package should contain:

```text
pulse-glass
  white transmission
  edge rim highlight
  slow diagonal light sweep
  selected/pressed state response

pulse-neon-edge
  lime edge emission
  clipped corner highlight
  hover/pressed intensity

pulse-equalizer
  bounded bar animation from typed waveform data
  no audio-device access from the shader
```

Real desktop blur remains a Windows composition responsibility. The material
shader adds the surface response; it must not pretend that procedural noise is
background blur.

## Validation And Diagnostics

New public capability names:

```text
ui.geometry.cut.v1
ui.shader.package.v1
ui.shader.material.v1
```

Required error codes:

```text
nui_flow_unknown_shader
nui_flow_invalid_geometry
ui_shader_unregistered_package
ui_shader_source_too_large
ui_shader_validation_failed
ui_shader_compile_failed
ui_shader_parameter_out_of_range
ui_shader_unsupported_backend
```

The runtime diagnostic stream must report JSONL records containing:

```json
{
  "event": "neon.ui.shader.frame",
  "frame_sequence": 42,
  "fragment_revision": 7,
  "node_id": "hero-card",
  "shader_key": "pulse-glass",
  "shader_version": 1,
  "geometry": {"kind": "cut", "insets": [20, 16, 20, 16]},
  "parameter_values": {"rim_strength": 0.18},
  "fallback_used": false,
  "result": "passed"
}
```

Focused acceptance probes must cover:

1. package registration and digest mismatch;
2. parser round trip for shader, geometry, and material-overflow declarations;
3. invalid geometry, negative overflow, and out-of-range parameters;
4. shader compilation and deterministic fallback;
5. visual polygon and rectangular hit-test agreement;
6. material draw layer larger than the host panel while hit routing stays on
   the host logical bounds;
7. transparent premultiplied output;
8. resize, hover, pressed, and animation state;
9. final PNG capture with non-empty UI and no unexpected opaque background.

## Implementation Order

1. Add `UiGeometry` and `UiMaterialRef` to `neon-ui-schema` with validation.
2. Add parser support for `geometry cut`, `shader`, `material`, and bounded
   parameters.
3. Add `wgpu.shader.register` to the public protocol and Node SDK.
4. Add a renderer material registry with standard fallback and JSONL diagnostics.
5. Implement cut geometry in the UI vertex/clip path and align hit testing.
6. Implement `pulse-glass.wgsl` and `pulse-neon-edge.wgsl`.
7. Apply the packages to the Pulse Flow and validate the supplied design.
8. Add the equalizer material only after the static material path is stable.

## Non-Goals

- letting Node call wgpu directly;
- allowing arbitrary GLSL/HLSL or native DLL shader plugins;
- moving business rules into shaders;
- using shader discard as a substitute for hit-test geometry;
- replacing Windows Composition backdrop blur with procedural grain;
- changing Linux, macOS, Android, or the domain service ownership model.
