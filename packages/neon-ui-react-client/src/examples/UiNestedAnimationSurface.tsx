import { Label, Panel, Surface } from "../components.js";
import type { Bounds, Revision, UiStyle } from "../protocol.js";

export type UiNestedAnimationPhase = "overview" | "focus" | "release";

type Scene = {
  workspace: Bounds;
  rail: Bounds;
  canvas: Bounds;
  primary: Bounds;
  secondary: Bounds;
  inspector: Bounds;
  status: Bounds;
};

const clear: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };
const shell: UiStyle = { background_color: [0.018, 0.026, 0.035, 1], border_color: [0.1, 0.2, 0.25, 1], border_width: 1, corner_radius: 0, opacity: 1 };
const workspaceStyle: UiStyle = { background_color: [0.035, 0.065, 0.08, 1], border_color: [0.25, 0.78, 0.86, 0.88], border_width: 2, corner_radius: 8, opacity: 1 };
const railStyle: UiStyle = { background_color: [0.04, 0.12, 0.14, 1], border_color: [0.16, 0.52, 0.58, 1], border_width: 1, corner_radius: 6, opacity: 1 };
const canvasStyle: UiStyle = { background_color: [0.045, 0.052, 0.085, 1], border_color: [0.25, 0.38, 0.75, 0.92], border_width: 1, corner_radius: 6, opacity: 1 };
const primaryStyle: UiStyle = { background_color: [0.05, 0.48, 0.62, 1], border_color: [0.72, 0.96, 1, 1], border_width: 2, corner_radius: 6, opacity: 1 };
const secondaryStyle: UiStyle = { background_color: [0.88, 0.3, 0.12, 1], border_color: [1, 0.8, 0.35, 1], border_width: 2, corner_radius: 6, opacity: 1 };
const inspectorStyle: UiStyle = { background_color: [0.16, 0.27, 0.19, 1], border_color: [0.52, 0.9, 0.52, 1], border_width: 2, corner_radius: 6, opacity: 1 };
const statusStyle: UiStyle = { background_color: [0.82, 0.68, 0.1, 1], border_color: [1, 0.94, 0.55, 1], border_width: 1, corner_radius: 4, opacity: 1 };

const scenes: Record<UiNestedAnimationPhase, Scene> = {
  overview: {
    workspace: { x: 76, y: 66, width: 1288, height: 768 }, rail: { x: 24, y: 82, width: 244, height: 610 },
    canvas: { x: 294, y: 82, width: 970, height: 610 }, primary: { x: 26, y: 82, width: 522, height: 344 },
    secondary: { x: 574, y: 82, width: 370, height: 220 }, inspector: { x: 574, y: 326, width: 370, height: 260 }, status: { x: 28, y: 716, width: 300, height: 28 },
  },
  focus: {
    workspace: { x: 34, y: 34, width: 1372, height: 832 }, rail: { x: 24, y: 82, width: 162, height: 670 },
    canvas: { x: 210, y: 82, width: 1138, height: 670 }, primary: { x: 38, y: 64, width: 716, height: 482 },
    secondary: { x: 782, y: 64, width: 318, height: 224 }, inspector: { x: 782, y: 314, width: 318, height: 302 }, status: { x: 28, y: 780, width: 468, height: 28 },
  },
  release: {
    workspace: { x: 92, y: 94, width: 1256, height: 712 }, rail: { x: 24, y: 82, width: 290, height: 552 },
    canvas: { x: 340, y: 82, width: 892, height: 552 }, primary: { x: 26, y: 54, width: 394, height: 286 },
    secondary: { x: 446, y: 54, width: 420, height: 194 }, inspector: { x: 446, y: 274, width: 420, height: 204 }, status: { x: 28, y: 656, width: 240, height: 28 },
  },
};

function transition(durationMs: number, bounds: Bounds, opacity = 0.18) {
  return { durationMs, easing: "ease_in_out" as const, from: { bounds, opacity } };
}

export function UiNestedAnimationSurface({ revision, phase }: { revision: Revision; phase: UiNestedAnimationPhase }) {
  const scene = scenes[phase];
  return <Surface surfaceId="surface.ui-nested-animation" revision={revision} bounds={{ x: 0, y: 0, width: 1440, height: 900 }} style={shell}>
    <Label nodeKey="title" bounds={{ x: 32, y: 28, width: 520, height: 34 }} style={clear}>NESTED RENDER PLAN / {phase.toUpperCase()}</Label>
    <Panel nodeKey="workspace" bounds={scene.workspace} layout={{ mode: "absolute", clip: true }} style={workspaceStyle} enterTransition={transition(720, { x: -200, y: 66, width: 1288, height: 768 })}>
      <Panel nodeKey="rail" bounds={scene.rail} layout={{ mode: "absolute", clip: true }} style={railStyle} enterTransition={transition(620, { x: -180, y: 82, width: 244, height: 610 })}>
        <Label nodeKey="rail-heading" bounds={{ x: 18, y: 20, width: 180, height: 28 }} style={clear}>LAYERS</Label>
        <Panel nodeKey="rail-item-a" bounds={{ x: 18, y: 72, width: 208, height: 76 }} style={primaryStyle} enterTransition={transition(460, { x: -90, y: 72, width: 208, height: 76 })}>
          <Label nodeKey="rail-item-a-label" bounds={{ x: 14, y: 24, width: 170, height: 24 }} style={clear}>TERRAIN</Label>
        </Panel>
        <Panel nodeKey="rail-item-b" bounds={{ x: 18, y: 170, width: 208, height: 76 }} style={secondaryStyle} enterTransition={transition(530, { x: -90, y: 170, width: 208, height: 76 })}>
          <Label nodeKey="rail-item-b-label" bounds={{ x: 14, y: 24, width: 170, height: 24 }} style={clear}>WATER</Label>
        </Panel>
        <Panel nodeKey="rail-item-c" bounds={{ x: 18, y: 268, width: 208, height: 76 }} style={inspectorStyle} enterTransition={transition(590, { x: -90, y: 268, width: 208, height: 76 })}>
          <Label nodeKey="rail-item-c-label" bounds={{ x: 14, y: 24, width: 170, height: 24 }} style={clear}>ASSETS</Label>
        </Panel>
      </Panel>
      <Panel nodeKey="canvas" bounds={scene.canvas} layout={{ mode: "absolute", clip: true }} style={canvasStyle} enterTransition={transition(760, { x: 1320, y: 82, width: 970, height: 610 })}>
        <Panel nodeKey="toolbar" bounds={{ x: 22, y: 20, width: 500, height: 42 }} style={railStyle} enterTransition={transition(420, { x: 22, y: -80, width: 500, height: 42 })}>
          <Panel nodeKey="tool-chip-a" bounds={{ x: 12, y: 8, width: 116, height: 26 }} style={statusStyle} enterTransition={transition(340, { x: 12, y: -40, width: 116, height: 26 })} />
          <Panel nodeKey="tool-chip-b" bounds={{ x: 142, y: 8, width: 116, height: 26 }} style={primaryStyle} enterTransition={transition(410, { x: 142, y: -40, width: 116, height: 26 })} />
          <Panel nodeKey="tool-chip-c" bounds={{ x: 272, y: 8, width: 116, height: 26 }} style={inspectorStyle} enterTransition={transition(480, { x: 272, y: -40, width: 116, height: 26 })} />
        </Panel>
        <Panel nodeKey="primary-card" bounds={scene.primary} style={primaryStyle} enterTransition={transition(680, { x: 26, y: 690, width: 522, height: 344 })}>
          <Label nodeKey="primary-label" bounds={{ x: 24, y: 28, width: 330, height: 30 }} style={clear}>PRIMARY VIEW</Label>
          <Panel nodeKey="primary-meter" bounds={{ x: 24, y: 240, width: 260, height: 48 }} style={statusStyle} enterTransition={transition(540, { x: 24, y: 400, width: 260, height: 48 })} />
        </Panel>
        <Panel nodeKey="secondary-card" bounds={scene.secondary} style={secondaryStyle} enterTransition={transition(740, { x: 1040, y: 82, width: 370, height: 220 })}>
          <Label nodeKey="secondary-label" bounds={{ x: 24, y: 28, width: 260, height: 30 }} style={clear}>MATERIAL STACK</Label>
        </Panel>
        <Panel nodeKey="inspector-card" bounds={scene.inspector} style={inspectorStyle} enterTransition={transition(820, { x: 1040, y: 326, width: 370, height: 260 })}>
          <Label nodeKey="inspector-label" bounds={{ x: 24, y: 28, width: 260, height: 30 }} style={clear}>INSPECTOR</Label>
          <Panel nodeKey="inspector-status" bounds={{ x: 24, y: 84, width: 160, height: 34 }} style={statusStyle} enterTransition={transition(600, { x: 300, y: 84, width: 160, height: 34 })} />
        </Panel>
      </Panel>
      <Panel nodeKey="status-bar" bounds={scene.status} style={statusStyle} enterTransition={transition(500, { x: 28, y: 820, width: 300, height: 28 })}>
        <Label nodeKey="status-label" bounds={{ x: 10, y: 5, width: 260, height: 18 }} style={clear}>GPU PLAN STABLE</Label>
      </Panel>
    </Panel>
  </Surface>;
}
