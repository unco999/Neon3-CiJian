import { Label, Panel, Surface } from "../components.js";
import type { Revision, UiStyle } from "../protocol.js";

export type UiAnimationPhase = "left" | "right";

const shell: UiStyle = { background_color: [0.015, 0.02, 0.028, 1], border_color: [0.12, 0.18, 0.22, 1], border_width: 1, corner_radius: 0, opacity: 1 };
const leftStyle: UiStyle = { background_color: [0.04, 0.52, 0.68, 1], border_color: [0.72, 0.96, 1, 1], border_width: 3, corner_radius: 12, opacity: 1 };
const rightStyle: UiStyle = { background_color: [0.92, 0.22, 0.06, 1], border_color: [1, 0.82, 0.3, 1], border_width: 3, corner_radius: 12, opacity: 1 };
const clear: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };

export function UiAnimationSurface({ revision, phase }: { revision: Revision; phase: UiAnimationPhase }) {
  const isRight = phase === "right";
  const target = { x: isRight ? 940 : 120, y: 320, width: 360, height: 220 };
  const initial = { x: isRight ? 120 : -360, y: 320, width: 360, height: 220 };
  return <Surface surfaceId="surface.ui-animation" revision={revision} bounds={{ x: 0, y: 0, width: 1440, height: 900 }} style={shell}>
    <Label nodeKey="caption" bounds={{ x: 32, y: 32, width: 500, height: 32 }} style={clear}>React-declared WGPU animation</Label>
    <Panel nodeKey="animated-card" bounds={target} style={isRight ? rightStyle : leftStyle} enterTransition={{ durationMs: 900, easing: "ease_in_out", from: { bounds: initial, opacity: 0.35 } }}>
      <Label nodeKey="card-label" bounds={{ x: 28, y: 86, width: 304, height: 42 }} style={clear}>{isRight ? "RIGHT TARGET" : "LEFT TARGET"}</Label>
    </Panel>
  </Surface>;
}
