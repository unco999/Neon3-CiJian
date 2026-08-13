# Neon3 UI Platform Roadmap

This decision record is governed by the `D:\goal` driver configuration, which must
reference only plans under `D:\Neon3\plan`, together with `D:\Neon3\AGENTS.md`.
Construction follows U0 through U10 in dependency order.

## U0 Baseline

The current compatibility declaration is `UiFragment` with `Panel`, `Label`, and
`Button` nodes, logical `UiBounds`, sparse `UiStyle`, and optional entry
transitions. Existing static fragments remain supported while later milestones
add versioned capabilities; this record does not change their serialized form.

`UiNodeId` is the current compatibility name for a fragment-local declaration
identity. It is not a former element ID, a domain identifier, or an input-event
field. The target `UiNodeKey` is the explicit successor term for that same
fragment-local reconciliation role. The WGPU runtime may convert a validated
local declaration identity into an opaque `RenderHitId`, but neither value may
cross from renderer input handling into a UI semantic event, intent, project
record, or terrain/resource/project protocol.

The renderer advertises supported UI features through `service.describe` and
`debug.snapshot.get`. Clients must gate optional behavior on those capabilities
instead of assuming a complete UI platform. The U0 baseline currently advertises
`wgpu.ui.fragment.v1` and `wgpu.render.diagnostics`; later milestones add their
own versioned capability only after their contract tests pass.

## Construction Order

U0 establishes this inventory. U1 establishes semantic input before U2 creates
the final UI hit target. U3 through U10 then follow the dependency order in the
governing construction plan. No compatibility shell permits a raw render ID,
node key, pixel coordinate, or GPU handle in a domain protocol.
