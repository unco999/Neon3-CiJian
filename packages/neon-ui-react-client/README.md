# neon-ui-react-client

React is used here as a declaration authoring runtime. It does not render DOM,
Canvas, or GPU pixels. The client reconciles JSX into Neon3 `UiFragment`
declarations and submits them through the public RPC protocol. Final pixels are
still owned by `neon-wgpu-runtime`.

## Identity rules

- `key` is React's local reconciliation key and is never serialized.
- `surfaceId` is the stable UI surface identity and becomes `fragment_id`.
- `nodeKey` is a stable sibling-local declaration key. It must be semantic,
  non-numeric, and unique among siblings.
- The compiler hashes `surfaceId` plus the declaration path into `UiNodeId`.
- Domain object IDs belong only in typed intent parameters or `AssetRef`; they
  never become render IDs.
- `id`, `elementId`, `targetId`, coordinate callbacks, and `onClick` are not
  part of this API.

## Example

```tsx
import { Button, Label, Panel, Surface, createNeonRoot } from "neon-ui-react-client";

const root = createNeonRoot({
  submit: (fragment) => client.submitFragment(fragment),
  onError: console.error,
});

root.render(
  <Surface surfaceId="surface.terrain.inspector" revision={1}
    bounds={{ x: 24, y: 24, width: 420, height: 240 }}>
    <Panel nodeKey="header" bounds={{ x: 16, y: 16, width: 388, height: 40 }}>
      <Label nodeKey="title" bounds={{ x: 0, y: 0, width: 240, height: 28 }}>
        Terrain Inspector
      </Label>
      <Button nodeKey="water-tool" bounds={{ x: 250, y: 0, width: 138, height: 34 }}
        intent={{ action: "terrain.tool.select", params: { tool: "water_inject" } }}>
        Water tool
      </Button>
    </Panel>
  </Surface>,
);
```

## Complex Case

The package includes `TerrainEditorSurface`, a snapshot-driven editor workbench
with a top command bar, tool rail, viewport, resource binding inspector,
material rows, selection controls, and diagnostics. It deliberately uses a
stable `rowKey` from the UI view model for repeated rows; terrain IDs and asset
IDs remain intent parameters or `AssetRef` values.

```tsx
import { TerrainEditorSurface, createNeonRoot } from "neon-ui-react-client";

const root = createNeonRoot({
  submit: (fragment) => client.submitFragment(fragment),
});

root.render(<TerrainEditorSurface snapshot={terrainEditorSnapshot} />);
```

`terrainEditorSnapshot` is rebuilt from revisioned terrain/resource/project
snapshots. React does not decide whether a material is needed or whether water
mode is valid; those decisions arrive in the snapshot and intents are sent to
the owning runtime.
