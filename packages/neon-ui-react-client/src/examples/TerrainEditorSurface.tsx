import type { ReactNode } from "react";
import { Button, Label, Panel, Surface } from "../components.js";
import type { Bounds, JsonValue, NodeKey, Revision, UiIntentSpec, UiStyle } from "../protocol.js";

export type TerrainToolSnapshot = {
  mode: "select" | "raise" | "lower" | "smooth" | "water_inject";
  label: string;
  active: boolean;
  rowKey: NodeKey;
};

export type TerrainMaterialSnapshot = {
  rowKey: NodeKey;
  label: string;
  assetRevision: Revision;
  state: "unbound" | "loading" | "ready" | "failed";
};

export type TerrainEditorSnapshot = {
  projectRevision: Revision;
  terrainId: number;
  terrainName: string;
  mode: "terrain" | "water_paint";
  bindingState: "none" | "needs_selection" | "loading" | "ready" | "failed";
  tools: TerrainToolSnapshot[];
  materials: TerrainMaterialSnapshot[];
  history: Array<{ rowKey: NodeKey; label: string; revision: Revision; selected: boolean }>;
  diagnostics: Array<{ rowKey: NodeKey; label: string; value: string; tone: "normal" | "warning" | "error" }>;
};

export type TerrainEditorSurfaceProps = {
  snapshot: TerrainEditorSnapshot;
  onIntent?: (intent: UiIntentSpec) => void;
};

const clear: UiStyle = { background_color: [0, 0, 0, 0], border_width: 0, opacity: 1 };
const shell: UiStyle = { background_color: [0.025, 0.04, 0.055, 0.98], border_color: [0.12, 0.3, 0.36, 0.95], border_width: 1, corner_radius: 6, opacity: 1 };
const panel: UiStyle = { background_color: [0.045, 0.07, 0.085, 0.98], border_color: [0.12, 0.25, 0.29, 0.95], border_width: 1, corner_radius: 4, opacity: 1 };
const mutedPanel: UiStyle = { background_color: [0.03, 0.05, 0.06, 0.98], border_color: [0.08, 0.17, 0.2, 0.95], border_width: 1, corner_radius: 3, opacity: 1 };
const accent: UiStyle = { background_color: [0.06, 0.32, 0.38, 1], border_color: [0.35, 0.9, 0.88, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const warning: UiStyle = { background_color: [0.32, 0.22, 0.06, 0.98], border_color: [0.95, 0.65, 0.2, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const heading: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };

function action(action: string, params: JsonValue): UiIntentSpec {
  return { action, params };
}

function Text({ nodeKey, bounds, children, style = clear }: { nodeKey: NodeKey; bounds: Bounds; children: ReactNode; style?: UiStyle }) {
  return <Label nodeKey={nodeKey} bounds={bounds} style={style}>{children}</Label>;
}

function Section({ nodeKey, bounds, title, children }: { nodeKey: NodeKey; bounds: Bounds; title: string; children: ReactNode }) {
  return (
    <Panel nodeKey={nodeKey} bounds={bounds} style={panel}>
      <Text nodeKey="section-title" bounds={{ x: 10, y: 8, width: bounds.width - 20, height: 20 }}>{title}</Text>
      {children}
    </Panel>
  );
}

export function TerrainEditorSurface({ snapshot }: TerrainEditorSurfaceProps) {
  const viewportIntent = action("terrain.viewport.focus", { terrain_id: snapshot.terrainId });
  return (
    <Surface
      surfaceId="surface.editor.terrain-workbench"
      revision={snapshot.projectRevision}
      bounds={{ x: 0, y: 0, width: 1440, height: 900 }}
      style={shell}
    >
      <Panel nodeKey="topbar" bounds={{ x: 0, y: 0, width: 1440, height: 54 }} style={panel}>
        <Text nodeKey="brand" bounds={{ x: 18, y: 12, width: 170, height: 26 }} style={heading}>NEON / TERRAIN</Text>
        <Text nodeKey="project-name" bounds={{ x: 210, y: 14, width: 260, height: 22 }}>{snapshot.terrainName}</Text>
        <Button nodeKey="save-project" bounds={{ x: 1090, y: 10, width: 96, height: 32 }} style={accent} intent={action("project.transaction.commit", { project_revision: snapshot.projectRevision })}>Commit</Button>
        <Button nodeKey="open-diagnostics" bounds={{ x: 1196, y: 10, width: 112, height: 32 }} style={mutedPanel} intent={action("debug.diagnostics.open", { context: "terrain" })}>Diagnostics</Button>
        <Button nodeKey="close-workbench" bounds={{ x: 1320, y: 10, width: 100, height: 32 }} style={mutedPanel} intent={action("ui.surface.close", { surface: "terrain-workbench" })}>Close</Button>
      </Panel>

      <Panel nodeKey="left-rail" bounds={{ x: 12, y: 66, width: 228, height: 710 }} style={mutedPanel}>
        <Text nodeKey="tools-title" bounds={{ x: 12, y: 12, width: 190, height: 20 }} style={heading}>TERRAIN TOOLS</Text>
        {snapshot.tools.map((tool, index) => (
          <Button
            key={tool.rowKey}
            nodeKey={tool.rowKey}
            bounds={{ x: 12, y: 44 + index * 42, width: 204, height: 34 }}
            style={tool.active ? accent : clear}
            intent={action("terrain.tool.select", { terrain_id: snapshot.terrainId, tool: tool.mode })}
          >
            {tool.label}
          </Button>
        ))}
        <Text nodeKey="brush-title" bounds={{ x: 12, y: 278, width: 190, height: 20 }} style={heading}>BRUSH STATE</Text>
        <Panel nodeKey="brush-state" bounds={{ x: 12, y: 308, width: 204, height: 96 }} style={panel}>
          <Text nodeKey="brush-radius" bounds={{ x: 10, y: 10, width: 184, height: 20 }}>Radius                 32.0 m</Text>
          <Text nodeKey="brush-strength" bounds={{ x: 10, y: 34, width: 184, height: 20 }}>Strength              0.65</Text>
          <Text nodeKey="brush-channel" bounds={{ x: 10, y: 58, width: 184, height: 20 }}>Channel                height</Text>
        </Panel>
        <Button nodeKey="brush-reset" bounds={{ x: 12, y: 420, width: 204, height: 32 }} style={mutedPanel} intent={action("terrain.brush.reset", { terrain_id: snapshot.terrainId })}>Reset brush</Button>
      </Panel>

      <Panel nodeKey="viewport" bounds={{ x: 252, y: 66, width: 748, height: 710 }} style={{ background_color: [0.015, 0.025, 0.03, 1], border_color: [0.15, 0.38, 0.42, 1], border_width: 1, corner_radius: 4, opacity: 1 }}>
        <Button nodeKey="viewport-focus" bounds={{ x: 16, y: 14, width: 126, height: 30 }} style={mutedPanel} intent={viewportIntent}>Focus terrain</Button>
        <Text nodeKey="viewport-mode" bounds={{ x: 520, y: 18, width: 210, height: 20 }} style={snapshot.mode === "water_paint" ? warning : heading}>{snapshot.mode === "water_paint" ? "WATER PAINT / PREVIEW" : "TERRAIN / EDIT"}</Text>
        <Panel nodeKey="viewport-crosshair" bounds={{ x: 318, y: 300, width: 110, height: 110 }} style={{ background_color: [0.04, 0.18, 0.2, 0.5], border_color: [0.24, 0.75, 0.72, 0.7], border_width: 1, corner_radius: 55, opacity: 1 }}>
          <Text nodeKey="crosshair-label" bounds={{ x: 12, y: 43, width: 86, height: 20 }} style={heading}>WORLD VIEW</Text>
        </Panel>
        <Text nodeKey="viewport-hint" bounds={{ x: 18, y: 674, width: 420, height: 20 }}>Select a tool to begin a typed terrain interaction.</Text>
      </Panel>

      <Panel nodeKey="right-inspector" bounds={{ x: 1012, y: 66, width: 416, height: 710 }} style={mutedPanel}>
        <Text nodeKey="inspector-title" bounds={{ x: 14, y: 12, width: 260, height: 22 }} style={heading}>INSPECTOR</Text>
        <Section nodeKey="binding-section" bounds={{ x: 12, y: 48, width: 392, height: 142 }} title="RESOURCE BINDING">
          <Text nodeKey="binding-mode" bounds={{ x: 12, y: 38, width: 360, height: 20 }}>Mode                         {snapshot.mode}</Text>
          <Text nodeKey="binding-state" bounds={{ x: 12, y: 64, width: 360, height: 20 }} style={snapshot.bindingState === "failed" ? warning : clear}>State                        {snapshot.bindingState}</Text>
          <Button nodeKey="bind-material" bounds={{ x: 12, y: 96, width: 168, height: 30 }} style={snapshot.bindingState === "needs_selection" ? accent : mutedPanel} intent={action("resource.pick.open", { accepted_kinds: ["water_material"], terrain_id: snapshot.terrainId })}>Select material</Button>
        </Section>
        <Section nodeKey="material-section" bounds={{ x: 12, y: 202, width: 392, height: 212 }} title="MATERIALS">
          {snapshot.materials.map((material, index) => (
            <Panel key={material.rowKey} nodeKey={material.rowKey} bounds={{ x: 12, y: 38 + index * 38, width: 368, height: 30 }} style={material.state === "ready" ? accent : material.state === "failed" ? warning : panel}>
              <Text nodeKey="material-label" bounds={{ x: 10, y: 6, width: 190, height: 18 }}>{material.label}</Text>
              <Text nodeKey="material-state" bounds={{ x: 220, y: 6, width: 136, height: 18 }}>{material.state} / r{material.assetRevision}</Text>
            </Panel>
          ))}
        </Section>
        <Section nodeKey="selection-section" bounds={{ x: 12, y: 426, width: 392, height: 156 }} title="SELECTION">
          <Text nodeKey="selection-terrain" bounds={{ x: 12, y: 38, width: 360, height: 20 }}>Terrain ID                    {snapshot.terrainId}</Text>
          <Text nodeKey="selection-mode" bounds={{ x: 12, y: 64, width: 360, height: 20 }}>Tool mode                    {snapshot.mode}</Text>
          <Button nodeKey="clear-selection" bounds={{ x: 12, y: 100, width: 150, height: 30 }} style={mutedPanel} intent={action("terrain.selection.clear", { terrain_id: snapshot.terrainId })}>Clear selection</Button>
        </Section>
      </Panel>

      <Panel nodeKey="bottom-diagnostics" bounds={{ x: 12, y: 790, width: 1416, height: 94 }} style={panel}>
        <Text nodeKey="diagnostics-title" bounds={{ x: 14, y: 10, width: 210, height: 20 }} style={heading}>COMMAND JOURNAL / LIVE</Text>
        {snapshot.diagnostics.map((entry, index) => (
          <Panel key={entry.rowKey} nodeKey={entry.rowKey} bounds={{ x: 14 + index * 344, y: 40, width: 326, height: 38 }} style={entry.tone === "error" || entry.tone === "warning" ? warning : mutedPanel}>
            <Text nodeKey="diagnostic-label" bounds={{ x: 10, y: 5, width: 140, height: 16 }}>{entry.label}</Text>
            <Text nodeKey="diagnostic-value" bounds={{ x: 150, y: 5, width: 164, height: 16 }}>{entry.value}</Text>
          </Panel>
        ))}
      </Panel>
    </Surface>
  );
}
