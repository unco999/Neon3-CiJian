import type { ReactNode } from "react";
import { Button, Label, Panel, Surface, TextInput } from "../components.js";
import type { Bounds, JsonValue, Revision, UiIntentSpec, UiStyle } from "../protocol.js";

export type ShowcasePhase = "overview" | "motion";

const clear: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };
const shell: UiStyle = { background_color: [0.012, 0.022, 0.03, 1], border_color: [0.1, 0.3, 0.36, 1], border_width: 1, corner_radius: 4, opacity: 1 };
const panel: UiStyle = { background_color: [0.03, 0.06, 0.075, 1], border_color: [0.1, 0.24, 0.29, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const muted: UiStyle = { background_color: [0.02, 0.04, 0.05, 1], border_color: [0.07, 0.16, 0.2, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const active: UiStyle = { background_color: [0.04, 0.3, 0.34, 1], border_color: [0.32, 0.9, 0.82, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const warning: UiStyle = { background_color: [0.28, 0.16, 0.05, 1], border_color: [0.88, 0.52, 0.16, 1], border_width: 1, corner_radius: 3, opacity: 1 };

function text(nodeKey: string, bounds: Bounds, children: ReactNode, style: UiStyle = clear) {
  return <Label nodeKey={nodeKey} bounds={bounds} style={style}>{children}</Label>;
}

function intent(action: string, params: JsonValue = {}): UiIntentSpec {
  return { action, params };
}

function Capability({ nodeKey, y, title, detail, status }: { nodeKey: string; y: number; title: string; detail: string; status: "ready" | "partial" | "planned" }) {
  const statusStyle = status === "ready" ? active : status === "partial" ? warning : muted;
  const statusLabel = status === "ready" ? "READY" : status === "partial" ? "PARTIAL" : "PLANNED";
  return <Panel nodeKey={nodeKey} bounds={{ x: 0, y, width: 360, height: 54 }} style={muted}>
    {text(`${nodeKey}-title`, { x: 12, y: 8, width: 210, height: 18 }, title)}
    {text(`${nodeKey}-detail`, { x: 12, y: 29, width: 228, height: 16 }, detail)}
    <Panel nodeKey={`${nodeKey}-status`} bounds={{ x: 264, y: 12, width: 82, height: 28 }} style={statusStyle}>
      {text(`${nodeKey}-status-label`, { x: 6, y: 6, width: 70, height: 16 }, statusLabel)}
    </Panel>
  </Panel>;
}

export function UiPlatformShowcaseSurface({ revision, phase }: { revision: Revision; phase: ShowcasePhase }) {
  const moving = phase === "motion";
  const card = { x: moving ? 650 : 430, y: 160, width: 430, height: 230 };
  const from = { x: moving ? 1100 : 200, y: 160, width: 430, height: 230 };
  return <Surface surfaceId="surface.ui-platform-showcase" revision={revision} bounds={{ x: 0, y: 0, width: 1440, height: 900 }} style={shell}>
    <Panel nodeKey="header" bounds={{ x: 0, y: 0, width: 1440, height: 66 }} style={panel}>
      {text("brand", { x: 20, y: 15, width: 360, height: 24 }, "NEON3 UI PLATFORM / LIVE SHOWCASE")}
      {text("path", { x: 420, y: 17, width: 540, height: 20 }, "React declaration  ->  UI Runtime  ->  WGPU composition")}
      {text("revision", { x: 1240, y: 17, width: 160, height: 20 }, `revision ${revision}`)}
    </Panel>

    <Panel nodeKey="capabilities" bounds={{ x: 18, y: 84, width: 384, height: 798 }} style={panel}>
      {text("capabilities-title", { x: 14, y: 14, width: 300, height: 22 }, "CAPABILITY STATUS")}
      {text("capabilities-note", { x: 14, y: 40, width: 340, height: 30 }, "Verified renderer behavior, separated from planned platform work.")}
      <Capability nodeKey="declaration" y={88} title="UI declaration protocol" detail="fragment / revision / intent" status="ready" />
      <Capability nodeKey="composition" y={148} title="WGPU final composition" detail="single GPU owner" status="ready" />
      <Capability nodeKey="text" y={208} title="Text and CJK glyphs" detail="atlas / baseline / clipping" status="ready" />
      <Capability nodeKey="transition" y={268} title="Entry transitions" detail="bounds / opacity / easing" status="ready" />
      <Capability nodeKey="surface" y={328} title="RenderSurface" detail="renderer-owned texture" status="ready" />
      <Capability nodeKey="layout" y={388} title="Flex / intrinsic text" detail="row/column, factors, alignment" status="ready" />
      <Capability nodeKey="style" y={448} title="Theme and visual tokens" detail="currently component styles" status="planned" />
      <Capability nodeKey="input" y={508} title="Focused TextInput / IME" detail="local selection, scroll, typed commit" status="ready" />
      <Capability nodeKey="fast-path" y={568} title="High-frequency data path" detail="TCP/SPSC benchmark" status="planned" />
      {text("boundary", { x: 14, y: 650, width: 340, height: 74 }, "Ownership boundary: React declares. UI Runtime owns semantic state. WGPU owns window, GPU resources, hit-test and final pixels.", muted)}
    </Panel>

    <Panel nodeKey="motion-stage" bounds={{ x: 420, y: 84, width: 700, height: 430 }} style={panel}>
      {text("motion-title", { x: 16, y: 14, width: 320, height: 22 }, "LIVE MOTION / NESTED COMPOSITION")}
      {text("motion-detail", { x: 16, y: 42, width: 620, height: 18 }, "Toggle the phase to submit a new revision and animate one renderer-owned panel.")}
      <Panel nodeKey="motion-card" bounds={card} style={moving ? active : warning} enterTransition={{ durationMs: 720, easing: "ease_in_out", from: { bounds: from, opacity: 0.2 } }}>
        {text("motion-card-title", { x: 24, y: 72, width: 360, height: 24 }, moving ? "REVISION / MOTION TARGET" : "REVISION / INITIAL TARGET")}
        {text("motion-card-body", { x: 24, y: 112, width: 360, height: 42 }, "Nested panel, label placement, color state, opacity and transition are rendered by WGPU.")}
        <Button nodeKey="motion-intent" bounds={{ x: 24, y: 172, width: 170, height: 32 }} style={moving ? active : muted} intent={intent("ui.showcase.motion", { phase: moving ? "motion" : "overview" })}>{moving ? "Motion active" : "Motion state"}</Button>
      </Panel>
    </Panel>

    <Panel nodeKey="interaction" bounds={{ x: 420, y: 532, width: 700, height: 370 }} style={panel}>
      {text("interaction-title", { x: 16, y: 14, width: 330, height: 22 }, "DECLARATION AND INTENT CHECK")}
      {text("interaction-detail", { x: 16, y: 44, width: 620, height: 20 }, "These controls emit typed semantic intents; they do not directly mutate terrain or project state.")}
      <Panel nodeKey="intent-row" bounds={{ x: 16, y: 82, width: 668, height: 62 }} style={muted}>
        {text("intent-code", { x: 12, y: 10, width: 380, height: 18 }, "terrain.tool.select { tool: water_inject }")}
        <Button nodeKey="intent-button" bounds={{ x: 500, y: 14, width: 150, height: 32 }} style={active} intent={intent("terrain.tool.select", { tool: "water_inject" })}>Emit intent</Button>
      </Panel>
      <Panel nodeKey="render-row" bounds={{ x: 16, y: 158, width: 668, height: 62 }} style={muted}>
        {text("render-code", { x: 12, y: 10, width: 420, height: 18 }, "wgpu.render.diagnostics -> composition snapshot")}
        <Button nodeKey="diagnostics-button" bounds={{ x: 500, y: 14, width: 150, height: 32 }} style={muted} intent={intent("debug.diagnostics.open", { surface: "ui-platform-showcase" })}>Inspect path</Button>
      </Panel>
      <Panel nodeKey="flex-row" bounds={{ x: 16, y: 234, width: 668, height: 34 }} style={muted} layout={{ mode: "row", gap: 8, align_items: "center", justify_content: "space_between" }}>
        <Label nodeKey="flex-intrinsic" bounds={{ x: 0, y: 0, width: 0, height: 0 }} layout={{ flex_basis: null }}>intrinsic text</Label>
        <Label nodeKey="flex-grow" bounds={{ x: 0, y: 0, width: 0, height: 18 }} layout={{ flex_grow: 1 }}>grows into remaining row space</Label>
        <Label nodeKey="flex-end" bounds={{ x: 0, y: 0, width: 0, height: 0 }} layout={{ flex_basis: null }}>end</Label>
      </Panel>
      <Panel nodeKey="text-input-row" bounds={{ x: 16, y: 278, width: 668, height: 76 }} style={muted}>
        {text("text-input-label", { x: 12, y: 10, width: 420, height: 18 }, "TEXT INPUT / POINTER CARET, SELECTION, LOCAL PREEDIT")}
        <TextInput nodeKey="showcase-text" bounds={{ x: 12, y: 34, width: 640, height: 30 }} value="Focus, select, and type a longer value" maxLength={256} style={active} intent={intent("ui.showcase.text.commit", {})} />
      </Panel>
      {text("interaction-note", { x: 16, y: 356, width: 650, height: 14 }, "Single-line editing stays local in WGPU; only committed values reach UI Runtime.", warning)}
    </Panel>
  </Surface>;
}
