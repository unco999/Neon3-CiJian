# Neon3 Bevy NUI Host Case

This is the first external-host case for the Neon3 UI protocol.

Target engine version: Bevy `0.19.1`.

The case is intentionally split into three planes:

```text
Bevy ECS
  character entity, movement, camera, stable host object IDs

Neon3 control plane
  surface open, camera snapshots, semantic intent forwarding

Neon3 render plane
  color target + ID target with the same requested size and frame generation
```

The NUI document at `assets/ui/character-status.nui` describes the intended overhead
status component: avatar, static name label, level, health bar, mana bar, and status text. The
`.glb` character asset is intentionally not committed; place a compatible
`assets/character.glb` file there before running the visual case.

## Build

```powershell
cargo check --manifest-path D:\Neon3\cases\bevy-nui-host\Cargo.toml
cargo test --manifest-path D:\Neon3\cases\bevy-nui-host\Cargo.toml
```

The current case compiles the thin Bevy adapter. NUI contents remain runtime data;
the adapter does not parse or reimplement NUI.

## Target contract

The host requests two same-size targets:

```text
case.bevy.screen.ui.color : rgba8unorm
case.bevy.screen.ui.id    : r32uint
```

The color target is displayed by the host. The ID target is sampled by the host's
input path and submitted as a revisioned `HostUiPointerClick`. Neon resolves the
target generation and frame sequence to a declared semantic intent. Raw renderer
IDs never become ECS or gameplay identities.

## NUI Flow variables

The case uses `nui_flow_vars!` for typed ECS-to-UI state:

```text
CharacterStatusVars
  health: f32 -> health
  mana: f32   -> mana
  level: u32  -> level
```

The first frame is complete. Later frames contain only changed fields and are sent
to UI Runtime through `ui.input.frame`. A Flow variable marked `emitevent` is then
published by UI Runtime to eventd using the existing `flow.<flow>.<variable>` event
contract.

The current case intentionally keeps the character name as static NUI text until
the text-registry upload path is connected; it does not fabricate a `UiTextHandle`.

If `eventd_endpoint` is configured, the adapter subscribes to `flow.` events and
stores them in `Neon3VariableEvents`. These are observations for diagnostics and
host synchronization, not gameplay commands and not automatic ECS mutations.

The Bevy `RenderApp` now owns a `Neon3ExternalSurfaceGpu` render resource with
matching color/id formats and frame identity. Native D3D12 handle import and the
handle acquisition and color fullscreen overlay are now wired. The Windows helper uses Bevy's own
`wgpu 29.0.4 / wgpu-hal 29.0.4` types to call `OpenSharedHandle` and wrap the
resource without copying pixels. The remaining consumer work is fence wait,
ID target import/readback, and automatic pointer-click submission.

## World and camera synchronization

The case uses the existing Neon3 world bridge instead of a Bevy-specific camera
protocol:

```text
startup: wgpu.world.info.configure
runtime: wgpu.world.camera.submit_frame
```

Each camera frame carries the world space ID, producer epoch, monotonic sequence,
position, quaternion orientation, FOV, near plane, and far plane. Neon rejects
wrong-world and stale frames. A World UI declaration must use the same world space
and camera identity before it becomes visible.
