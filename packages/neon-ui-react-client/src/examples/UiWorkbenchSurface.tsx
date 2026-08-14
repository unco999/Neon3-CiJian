import { Button, Label, Panel, Surface } from "../components.js";
import type { Revision, UiIntentSpec, UiStyle } from "../protocol.js";

export type UiWorkbenchSnapshot = { revision: Revision; diagnosticsExpanded: boolean; inspectorTab: "overview" | "materials" | "history" };

const shell: UiStyle = { background_color: [0.025, 0.04, 0.055, 1], border_color: [0.12, 0.3, 0.36, 1], border_width: 1, corner_radius: 4, opacity: 1 };
const panel: UiStyle = { background_color: [0.045, 0.07, 0.085, 1], border_color: [0.12, 0.25, 0.29, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const active: UiStyle = { background_color: [0.06, 0.32, 0.38, 1], border_color: [0.35, 0.9, 0.88, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const clear: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };

const action = (action: string, params: Record<string, string> = {}): UiIntentSpec => ({ action, params });

export function UiWorkbenchSurface({ snapshot }: { snapshot: UiWorkbenchSnapshot }) {
  const diagnosticsHeight = snapshot.diagnosticsExpanded ? 260 : 94;
  const diagnosticsY = 888 - diagnosticsHeight;
  return <Surface surfaceId="surface.ui-workbench" revision={snapshot.revision} bounds={{ x: 0, y: 0, width: 1440, height: 900 }} style={shell}>
    <Panel nodeKey="workspace" bounds={{ x: 12, y: 12, width: 1416, height: diagnosticsY - 24 }} style={panel}>
      <Label nodeKey="title" bounds={{ x: 16, y: 16, width: 300, height: 24 }} style={clear}>UI Workbench</Label>
      <Button nodeKey="toggle-diagnostics" bounds={{ x: 1210, y: 12, width: 180, height: 32 }} style={snapshot.diagnosticsExpanded ? active : panel} intent={action("ui.surface.action", { action: "diagnostics.toggle" })}>{snapshot.diagnosticsExpanded ? "Collapse diagnostics" : "Expand diagnostics"}</Button>
      {(["overview", "materials", "history"] as const).map((tab, index) => <Button key={tab} nodeKey={`tab-${tab}`} bounds={{ x: 16 + index * 116, y: 60, width: 108, height: 30 }} style={snapshot.inspectorTab === tab ? active : panel} intent={action("ui.surface.action", { action: "inspector.tab.select", tab })}>{tab}</Button>)}
      <Label nodeKey="tab-content" bounds={{ x: 16, y: 112, width: 640, height: 30 }} style={clear}>Selected tab: {snapshot.inspectorTab}</Label>
    </Panel>
    <Panel nodeKey="diagnostics" bounds={{ x: 12, y: diagnosticsY, width: 1416, height: diagnosticsHeight }} style={panel} enterTransition={{ durationMs: 180, easing: "ease_out", from: { opacity: 0 } }}>
      <Label nodeKey="diagnostics-title" bounds={{ x: 16, y: 14, width: 300, height: 24 }} style={clear}>Diagnostics</Label>
      <Label nodeKey="diagnostics-body" bounds={{ x: 16, y: 52, width: 800, height: diagnosticsHeight - 64 }} style={clear}>{snapshot.diagnosticsExpanded ? "Expanded diagnostics content" : "Collapsed diagnostics summary"}</Label>
    </Panel>
  </Surface>;
}
