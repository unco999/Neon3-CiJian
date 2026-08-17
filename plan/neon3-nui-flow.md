# NUI Flow V1

NUI Flow V1 is a compact, line-oriented authoring and patch notation for Neon3 UI declarations. It is not executable code. Its only output is a validated `UiIrDocument` and, for patches, a validated `UiIrPatch`. JSON UI IR remains the canonical persisted representation.

## Ownership and boundary

`neon-ui-runtime` parses, validates, formats and lowers Flow. It does not query projects, calculate business facts, create WGPU objects, access files or execute a callback. Domain owners provide already-formatted display values and business facts through typed, defaulted input slots. `neon-wgpu-runtime` owns final measurement, hit testing, GPU resources and pixels.

Flow source, source spans and diagnostics use logical units only. Stable node keys, domain IDs and source spans can be exposed by semantic inspection APIs. Renderer-local hit IDs, physical-pixel coordinates, GPU handles and instance indices cannot appear in Flow, patches, semantic events or inspection APIs.

## Lexical rules

- Source is UTF-8 and uses one declaration per line.
- Indentation is exactly two spaces per hierarchy level. Tabs and odd indentation are errors.
- A line whose first non-space character is `#` is a comment. Inline comments are not part of V1.
- Strings use double quotes. `\"`, `\\` and `\n` are the supported escapes. Literal text containing whitespace must be quoted.
- Node and input keys use ASCII letters, digits, `.`, `_`, and `-`. They are stable semantic names, never list indexes.
- Braces, square brackets, assignment syntax, URLs, callbacks, function syntax, code blocks and expressions are forbidden.

## Document form

```text
version 1
surface surface.editor.terrain revision 12
budget nodes=128 bindings=96 instances=128 text=96 glyphs=2048 events=64 clips=32
input can_commit bool default false
input terrain_name text default text:empty
surface workbench row gap 8 token token:surface
  panel tool_rail column gap 4
    button water_tool value "Water" enabled $can_commit event terrain.tool.select
  panel inspector column gap 6
    text terrain_title value $terrain_name
```

The root declaration has no indentation. A Flow document has one root. `surface` is the preferred root vocabulary name; legacy-compatible `panel` roots remain accepted while the canonical IR uses the existing `Panel` node kind.

Input declarations have the form `input <key> <kind> default <literal>`. V1 supports `bool`, `i32`, `u32`, `f32`, and `text`; numeric kinds may declare an inclusive range as `<kind>:<minimum>..<maximum>` (for example `i32:0..24` or `f32:0..1`), and text defaults use the immutable empty handle, `text:empty`. Range bounds and defaults must be ordered, finite where applicable, and type-correct. The input schema installs every default at program activation. Inputs are direct values only, never JSON, lists, arbitrary objects or raw variable-length frame text.

### Directed variable events (`emitevent`)

An input may end with the trailing marker `emitevent` to declare that changes to
that variable are forwarded to `neon-eventd` as a directed event:

```text
flow terrain-workbench
input brush_size i32:0..24 default 4 emitevent
```

The event name follows the fixed rule `flow.<flow_name>.<variable_key>`, where
`flow_name` comes from the `flow <name>` declaration line (required for any
`emitevent` input). Grid inputs cannot declare `emitevent`. Only declared
variables produce directed events; undeclared variable changes stay silent.
The event payload carries `module`, `surface`, `variable_key`, `kind`, and
old/new values as structured observation data. A directed event is an
observation, not a domain command: receivers must never mutate authoritative
state from an event payload.

## Components and attributes

The closed V1 vocabulary is `surface`, `panel`, `text`, `button`, `input`, `checkbox`, `radio_button`, `slider`, `drag_value`, `combo`, `dropdown`, `tabs`, `selectable`, `list_box`, `scrollbar`, `progress_bar`, `image`, `render`, `scroll`, `overlay`, `branch`, `repeat`, and `template`. No other component name is valid. `surface`, `panel`, `scroll`, `overlay`, `branch`, `repeat`, and `template` lower through the current compatible panel topology; bounded branch and template records are completed by their dedicated runtime capability.

Supported layout tokens are `row`, `column`, `overlay`, `w`, `h`, `minw`, `maxw`, `grow`, `shrink`, `basis`, `pad`, `gap`, `align`, `justify`, and `clip`. Values are finite logical numbers. Alignment accepts `start`, `center`, `end`, and `stretch`; justification additionally accepts `between`, `around`, and `evenly`. Panels default to `clip bounds`; `clip none`, `clip bounds`, `clip rounded`, and `clip scroll` are explicit policies. Rounded clipping uses the panel corner radius for both pixels and hit tests; `scroll` also uses the existing layout scroll offset.

`token`, `fill`, `line`, and `ink` are visual declarations. `token` and `ink` require `token:<name>` references. `fill` and `line` accept an explicit `#RRGGBB` or `#RRGGBBAA` compatibility color until theme-token lowering carries those fields in canonical IR. There is no shader source or arbitrary visual expression.

Direct bindings use `$input_key` only. `value $name`, `enabled $can_commit`, and `visible $show_details` lower to canonical binding records. Literal `value` text is quoted. Component state uses typed forms: `checked $bool` for checkboxes, `selected $bool` for radio buttons and selectables, `numeric $i32_or_f32` for sliders, drag values, scrollbars, and progress bars, and `state $enum` for combos, dropdowns, tabs, and list boxes. V1 conditional syntax is reserved for bounded branch lowering: `when $flag`, `when !$flag`, and `when $mode=ready`; complex predicates require a domain-provided boolean or enum input and are rejected rather than evaluated.

`event <dotted.intent>` declares a typed semantic intent. V1 event declarations contain no handlers and no computed payload expressions. Payload fields must be declared through the canonical event schema; Flow cannot inject pointer positions, render hit IDs, GPU handles or domain mutations.

### Camera-Gated World Panels

`world panel <key> camera <2d:id|3d:id>` lowers to the same `Panel` node kind
as `panel`, plus a renderer-neutral camera visibility effect. Its descendants
remain a normal fixed NUI subtree. WGPU excludes the full subtree from layout,
hit testing, and drawing until the named camera has a valid frame in the active
world information bridge session. Camera coordinates, matrices, transport, and
GPU resources are not Flow features.

The program event boundary distinguishes activation, value preview, value commit, selection change, text commit, and cancellation. The renderer may retain hover, pointer capture, and focus locally for enabled interactive controls, but it emits only the declared semantic intent. `progress_bar` is display-only in the renderer; a preview event, when declared, remains a typed program event rather than a pointer-derived domain mutation.

## Local Statecharts

Flow may declare a finite presentation-only statechart. It is executed by
`NuiFlowStateMachineRuntime` in `neon-ui-runtime`; it is not a general script
engine and cannot mutate domain state, access I/O, call functions, or evaluate
arbitrary expressions.

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

`machine` declares a unique local chart and its initial state. `state` adds a
finite legal target. `sync` consumes only a direct bool or enum input predicate.
`on` consumes only a declared dotted semantic intent. `emit` is optional and
publishes one typed semantic intent for the owning domain service. Domain
completion always returns through a fresh typed input snapshot; it does not
write the statechart directly.

A branch may directly select a local state with
`branch <key> in <machine>.<state>`. This only gates a precompiled subtree;
it cannot create nodes or bypass the semantic event gate.

## Drag Interactions

Flow can declare a finite presentation drag without introducing pointer scripts:

```text
drag inspector-drag source inspector axis both snap 8 threshold 3 within parent
```

The declaration names a stable drag key and source node. Axis is
`horizontal`, `vertical`, or `both`; snap and threshold are nonnegative logical
units. `within parent` clamps to the source parent, `within surface` clamps to
the renderer viewport, and `within free` leaves the preview unconstrained. Flow
lowers the declaration to a generic `UiEffect::DragBinding`; WGPU owns capture,
dead-zone filtering, snap rounding, boundary clamping, and the immediate
preview. It resolves matching `UiEffect::DropBinding` declarations locally on
release. Raw pointer coordinates and local offsets do not leave WGPU; the UI
runtime receives only the declared intent and stable source/target keys.

If a layout change needs persistence, the drag-end transition emits a domain
intent. The domain responds with an explicit enum/bool/text-handle input state;
the UI must not infer acceptance from elapsed time or drag position.

## Drop And Reparent

```text
drop progress-drop target in-progress-panel accepts backlog-card-drag placement into present progress-template emit workspace.card.reparent
```

`drop` has a stable key, a declared target node, exactly one accepted drag key,
an optional `placement <into|before|after>` clause (default `into`), an optional
`present <template-key>` for a bounded template directly owned by the target,
and one dotted semantic intent. `present` is valid with `into`, `before`, and
`after`; accepted drops require this target-owned template so the domain can
render source data in the target's representation. `into` inserts the new
representation as a target child; `before` and `after` insert it as the target's
immediate sibling. The presentation key is forwarded to the domain; the
renderer never applies the patch. Resolving a drop selects the deepest/topmost valid
target under the pointer, excluding the active dragged subtree, and produces a
proposal containing stable source and target keys plus placement. It cannot edit
the active `UiIrDocument` or alter a node's running parent.

Actual parent changes use the revisioned authoring path: domain validation,
`UiIrPatch` remove/insert by stable path, a new compiled `UiProgram`, and a new
accepted/rejected domain snapshot. This is the only supported A-to-B reparent
workflow in V1.

## Patches

Patch source begins with `@ revision <document-revision>`. Every operation carries this expected revision and has a precise source span.

```text
@ revision 12
~ /workbench/tool_rail/water_tool enabled false
+ /workbench/inspector text help_text
- /workbench/inspector/obsolete_help
> /workbench/tool_rail/water_tool /workbench/inspector
```

`+` inserts a closed-vocabulary component under a stable parent path. `-` removes a non-root node. `~` sets one supported static property (`enabled`, `visible`, `value`, `w`, or `h`). `>` moves a non-root node to a stable destination parent. Paths may be a globally unique semantic key or slash-separated stable semantic keys. Numeric array indexes are rejected. Applying a patch validates its expected revision and canonical IR validity, then creates exactly one next document revision. Dry-run uses the same validation and returns diagnostics without replacing the active program.

## Errors and compatibility

Parser and lowerer failures are deterministic `NuiFlowParseDiagnostic` records with `code`, severity, message, source span, and optional suggestion. Stable V1 errors include `nui_flow_mixed_indentation`, `nui_flow_unknown_component`, `nui_flow_duplicate_attribute`, `nui_flow_unquoted_text`, `nui_flow_invalid_patch_path`, `nui_flow_stale_patch_revision`, and `ui_program_forbidden_flow_feature`.

Formatting parses and re-emits the supported canonical subset in a stable order. It preserves declaration semantics, not comments or whitespace. Lowering preserves node source spans into `UiProgram` diagnostics and dependency inspection. A newer Flow version must be explicitly versioned and rejected by V1 parsers; it must not silently reinterpret an existing program.

## Terrain workbench example

```text
version 1
surface surface.editor.terrain-workbench revision 12
input can_commit bool default false
input terrain_name text default text:empty
surface workbench row gap 8 token token:surface
  panel tool_rail column gap 4 token token:rail
    button water_tool value "Water" enabled $can_commit event terrain.tool.select
  panel viewport column grow 1
    render terrain_view
  panel inspector column gap 6 token token:inspector
    text terrain_title value $terrain_name
```

The example describes hierarchy, layout, defaults, bindings and one semantic intent without implying any execution, domain-state inference, or renderer ownership outside the WGPU runtime.
