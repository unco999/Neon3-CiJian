# NUI Flow V1

## Status

NUI Flow is a compact authoring and patch notation for Neon3 UI declarations. It
parses deterministically into the versioned `UiIrDocument`; JSON IR remains the
canonical persisted representation. Flow is not executable code and is never a
runtime evaluator, a domain query language, or a renderer protocol.

## Lexical Rules

Files are UTF-8 text. One declaration occupies one line. `#` starts a comment.
Indentation is exactly two ASCII spaces per hierarchy level. Tabs, mixed-width
indentation, braces, square brackets, assignments, callbacks, functions, URLs,
code blocks, loops, and expressions are rejected. Keys use ASCII letters,
digits, `.`, `_`, and `-`. Literal text is a single quoted token with `\"` and
`\n` escapes; unquoted whitespace text is rejected.

The only conditional form reserved for a later bounded branch declaration is
`when $boolean`, `when !$boolean`, or `when $enum=variant`. Complex conditions
must arrive as a domain-owned typed input. Flow does not calculate permissions,
validation, eligibility, filtering, sorting, formatting, or resource state.

## Document Form

```text
version 1
surface surface.editor.terrain revision 12
budget nodes=64 bindings=32 instances=64 text=32 glyphs=1024 events=32 clips=16
input can_commit bool default false
input terrain_name text default text:empty
panel workspace row gap 8 token token:panel
  panel rail column gap 4
    button water value "Water" enabled $can_commit event terrain.tool.select
  panel inspector column gap 6
    text title value $terrain_name
```

`surface` metadata is declared once at indentation zero. `budget` is optional;
the V1 defaults are bounded and compiled into the canonical resource budget.
Each `input` has a stable key, one of `bool`, `i32`, `u32`, `f32`, or `text`, and
an explicit compatible default. `text:empty` is the default handle placeholder;
real dynamic text arrives through the text registry/input-frame boundary.

## Components and Properties

The closed V1 component vocabulary is `surface`, `panel`, `text`, `button`,
`input`, `image`, `render`, `scroll`, `overlay`, `branch`, `repeat`, and
`template`. The latter four lower to bounded presentation declarations and do
not introduce loops or dynamic component kinds. Unknown component names fail.

Supported tokens are `row`, `column`, `overlay`, `w`, `h`, `minw`, `maxw`,
`grow`, `shrink`, `basis`, `pad`, `gap`, `align`, `justify`, `token`, `fill`,
`line`, `ink`, `value`, `enabled`, `visible`, and `event`. Layout values are
finite logical-unit numbers. `fill` and `line` accept `#RRGGBB` or `#RRGGBBAA`.
`ink` and `token` require a `token:<name>` reference. No raw shader or resource
handle is permitted.

`value $slot`, `enabled $slot`, and `visible $slot` lower to direct canonical
IR bindings. Literal `value` text is quoted. `event` accepts a dotted semantic
intent name only. Events carry no handler, source position, render hit ID, or
domain mutation; the normal revisioned semantic-event protocol performs that
work.

## Source Maps and Lowering

`parse_nui_flow` returns a `NuiFlowDocument` containing the original source,
stable-key-to-source-span map, canonical IR, and input schema. `lower_nui_flow`
returns the canonical IR without I/O or GPU work. `format_nui_flow` parses then
emits the supported subset in stable form. Parse diagnostics contain a stable
code, severity, source span, message, and optional suggestion.

`compile_nui_flow_program` invokes the same portable `UiProgram` compiler used
by JSON IR and copies stable-key source spans into program dependency/debug
records. There is no Flow-only compiler or evaluator behavior.

Stable node keys are declaration identities. They are distinct from opt-in
domain values, source spans, renderer-local hit IDs, and GPU instance indices.
Only stable keys and source spans may appear in authoring/debug output.

## Patches

Patch documents begin with `@ revision <n>`. Operations target stable keys or
semantic paths, never array indexes:

```text
@ revision 12
~ water enabled false
+ inspector text warning
- obsolete_row
> warning inspector
```

`+ <parent> <component> <key>` inserts a closed-vocabulary node. `- <key>`
removes a non-root node. `~ <key> <property> <value>` supports `enabled`,
`visible`, `value`, `w`, and `h`. `> <key> <parent>` moves a non-root node.
`parse_nui_flow_patch` produces a revisioned `UiIrPatch`; `apply_nui_ir_patch`
is deterministic and can be used for dry-run diagnostics. A successful patch
returns a new canonical document revision. Stale revisions, unknown keys,
duplicate insert keys, root removal, invalid moves, and invalid properties are
rejected explicitly.

## Compatibility

Flow V1 is additive to the existing `UiFragment`/JSON IR compatibility path.
It does not replace React declaration clients, add a scripting runtime, or
permit a Flow document to bypass UI-runtime validation. New syntax or component
semantics require a new Flow version and an explicitly negotiated UI capability.
