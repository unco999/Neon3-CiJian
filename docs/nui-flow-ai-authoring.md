# AI NUI Flow Authoring

This is the required entry point for an AI or automation client creating Neon3
UI. Read `AGENTS.md` first, then this document, then
`plan/neon3-nui-flow.md` for the complete V1 grammar.

## Ownership

NUI Flow is a declarative authoring format. It can declare hierarchy, layout,
typed inputs, bounded branches/templates, semantic intents, and finite
presentation statecharts. It cannot execute code, read files, access network
resources, create GPU objects, calculate domain rules, or mutate project data.

Domain services own business truth. They send typed input frames and text
handles. The statechart is UI-local presentation state only. `emit` produces a
typed semantic intent for a domain service; domain completion returns through a
new input snapshot.

## Entry Points

| Need | Entry point |
| --- | --- |
| Parse and validate a Flow document | `neon_ui_runtime::parse_nui_flow` |
| Format a document | `neon_ui_runtime::format_nui_flow` |
| Lower to canonical IR | `neon_ui_runtime::lower_nui_flow` |
| Compile portable UI program | `neon_ui_runtime::compile_nui_flow_program` |
| Parse/apply a stable-key patch | `parse_nui_flow_patch` / `apply_nui_ir_patch` |
| Execute local statechart | `NuiFlowStateMachineRuntime` |
| Inspect/dry-run/replay | `UiDebugSession` APIs in `neon-ui-runtime` |
| Submit a visual fragment | `wgpu.ui.submit_fragment` through the public RPC boundary |

The complex reference fixture is
`tests/fixtures/ui/asset-review-workbench.nui`.

### External Image Case

An external engine does not put image bytes, file paths, or GPU handles in NUI
Flow. It declares the Image node and a stable binding effect in the fragment,
then sends the source bytes through the public RPC chain:

```text
external engine -> ui.image.upload { image_id, media_type, width, height, bytes }
ui-runtime      -> wgpu.ui.image.upload
wgpu-runtime    -> { texture_index, generation, atlas_size, region, uv }
```

The fragment binds the semantic Image node to the source without an `AssetRef`:

```json
{
  "kind": "image_binding",
  "node_id": "thumbnail",
  "image_id": "engine-image-01"
}
```

`neon-ui-runtime` validates and forwards this request only. `neon-wgpu-runtime`
owns the atlas, upload, slot assignment, region, generation, and sampling.
`projectd` is not involved in this path. The executable acceptance entry point
is `cargo run -p neon-ui-runtime --bin image_resource_probe`.

## Authoring Workflow

1. Define every domain-provided value as an `input` with a type and default.
2. Use `text` inputs for stable text handles only. Do not place raw dynamic
   strings in an input frame.
3. Add panels, controls, render surfaces, branches, repeats, and templates with
   stable semantic keys.
4. Declare semantic `event` intents on interactive nodes.
5. Add a finite UI-local statechart only for presentation transitions.
6. Parse, compile, inspect, and dry-run patches before submitting a revision.

## Component Controls

The first generic control batch is `checkbox`, `radio_button`, `slider`,
`drag_value`, `combo`, `dropdown`, `tabs`, `selectable`, `list_box`, `scrollbar`, and
`progress_bar`. Bind control state only through typed inputs:

```text
input enabled bool default true
input checked bool default false
input amount f32:0..1 default 0.5
input choice enum:alpha|beta default alpha
surface controls column w 320 h 220
  checkbox feature checked $checked enabled $enabled event settings.feature.toggle
  slider amount numeric $amount enabled $enabled event settings.amount.commit
  combo choice state $choice enabled $enabled event settings.choice.select
  progress_bar progress numeric $amount enabled $enabled event settings.progress.preview
```

`checked` and `selected` require bool inputs. `numeric` and `scroll` require
`i32`, `u32`, or `f32` inputs. Interactive numeric controls should declare a
range with deterministic `<kind>:<minimum>..<maximum>` syntax, such as
`i32:0..24` or `f32:0..1`. `state` requires an enum input. Use an event for
the declared preview, commit, or selection semantic; do not encode a control
value in the intent name, pointer coordinates, or a renderer-local identifier.
The renderer keeps focus and pointer capture local, excludes disabled controls
from hit testing, and never makes a progress bar pointer-interactive.

## Statechart Syntax

Statecharts are flat declarations so the grammar remains deterministic and has
no block evaluator, callback, loop, or expression language.

```text
machine asset_review initial loading
state asset_review ready
state asset_review publishing
state asset_review error

sync asset_review when $workspace_state=ready -> ready
sync asset_review when $workspace_state=error -> error
on asset_review asset.review.publish when $can_publish -> publishing emit asset.review.publish
on asset_review asset.review.retry -> loading emit asset.review.retry
```

Rules:

- `machine <key> initial <state>` creates a finite local statechart.
- `state <machine> <state>` adds an allowed target state.
- `sync` reacts only to `$bool`, `!$bool`, or `$enum=value` input predicates.
- `on` reacts only to a declared dotted semantic intent.
- `emit` is optional and can contain only one dotted semantic intent.
- A transition target must be a declared state.
- `branch <key> in <machine>.<state>` selects a precompiled branch directly
  from local state. It never changes topology at runtime.
- Call `dispatch_semantic_event` only after the normal semantic-event gate has
  accepted the event. Its transition result contains the new presentation state
  and optional domain `emit` intent.

## View Syntax

```text
input project_title text default text:empty
input can_publish bool default false
input workspace_state enum:loading|ready|error default loading

surface asset-review column w 1440 h 900 align stretch fill #101820
  panel toolbar row h 44 pad 8 fill #203040
    text title value $project_title
    button publish value "Publish" enabled $can_publish event asset.review.publish
branch loading-view when $workspace_state=loading
  text message value "Loading review"

branch publishing-view in asset_review.publishing
  panel banner h 32 fill #38556A
    text message value "Publishing review"
```

Use `repeat <key> h <height> capacity <n> key <field> overflow_summary` and
`template <key> h <height> capacity <n> key <field> overflow_summary` for
bounded rows. Give nested panel containers explicit dimensions or `align stretch`
when they need a visible background.

## Drag Syntax

```text
machine inspector_drag initial resting
state inspector_drag dragging
state inspector_drag accepted
on inspector_drag ui.drag.begin -> dragging
on inspector_drag ui.drag.end -> accepted emit asset.review.inspector.position.accept

drag inspector-drag source inspector axis both snap 8 threshold 3 within parent
```

`drag` is presentation-only. It declares a stable key, source node, allowed
axis, logical snap distance, dead-zone threshold, and boundary policy. `within
parent` clamps to the source parent, `within surface` clamps to the renderer
viewport, and `within free` leaves the preview unconstrained. Flow lowers it to
a generic `UiEffect::DragBinding`; WGPU owns pointer capture, preview offset,
snapping, and clamping. Pointer movement never crosses the process boundary. A
resolved release emits only the declared intent and stable source/target keys
through UI runtime.

For persistent layout, the domain must return an explicit accepted/rejected
input. A publish-style statechart uses the same pattern:

```text
input publish_status enum:idle|pending|accepted|rejected default idle
sync asset_review when $publish_status=accepted -> accepted
sync asset_review when $publish_status=rejected -> error
```

## Drop And Reparent

```text
drag backlog-card-drag source backlog-card-01 axis both snap 8 threshold 3 within surface
drop progress-drop target in-progress-panel accepts backlog-card-drag present progress-template emit workspace.card.reparent
```

`drop` accepts exactly one declared drag key and one target node. Its optional
`placement` is `into`, `before`, or `after`; omitted placement defaults to
`into`. An optional `present <template-key>` selects a bounded template owned
directly by the target. The key must name a declared template capability and be
a direct child of the target for `into`, `before`, and `after` placements. It
lowers to a generic `UiEffect::DropBinding`. WGPU resolves the
deepest topmost valid target under the pointer, excludes the active dragged
subtree, and emits stable source/target keys, placement, and the declared
semantic intent. Pointer coordinates and preview offsets never leave WGPU. It
does not move the source node into the target node's `children` list.

Panels clip child overflow to their bounds by default. Use `clip none` to let
descendants paint and receive hits outside the panel, `clip rounded` to clip to
the panel corner radius, or `clip scroll` for a scroll viewport. The `scroll`
component selects `clip scroll` by default.

For an actual A-to-B parent change:

1. WGPU/local UI previews the drag and resolves the declared drop target.
2. The controller emits `workspace.card.reparent` with stable source/target
   identity and the current revision.
3. The domain validates permissions and the target's accepted row schema.
4. The accepted payload includes the target-owned presentation template key.
   The domain uses source data with that template to construct a revisioned
   representation, removing the node from A and inserting it as a child for
   `into`, or as the immediate sibling before/after the target for relative
   placements. WGPU does not apply this template or mutate the canonical tree.
5. A new `UiProgram` revision is compiled and submitted; the domain returns
   `reparent_status=accepted` or `rejected`.

This preserves fixed runtime topology. Drag preview is local; only an accepted
revision changes canonical parentage.

## Validation Checklist

```text
cargo test -p neon-ui-runtime nui_flow::tests::
cargo test -p neon-ui-runtime nui_state_machine::tests::
cargo test -p neon-ui-runtime terrain_workbench::tests::
git diff --check
```

For a window demo, start `neon-wgpu-runtime --window-server` and submit only a
validated `UiFragment` through `wgpu.ui.submit_fragment`. Do not create windows
or WGPU resources in an AI/Flow client.

## AI Boundaries

Never add these to Flow: arbitrary JavaScript/Rust/Lua, callbacks, URLs, file
paths, GPU handles, pointer coordinates, element IDs, dynamic topology, raw
text payloads, domain mutation, filtering, sorting, or permission logic.

## Camera-Gated World Panels

`world panel` is a normal `panel` subtree with one additional renderer gate. It
uses the same layout, text, styles, and semantic events as a screen panel, but
is not rendered until its declared camera has supplied a valid frame through
the world information bridge:

```text
surface editor column
  world panel mission-marker camera 3d:editor-camera w 240 h 48
    text mission-title value "Mission"
```

The only accepted camera forms are `2d:<stable-camera-id>` and
`3d:<stable-camera-id>`. A camera at the coordinate origin is valid; missing
means no valid frame has been accepted for that ID and kind. Flow cannot define
camera matrices, world coordinates, shader parameters, or transport details.
