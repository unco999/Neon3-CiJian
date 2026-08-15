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

Input declarations have the form `input <key> <kind> default <literal>`. V1 supports `bool`, `i32`, `u32`, `f32`, and `text`; text defaults use the immutable empty handle, `text:empty`. The input schema installs every default at program activation. Inputs are direct values only, never JSON, lists, arbitrary objects or raw variable-length frame text.

## Components and attributes

The closed V1 vocabulary is `surface`, `panel`, `text`, `button`, `input`, `image`, `render`, `scroll`, `overlay`, `branch`, `repeat`, and `template`. No other component name is valid. `surface`, `panel`, `scroll`, `overlay`, `branch`, `repeat`, and `template` lower through the current compatible panel topology; bounded branch and template records are completed by their dedicated runtime capability.

Supported layout tokens are `row`, `column`, `overlay`, `w`, `h`, `minw`, `maxw`, `grow`, `shrink`, `basis`, `pad`, `gap`, `align`, and `justify`. Values are finite logical numbers. Alignment accepts `start`, `center`, `end`, and `stretch`; justification additionally accepts `between`, `around`, and `evenly`.

`token`, `fill`, `line`, and `ink` are visual declarations. `token` and `ink` require `token:<name>` references. `fill` and `line` accept an explicit `#RRGGBB` or `#RRGGBBAA` compatibility color until theme-token lowering carries those fields in canonical IR. There is no shader source or arbitrary visual expression.

Direct bindings use `$input_key` only. `value $name`, `enabled $can_commit`, and `visible $show_details` lower to canonical binding records. Literal `value` text is quoted. V1 conditional syntax is reserved for bounded branch lowering: `when $flag`, `when !$flag`, and `when $mode=ready`; complex predicates require a domain-provided boolean or enum input and are rejected rather than evaluated.

`event <dotted.intent>` declares a typed semantic intent. V1 event declarations contain no handlers and no computed payload expressions. Payload fields must be declared through the canonical event schema; Flow cannot inject pointer positions, render hit IDs, GPU handles or domain mutations.

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

Parser and lowerer failures are deterministic `NuiFlowParseDiagnostic` records with `code`, severity, message, source span, and optional suggestion. Stable V1 errors include `nui_flow_tabs_forbidden`, `nui_flow_mixed_indentation`, `nui_flow_unknown_component`, `nui_flow_duplicate_attribute`, `nui_flow_unquoted_text`, `nui_flow_invalid_patch_path`, `nui_flow_stale_patch_revision`, and `ui_program_forbidden_flow_feature`.

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
