# Bevy NUI Runtime Integration Plan

## 1. Purpose

This document defines the integration contract between the Bevy host and the
Neon UI Runtime.

The target architecture is:

```text
Bevy
  ECS state, world entities, camera, anchors, raw pointer events

Neon UI Runtime
  NUI parsing, layout, projection, hit testing, semantic interaction

Shared render plane
  color texture, producer/consumer fences, frame identity
```

Bevy must not interpret renderer IDs or reconstruct UI semantics from pixels.
The UI Runtime owns hit testing and returns semantic intents.

## 2. Current Bevy Implementation

The Bevy adapter currently provides:

- Persistent RPC connections, separated by endpoint.
- Automatic RPC reconnect and one retry for idempotent requests.
- Input revision advancement from accepted `ui.input.frame` responses.
- World camera frame submission.
- World anchor submission.
- Direct external color/depth texture binding on the render path.
- Raw pointer event collection and transport.
- Zero-sized semantic interaction key components.
- `NeonWorldUi<V>` for world-space UI state.
- `NeonScreenUi<V>` for fixed screen UI state.
- `Neon3SemanticIntentEvents` for runtime-to-ECS semantic feedback.

The current Bevy semantic tags are:

```rust
CharacterStatusScreenKey -> "character.player.main.status"
MonsterStatusWorldKey   -> "monster.status"
```

The current implementation no longer requests or imports a UI ID target.

## 3. Ownership Rules

### Bevy owns

- ECS entities and components.
- Gameplay state.
- `NeonWorldUi<V>` values.
- `NeonScreenUi<V>` values.
- Camera transform and projection.
- World anchor positions.
- Raw mouse events.
- Semantic intent consumption and gameplay reactions.

### UI Runtime owns

- NUI document parsing and compilation.
- UI layout and visual node geometry.
- Pointer hit testing.
- Hover, click, double-click, context menu, drag, and pointer capture state.
- Mapping from UI nodes to semantic interaction keys.
- Semantic intent generation.

### Shared contract owns

- Surface identity.
- Program revision.
- Input revision.
- Surface generation.
- Render frame sequence.
- Producer epoch.
- Pointer event sequence.

## 4. Render Surface Contract

The screen surface requests one target only:

```text
surface_id: case.bevy.screen.ui
target_id:  case.bevy.screen.ui.color
format:     rgba8unorm
```

The surface must not expose:

- `r32uint` ID targets.
- ID texture handles.
- ID fences.
- ID readback requests.
- Renderer ID values in ECS or gameplay messages.

The surface response must continue to provide:

- `generation`.
- Per-buffer color texture handle.
- Per-buffer color producer fence handle.
- Per-buffer color consumer-release fence handle.
- Optional depth handles when depth output is enabled.

## 5. Raw Pointer Event Contract

Bevy sends raw pointer events through the existing `ui.host.inbound` RPC:

```json
{
  "kind": "pointer_event",
  "event": {
    "event_type": "enter",
    "surface_id": "case.bevy.screen.ui",
    "pixel": [640, 360],
    "delta": [0.0, 0.0],
    "delta_mode": "pixel",
    "button": null,
    "buttons": [],
    "modifiers": ["shift"],
    "pointer_id": 0,
    "sequence": 12,
    "generation": 3,
    "frame_sequence": 91,
    "timestamp_monotonic_ns": 123456789
  }
}
```

### Event types

The runtime must accept these event types:

| Event | Meaning |
| --- | --- |
| `enter` | Pointer entered the host window/surface. |
| `leave` | Pointer left the host window/surface. |
| `move` | Pointer position changed. Buttons indicate drag state. |
| `down` | A mouse button was pressed. |
| `up` | A mouse button was released. |
| `wheel` | Wheel or trackpad scroll event. |
| `cancel` | Focus was lost or the pointer interaction was cancelled. |

The runtime derives higher-level events such as `click`, `double_click`,
`context_menu`, and `drag` from the raw sequence.

### Coordinate rules

- `pixel` is surface-local, top-left origin.
- X range is `[0, surface_width)`.
- Y range is `[0, surface_height)`.
- Bevy maps window logical coordinates into the requested surface size.
- The runtime must not reinterpret the coordinates as normalized values.
- `wheel` preserves its original `delta_mode`.

### Sequence rules

- `sequence` is strictly increasing for one Bevy session.
- `pointer_id` is `0` for the current mouse device.
- `generation` identifies the acquired surface generation.
- `frame_sequence` identifies the latest frame known by Bevy when the event was captured.
- The runtime may reject stale generation values.
- The runtime may accept a slightly stale frame sequence if the surface generation is valid.

## 6. Runtime Hit Testing

For every pointer event, the runtime performs hit testing against its current
layout state.

The runtime must use:

- Surface ID.
- Current program revision.
- Current layout tree.
- Current world camera frame, for world UI.
- Current world anchor frame, for world UI.
- Surface-local pointer coordinates.

The runtime must not require Bevy to provide an ID pixel.

### Fixed screen UI

Fixed screen UI is hit-tested directly in surface coordinates.

Example:

```text
pointer pixel -> screen layout node -> semantic interaction key
```

### World UI

World UI is hit-tested after the runtime projects the anchor using the latest
camera and anchor submissions.

Example:

```text
pointer pixel
  -> projected world panel bounds
  -> repeat row / stable anchor
  -> semantic interaction key
```

## 7. Semantic Interaction Keys

Semantic interaction keys are stable labels. They are not renderer IDs and do
not carry per-entity data.

Bevy represents them with zero-sized ECS components:

```rust
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct MonsterStatusWorldKey;
```

The type maps to a stable string through the shared key contract:

```text
MonsterStatusWorldKey -> monster.status
CharacterStatusScreenKey -> character.player.main.status
```

The runtime must return the same key in semantic feedback.

Per-entity identity must be in the semantic payload, not in the key:

```text
interaction_key: monster.status
payload.anchor: monster.m0
payload.object_id: monster.m0
```

This keeps the key reusable across all rows while preserving entity identity.

## 8. Semantic Intent Feedback

The preferred response is an accepted pointer RPC response containing:

```json
{
  "status": "accepted",
  "result": {
    "semantic_intent": {
      "interaction_key": "monster.status",
      "source_node_key": "monster.m0.status",
      "intent": "character.open_status",
      "payload": {
        "anchor": "monster.m0",
        "object_id": "monster.m0"
      }
    }
  }
}
```

The runtime may also publish the same object asynchronously through eventd.
If asynchronous delivery is used, the event name must be documented and must
be routed into the same Bevy `Neon3SemanticIntentEvents` resource.

Bevy stores:

- `intent`.
- `interaction_key`.
- `source_node_key`.
- `payload`.
- Originating request ID.

Bevy gameplay systems consume the resource and query their target component:

```rust
if semantic_intent_targets::<MonsterStatusWorldKey>(&event) {
    // Resolve payload.anchor or payload.object_id to an ECS entity.
}
```

The runtime must never return a raw GPU renderer ID as the only identity.

## 9. Fixed Screen UI Component

`NeonScreenUi<V>` represents a screen-fixed UI state in ECS:

```rust
NeonScreenUi<CharacterStatusVars>
CharacterStatusScreenKey
```

It contains:

- Flow name.
- Typed variables.
- Program/input identity.
- Last sent state.
- Visibility.
- Currently selected object, when a world interaction opens the screen UI.

It does not contain:

- A renderer ID.
- A texture coordinate identity.
- A GPU handle.
- A runtime-specific node integer.

When a world semantic intent targets the screen UI, a gameplay system may:

1. Query `With<CharacterStatusScreenKey>`.
2. Set `visible = true`.
3. Set `selected_object` from the intent payload.
4. Update typed variables.
5. Let the normal `ui.input.frame` flow publish the changes.

## 10. World UI Component

`NeonWorldUi<V>` remains the world-space state component.

Its stable per-instance identity is:

```text
anchor = monster.m0
```

The associated semantic label is provided by a zero-sized ECS tag such as:

```rust
MonsterStatusWorldKey
```

The runtime must preserve the repeat row key or anchor in semantic payloads so
Bevy can resolve the feedback to the correct entity.

## 11. Flow and Variable Synchronization

### Initial flow submission

Bevy submits the NUI source through:

```text
ui.flow.submit
```

The response must include the active `UiProgramRevision`.

Bevy updates both the bridge identity and the corresponding screen component
identity.

### Variable input

Bevy sends:

```text
ui.input.frame
```

Every frame includes:

- Program revision.
- Expected input revision.
- Request ID.
- Idempotency key.
- Typed changes.

Accepted responses must return one of:

```text
input_revision
accepted_input_revision
```

Bevy advances its expected revision only from an accepted response and never
allows a delayed response to move the revision backwards.

Rejected frames must include enough information for the host to retry against
the current revision.

## 12. Camera and Anchor Synchronization

The runtime continues to receive:

```text
wgpu.world.info.configure
wgpu.world.camera.submit_frame
wgpu.world.ui.anchor.submit
```

Each frame must carry:

- World space ID.
- Camera ID where applicable.
- Producer epoch.
- Monotonic sequence.
- Monotonic timestamp.
- Position/orientation or anchor position.

The runtime must reject stale producer epochs and stale sequences.

### Producer epoch

`producer_epoch` must no longer be permanently hardcoded to `1`.

The runtime should return or negotiate the active epoch when the surface/world
session is opened. Bevy then uses that epoch for all camera and anchor frames.

When the runtime restarts, a new epoch invalidates frames from the old session.

## 13. Anchor Batching

The current Bevy adapter sends one RPC per changed anchor. This is acceptable as
a temporary compatibility path but is not the final performance design.

The preferred runtime method is:

```text
wgpu.world.ui.anchor.submit_batch
```

Example payload:

```json
{
  "world_space_id": "case.bevy.world.main",
  "producer_epoch": 4,
  "sequence": 120,
  "timestamp_monotonic_ns": 123456789,
  "anchors": [
    {
      "anchor_id": "monster.m0",
      "position": [1.0, 2.0, 3.0],
      "billboard": true,
      "occlusion": "depth_tested"
    }
  ]
}
```

The runtime must treat the batch as one revisioned snapshot. Bevy may drop
older unsent batches and keep only the newest batch.

## 14. RPC Backpressure

High-frequency messages must not accumulate without bound.

The following messages are latest-value data:

wgpu.world.camera.submit_frame
wgpu.world.ui.anchor.submit
wgpu.world.ui.anchor.submit_batch
```

The transport should coalesce unsent messages by endpoint and logical identity.
Pointer events are ordered input and must not be coalesced across button state
transitions. Pointer move events may be coalesced only when no button transition
or wheel event is between them.

## 15. Runtime Implementation Phases

### Phase A: Pointer ingress

- Add `kind = pointer_event` handling to `ui.host.inbound`.
- Validate surface ID and generation.
- Validate pointer sequence ordering.
- Convert surface pixel to runtime hit-test coordinates.
- Store pointer state per pointer ID.
- Implement enter/leave/move/down/up/wheel/cancel.

### Phase B: Fixed screen hit testing

- Hit-test ordinary screen UI nodes.
- Resolve the target to a semantic interaction key.
- Return a semantic intent response.
- Add click, double-click, context-menu, and pointer-capture behavior.

### Phase C: World UI hit testing

- Consume Bevy camera frames.
- Consume Bevy anchor frames.
- Project world panels into the screen surface.
- Hit-test repeat rows.
- Return anchor/object identity in payload.

### Phase D: Bevy semantic reaction

- Consume semantic intent responses/events.
- Match the interaction key to ECS tag components.
- Resolve payload anchor/object ID to an entity.
- Update `NeonScreenUi<V>` or gameplay state.
- Publish resulting typed flow changes.

### Phase E: Revision and restart handling

- Negotiate producer epoch.
- Return input revision consistently.
- Reject stale program/generation/epoch/frame data.
- Confirm reconnect and retry behavior.

### Phase F: Performance protocol

- Add batch anchor submission.
- Add latest-value coalescing.
- Measure pointer-to-intent latency.
- Measure world UI projection cost.
- Confirm no ID target allocation or readback remains.

## 16. Acceptance Tests

### Pointer tests

- Move over a screen node produces enter and move.
- Leaving the window produces leave.
- Button press/release produces down/up with correct button state.
- Right click produces a semantic context-menu intent.
- Double click is detected by runtime timing and position rules.
- Wheel preserves line/pixel delta mode.
- Focus loss produces cancel.
- Pointer sequences are rejected when stale or duplicated.

### Fixed screen UI tests

- A pointer event over the status panel resolves to `character.player.main.status`.
- The returned intent reaches `Neon3SemanticIntentEvents`.
- `With<CharacterStatusScreenKey>` finds the screen UI entity.
- Selected object data updates the screen component.
- Typed variable changes are sent with the current input revision.

### World UI tests

- Anchor `monster.m0` projects to the expected screen region.
- A pointer event in that region returns `monster.status`.
- The payload contains `monster.m0`.
- Bevy resolves the payload to the correct ECS entity.
- The screen UI updates without any renderer ID.

### Restart tests

- Runtime restart changes producer epoch.
- Old camera and anchor frames are rejected.
- Bevy reacquires the surface generation.
- Pointer events use the new generation.
- Input revision is resynchronized.

### Performance tests

- No ID texture is requested or imported.
- No ID fence is created or waited on.
- No full-frame ID copy exists.
- Anchor updates can be batched.
- Backpressure does not create unbounded camera/anchor queues.

## 17. Definition Of Done

The integration is complete when:

- Runtime accepts all Bevy raw pointer event types.
- Runtime performs hit testing without an ID texture.
- Runtime returns semantic intents with stable interaction keys.
- Bevy maps intents to zero-sized ECS tag components.
- World interaction can update a fixed screen UI component.
- Input revisions remain synchronized over long sessions.
- Producer epoch and surface generation survive runtime restart.
- World anchors support a batched submission path.
- All ID target code and documentation remain removed.
- The pointer, fixed-screen, world-screen, restart, and performance tests pass.
