# Neon3 GPU-Reactive UI Program Contract

## Status and Compatibility

This is the stage-001 baseline for `ui.program.v1`. It supplements, and does not
replace, the existing `UiFragment` v1 compatibility declaration. A client uses
the program contract only after capability negotiation; existing fragment
submissions keep their serialized form and behavior. The capability is initially
experimental. A later supported version must remain explicitly versioned and
must not silently reinterpret a program, input schema, text handle, or event.

`UiIrDocument` remains the canonical persisted UI declaration. NUI Flow is an
authoring and patch notation that parses and lowers to the versioned JSON IR; it
is never the sole persisted representation. `UiIrPatch` is the only declarative
topology mutation representation. Neither is executable code.

## Ownership

`neon-ui-runtime` owns UI IR validation and lowering, program validation,
allowed intent policy, declared input schemas, resolved external inputs, CPU
evaluation, and semantic-event validation. Its CPU backend is a supported
headless product backend and has no WGPU or window dependency.

`neon-wgpu-runtime` is the only owner of windows, physical targets, WGPU
resources, render hit testing, pointer capture, local visual prediction, glyph
residency, renderer-local interaction state, GPU program buffers, composition,
and final pixels. It may execute only the declared presentation subset of an
accepted program. It does not compute domain facts or mutate persistent domain
state.

Domain runtimes own business facts, permissions, validation, formatted display
strings, rows, sort/filter results, resource availability, and command
eligibility. They supply typed, revisioned read-only inputs. A UI declaration
cannot infer a domain condition or change domain state.

## Allowed and Forbidden Computation

CPU and GPU presentation evaluation may propagate direct bindings, select
precompiled branches, select bounded template instances, arrange flex/grid/
overlay layouts, propagate clips, place text from resident metrics, generate
caret/selection/scroll geometry, advance renderer-owned named presentation
state, and generate bounded instances.

It may not perform I/O, file or network access, arbitrary formatting, locale
conversion, project writes, durable mutation, unbounded allocation, recursion,
dynamic code execution, shader compilation, resource loading, or business
calculation. NUI Flow admits no code blocks, braces, square brackets, functions,
loops, assignments, callbacks, arbitrary predicates, expressions, URLs, file
paths, shader source, GPU handles, or pointer coordinates.

## Identity and Coordinates

An IR node key is a stable semantic declaration identity and is sibling/path
validated. It is not a domain entity ID. A domain ID is an opt-in typed data
value supplied by a domain owner and never becomes node identity. A source span
identifies the authoring source location. These three values may appear in
semantic inspection APIs.

A renderer-local hit ID is an opaque WGPU-runtime-only input implementation
detail. A GPU instance index is a transient renderer allocation index. Neither
may appear in UI IR, patches, semantic events, inputs, diagnostics exposed
cross-process, project records, or domain protocols.

All declarations, source maps, outlines, layout snapshots, and diagnostics use
logical UI units. `neon-wgpu-runtime` alone converts logical layout to physical
pixels at the raster boundary. Physical coordinates, target dimensions, and hit
IDs remain renderer-private.

## Program Lifecycle

1. An author submits a versioned `UiIrDocument`, optionally authored as NUI
   Flow, plus declared theme/resource contracts.
2. UI Runtime validates and deterministically lowers it into an immutable
   `UiProgram` with static node topology, fixed component kinds, stable keys,
   bindings, events, optional preallocated branches/templates, and a bounded
   resource budget.
3. Activation validates `UiProgramRevision`, schema version, and negotiated
   capabilities, then installs every `UiInputSchema` default before rendering.
4. CPU consumers evaluate the active program from resolved inputs. WGPU Runtime
   uploads only a fully validated revision and makes it visible atomically at a
   frame boundary.
5. Structural change requires a revisioned `UiIrPatch` dry-run and accepted new
   program revision. A running program cannot create nodes or change component
   kinds.

## Input and Text Lifecycle

`UiInputSchema` declares stable `UiInputSlot` keys, types, explicit defaults,
update policy, documentation, provenance, and packing metadata. `UiInputFrame`
is a sparse, revisioned external write; it carries request and idempotency
identity. `UiResolvedInputs` always contains every slot because defaults are
installed at activation. Stale, malformed, or unauthorized writes are rejected
or explicitly partially rejected, never silently applied.

Dynamic text crosses the boundary as `UiTextHandle` plus registry generation.
`UiTextRegistryRevision` owns bounded records, handle lifetimes, replacement,
residency status, and missing/stale diagnostics. Literal text compiles into the
immutable program text table. The domain or an approved presentation adapter
formats user-facing values before creating a text record; GPU code only places
and clips glyphs.

## Bounded Structure and Events

`UiBinding` is a direct typed property binding only. `UiBranch` selects a
precompiled subtree with a direct boolean or enum-equality input predicate.
`UiTemplate` selects from a preallocated bounded instance range. Both the
topology and resource allocation remain fixed after compilation.

`UiSemanticEvent` carries a declared intent, semantic node key, approved typed
payload values, interaction metadata, program/input revisions, request ID, and
idempotency key. Local hover, pressed, capture, caret, selection, and scroll
preview remain inside WGPU Runtime. Domain-affecting action is accepted only by
the receiving authority and its resulting value returns through a new resolved
input revision.

### Program Semantic Event Boundary

`ui.program.semantic_event.v1` uses `UiProgramSemanticEvent` for the compiled
program path. Each declaration names a stable semantic node key and intent, then
lists exact literal payload values and/or direct bound input keys. The emitted
payload must exactly equal those declared values resolved from the accepted
input snapshot. Arbitrary JSON, raw text, coordinates, renderer hit IDs, and
GPU identifiers are rejected. Text edit commits carry a `UiTextHandle`; the
text registry remains responsible for bounded text storage and validation.

The UI runtime validates program and input revision, renderer epoch, source
node, declared intent, effective visible/enabled state, payload shape, and
idempotency key before an event is routed to a domain authority. Replaying an
idempotency key returns the original result as a duplicate rather than applying
the intent twice. A controlled control may display renderer-local tentative
feedback, but only a later reliable external input frame is authoritative;
rejection or cancellation clears that local prediction.

`UiEventTraceRecord` exposes event identity, semantic key, intent, revisions,
epoch, result code, and timestamp. It never exposes physical positions, render
hit IDs, selection rectangles, native handles, or GPU state.

## Capacity and Diagnostics

`UiResourceBudget` declares finite limits for nodes, bindings, branch/template
instances, text records, glyph instances, clips, and events. A capacity limit
must have a declared fallback or rejection behavior. Overflow creates a stable,
machine-readable diagnostic and must never discard content or samples silently.

`UiDiagnostic` has stable code, `info`, `warning`, or `error` severity, readable
message, optional semantic node/input key and source span, and applicable
revision. Diagnostics may expose logical data and semantic identities only;
they never expose GPU memory, native window handles, render hit IDs, or physical
pixel coordinates.

## Baseline Failure Codes

| Code | Meaning |
| --- | --- |
| `ui_program_unsupported_schema` | Program or input-schema version is not supported. |
| `ui_program_unsupported_capability` | A required capability/version was not negotiated. |
| `ui_program_duplicate_input_key` | Input schema repeats a stable slot key. |
| `ui_program_invalid_default` | A slot default is absent, non-finite, or incompatible. |
| `ui_program_input_type_mismatch` | An input frame value does not match its declared slot type. |
| `ui_program_stale_input_revision` | Input frame expected revision is no longer current. |
| `ui_program_unknown_text_handle` | A referenced text handle is unavailable. |
| `ui_program_text_registry_generation_mismatch` | A handle generation is stale. |
| `ui_program_unknown_binding_target` | A binding references no declared input/node/property target. |
| `ui_program_capacity_overflow` | A declared program or runtime capacity was exceeded. |
| `ui_program_invalid_branch_template` | A branch/template is not static, bounded, or directly typed. |
| `ui_program_forbidden_flow_feature` | NUI Flow attempted a scripting or other forbidden feature. |
| `ui_program_event_stale_revision` | An event targets an outdated program or input snapshot. |
| `ui_program_event_invalid_source` | The semantic source node or declared intent is invalid. |
| `ui_program_event_control_unavailable` | The source control is hidden or disabled. |
| `ui_program_event_payload_rejected` | Payload values differ from the typed declaration. |
| `ui_program_event_duplicate_idempotency_key` | A repeated event key is recorded as a duplicate result. |
| `ui_program_event_interaction_epoch_mismatch` | The event belongs to a previous renderer epoch. |

Future revisions may add failure codes but must not repurpose these meanings.
They must include the associated request, program/input revision, and semantic
context where that context exists.

## Glossary

- `UiIrDocument`: canonical versioned declarative UI document.
- `UiIrPatch`: revisioned declarative update addressed by stable semantic keys.
- `UiProgram`: validated immutable compiled declaration with static topology.
- `UiProgramRevision`: program identity, revision, schema version, capabilities.
- `UiInputSchema`: typed, defaulted slot declarations for one program.
- `UiInputSlot`: one stable external or local presentation value declaration.
- `UiInputFrame`: sparse revisioned input changes with request identity.
- `UiResolvedInputs`: full effective values after defaults and accepted frames.
- `UiTextHandle`: opaque stable text-record identifier plus generation.
- `UiTextRegistryRevision`: bounded text registry state/version.
- `UiBinding`: direct typed input-to-property dependency.
- `UiBranch`: precompiled conditional subtree selection record.
- `UiTemplate`: precompiled bounded repeated-instance declaration.
- `UiResourceBudget`: declared finite program/runtime capacity contract.
- `UiSemanticEvent`: typed revisioned intent emitted from a semantic node.
- `UiDiagnostic`: stable program/input/layout/event diagnostic record.
