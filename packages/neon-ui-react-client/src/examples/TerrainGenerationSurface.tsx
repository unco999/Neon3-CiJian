import type { ReactNode } from "react";
import { Button, Label, Panel, RenderSurface, Surface } from "../components.js";
import type { Bounds, JsonValue, Revision, UiIntentSpec, UiStyle } from "../protocol.js";

export const TERRAIN_SUB_LABELS = [
  "alpine", "caldera_rim", "canyon_gorge", "cliff_coast", "delta", "dissected_hills",
  "dune_sea", "fjord", "flat_plain", "glacier_highland", "hamada", "high_plateau",
  "lava_plateau", "mesa_badlands", "mid_mountain", "rocky_wadi", "rolling_hills",
  "salt_playa", "sandy_coast", "shield_volcano", "stratovolcano", "tundra_lowland",
  "undulating_plain",
] as const;
export const TERRAIN_PARENT_LABELS = ["coastal", "desert", "glacial", "hill", "mountain", "plain", "plateau", "volcanic"] as const;
export const TERRAIN_RELIEF_LABELS = ["flat", "low", "mid", "high", "extreme"] as const;
export const TERRAIN_TEXTURE_LABELS = ["smooth", "undulating", "fine_ridged", "coarse_rugged"] as const;
export const TERRAIN_WATER_LABELS = ["land", "water_edge", "water_lots"] as const;

export type TerrainGenerationCondition = {
  sub: number | null;
  parent: number | null;
  relief: number | null;
  texture: number | null;
  water: number | null;
};

export type TerrainGenerationSnapshot = {
  revision: Revision;
  condition: TerrainGenerationCondition;
  guidance: number;
  steps: number;
  seed: number;
  lastSeed: number | null;
  size: number;
  targetId: string;
  state: "idle" | "queued" | "generating" | "ready" | "failed";
  jobId: string | null;
  elapsedMs: number | null;
  errorCode: string | null;
};

const clear: UiStyle = { background_color: [0, 0, 0, 0], border_color: [0, 0, 0, 0], border_width: 0, corner_radius: 0, opacity: 1 };
const shell: UiStyle = { background_color: [0.018, 0.027, 0.034, 1], border_color: [0.12, 0.27, 0.31, 1], border_width: 1, corner_radius: 4, opacity: 1 };
const panel: UiStyle = { background_color: [0.035, 0.052, 0.061, 0.98], border_color: [0.11, 0.23, 0.26, 1], border_width: 1, corner_radius: 4, opacity: 1 };
const control: UiStyle = { background_color: [0.055, 0.075, 0.085, 1], border_color: [0.14, 0.28, 0.3, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const selected: UiStyle = { background_color: [0.05, 0.34, 0.36, 1], border_color: [0.35, 0.92, 0.82, 1], border_width: 1, corner_radius: 3, opacity: 1 };
const render: UiStyle = { background_color: [0.04, 0.48, 0.37, 1], border_color: [0.5, 1, 0.78, 1], border_width: 1, corner_radius: 4, opacity: 1 };
const warning: UiStyle = { background_color: [0.3, 0.16, 0.05, 1], border_color: [0.9, 0.52, 0.16, 1], border_width: 1, corner_radius: 3, opacity: 1 };

function action(actionName: string, params: JsonValue): UiIntentSpec {
  return { action: actionName, params };
}

function Text({ nodeKey, bounds, children, style = clear }: { nodeKey: string; bounds: Bounds; children: ReactNode; style?: UiStyle }) {
  return <Label nodeKey={nodeKey} bounds={bounds} style={style}>{children}</Label>;
}

function optionIntent(dimension: keyof TerrainGenerationCondition, index: number, label: string): UiIntentSpec {
  return action("ai.terrain.condition.set", { dimension, index, label });
}

function displayLabel(value: string): string {
  return value.replaceAll("_", " ");
}

function selectedLabel(labels: readonly string[], index: number | null): string {
  return index === null ? "unconditioned" : displayLabel(labels[index] ?? "invalid");
}

export function TerrainGenerationSurface({ snapshot }: { snapshot: TerrainGenerationSnapshot }) {
  const busy = snapshot.state === "queued" || snapshot.state === "generating";
  const renderIntent = action("ai.terrain.generate", {
    condition: snapshot.condition,
    guidance: snapshot.guidance,
    steps: snapshot.steps,
    seed: snapshot.seed,
    size: snapshot.size,
    target_id: snapshot.targetId,
  });
  const statusStyle = snapshot.state === "failed" ? warning : snapshot.state === "ready" ? selected : control;
  return (
    <Surface surfaceId="surface.ai.terrain-generator" revision={snapshot.revision} bounds={{ x: 0, y: 0, width: 1440, height: 900 }} style={shell}>
      <Panel nodeKey="topbar" bounds={{ x: 0, y: 0, width: 1440, height: 54 }} style={panel}>
        <Text nodeKey="title" bounds={{ x: 18, y: 13, width: 320, height: 24 }}>AI TERRAIN GENERATOR</Text>
        <Text nodeKey="model" bounds={{ x: 354, y: 15, width: 360, height: 20 }}>terrain_unet_ddim_v1 / GPU resident preview</Text>
        <Text nodeKey="revision" bounds={{ x: 1190, y: 15, width: 220, height: 20 }}>UI revision {snapshot.revision}</Text>
      </Panel>

      <Panel nodeKey="condition-panel" bounds={{ x: 12, y: 66, width: 348, height: 818 }} style={panel}>
        <Text nodeKey="condition-title" bounds={{ x: 12, y: 10, width: 250, height: 20 }}>CONDITION LABELS</Text>
        <Text nodeKey="sub-title" bounds={{ x: 12, y: 40, width: 200, height: 18 }}>Terrain subtype / 23</Text>
        <Panel nodeKey="sub-current" bounds={{ x: 12, y: 64, width: 204, height: 36 }} style={selected}>
          <Text nodeKey="sub-current-label" bounds={{ x: 10, y: 8, width: 184, height: 20 }}>{selectedLabel(TERRAIN_SUB_LABELS, snapshot.condition.sub)}</Text>
        </Panel>
        <Button nodeKey="sub-previous" bounds={{ x: 224, y: 64, width: 48, height: 36 }} style={control} intent={optionIntent("sub", ((snapshot.condition.sub ?? 0) + TERRAIN_SUB_LABELS.length - 1) % TERRAIN_SUB_LABELS.length, TERRAIN_SUB_LABELS[((snapshot.condition.sub ?? 0) + TERRAIN_SUB_LABELS.length - 1) % TERRAIN_SUB_LABELS.length])}>&lt;</Button>
        <Button nodeKey="sub-next" bounds={{ x: 280, y: 64, width: 48, height: 36 }} style={control} intent={optionIntent("sub", ((snapshot.condition.sub ?? -1) + 1) % TERRAIN_SUB_LABELS.length, TERRAIN_SUB_LABELS[((snapshot.condition.sub ?? -1) + 1) % TERRAIN_SUB_LABELS.length])}>&gt;</Button>
        <Text nodeKey="sub-index" bounds={{ x: 12, y: 108, width: 316, height: 18 }}>Embedding index: {snapshot.condition.sub ?? TERRAIN_SUB_LABELS.length} / null index {TERRAIN_SUB_LABELS.length}</Text>

        <Text nodeKey="parent-title" bounds={{ x: 12, y: 144, width: 200, height: 18 }}>Parent biome / 8</Text>
        {TERRAIN_PARENT_LABELS.map((label, index) => (
          <Button key={label} nodeKey={`parent-${label}`} bounds={{ x: 12 + (index % 4) * 81, y: 168 + Math.floor(index / 4) * 31, width: 74, height: 26 }} style={snapshot.condition.parent === index ? selected : control} intent={optionIntent("parent", index, label)}>{displayLabel(label)}</Button>
        ))}

        <Text nodeKey="relief-title" bounds={{ x: 12, y: 238, width: 200, height: 18 }}>Relief / 5</Text>
        {TERRAIN_RELIEF_LABELS.map((label, index) => (
          <Button key={label} nodeKey={`relief-${label}`} bounds={{ x: 12 + index * 64, y: 262, width: 58, height: 26 }} style={snapshot.condition.relief === index ? selected : control} intent={optionIntent("relief", index, label)}>{label}</Button>
        ))}

        <Text nodeKey="texture-title" bounds={{ x: 12, y: 306, width: 200, height: 18 }}>Surface texture / 4</Text>
        {TERRAIN_TEXTURE_LABELS.map((label, index) => (
          <Button key={label} nodeKey={`texture-${label}`} bounds={{ x: 12 + (index % 2) * 162, y: 330 + Math.floor(index / 2) * 31, width: 154, height: 26 }} style={snapshot.condition.texture === index ? selected : control} intent={optionIntent("texture", index, label)}>{displayLabel(label)}</Button>
        ))}

        <Text nodeKey="water-title" bounds={{ x: 12, y: 404, width: 200, height: 18 }}>Water profile / 3</Text>
        {TERRAIN_WATER_LABELS.map((label, index) => (
          <Button key={label} nodeKey={`water-${label}`} bounds={{ x: 12 + index * 108, y: 428, width: 100, height: 28 }} style={snapshot.condition.water === index ? selected : control} intent={optionIntent("water", index, label)}>{displayLabel(label)}</Button>
        ))}

        <Button nodeKey="condition-clear" bounds={{ x: 12, y: 482, width: 316, height: 32 }} style={control} intent={action("ai.terrain.condition.reset", {})}>Clear all labels</Button>
        <Panel nodeKey="selected-summary" bounds={{ x: 12, y: 536, width: 316, height: 166 }} style={control}>
          <Text nodeKey="summary-title" bounds={{ x: 10, y: 10, width: 280, height: 18 }}>SELECTED EMBEDDINGS</Text>
          <Text nodeKey="summary-sub" bounds={{ x: 10, y: 38, width: 292, height: 18 }}>sub: {selectedLabel(TERRAIN_SUB_LABELS, snapshot.condition.sub)}</Text>
          <Text nodeKey="summary-parent" bounds={{ x: 10, y: 64, width: 292, height: 18 }}>parent: {selectedLabel(TERRAIN_PARENT_LABELS, snapshot.condition.parent)}</Text>
          <Text nodeKey="summary-relief" bounds={{ x: 10, y: 90, width: 292, height: 18 }}>relief: {selectedLabel(TERRAIN_RELIEF_LABELS, snapshot.condition.relief)}</Text>
          <Text nodeKey="summary-texture" bounds={{ x: 10, y: 116, width: 292, height: 18 }}>texture: {selectedLabel(TERRAIN_TEXTURE_LABELS, snapshot.condition.texture)}</Text>
          <Text nodeKey="summary-water" bounds={{ x: 10, y: 142, width: 292, height: 18 }}>water: {selectedLabel(TERRAIN_WATER_LABELS, snapshot.condition.water)}</Text>
        </Panel>
        <Text nodeKey="condition-note" bounds={{ x: 12, y: 726, width: 316, height: 58 }}>Selections only revise this snapshot. Inference starts exclusively from Render once.</Text>
      </Panel>

      <Panel nodeKey="preview-panel" bounds={{ x: 372, y: 66, width: 728, height: 818 }} style={panel}>
        <Text nodeKey="preview-title" bounds={{ x: 14, y: 10, width: 300, height: 20 }}>GPU PREVIEW / {snapshot.size} x {snapshot.size}</Text>
        <RenderSurface nodeKey="terrain-preview" bounds={{ x: 14, y: 42, width: 700, height: 700 }} surface={{ target_id: snapshot.targetId }} style={clear} />
        <Panel nodeKey="preview-footer" bounds={{ x: 14, y: 754, width: 700, height: 48 }} style={control}>
          <Text nodeKey="preview-state" bounds={{ x: 12, y: 8, width: 260, height: 20 }}>State: {snapshot.state}</Text>
          <Text nodeKey="preview-job" bounds={{ x: 286, y: 8, width: 396, height: 20 }}>Job: {snapshot.jobId ?? "not submitted"}</Text>
        </Panel>
      </Panel>

      <Panel nodeKey="render-panel" bounds={{ x: 1112, y: 66, width: 316, height: 818 }} style={panel}>
        <Text nodeKey="render-title" bounds={{ x: 14, y: 10, width: 220, height: 20 }}>RENDER SETTINGS</Text>
        <Panel nodeKey="settings-summary" bounds={{ x: 12, y: 42, width: 292, height: 146 }} style={control}>
          <Text nodeKey="size" bounds={{ x: 12, y: 12, width: 268, height: 20 }}>Output size                 {snapshot.size}</Text>
          <Text nodeKey="steps" bounds={{ x: 12, y: 40, width: 268, height: 20 }}>DDIM steps                 {snapshot.steps}</Text>
          <Text nodeKey="guidance" bounds={{ x: 12, y: 68, width: 268, height: 20 }}>CFG guidance               {snapshot.guidance}</Text>
          <Text nodeKey="seed" bounds={{ x: 12, y: 96, width: 268, height: 20 }}>Next seed                  {snapshot.seed}</Text>
        </Panel>

        <Text nodeKey="steps-title" bounds={{ x: 14, y: 210, width: 180, height: 18 }}>DDIM step preset</Text>
        {[4, 6, 10, 25].map((steps, index) => (
          <Button key={steps} nodeKey={`steps-${steps}`} bounds={{ x: 14 + index * 72, y: 236, width: 64, height: 30 }} style={snapshot.steps === steps ? selected : control} intent={action("ai.terrain.settings.set", { steps })}>{steps}</Button>
        ))}

        <Text nodeKey="guidance-title" bounds={{ x: 14, y: 288, width: 180, height: 18 }}>Guidance preset</Text>
        {[0, 1, 3, 5].map((guidance, index) => (
          <Button key={guidance} nodeKey={`guidance-${guidance}`} bounds={{ x: 14 + index * 72, y: 314, width: 64, height: 30 }} style={snapshot.guidance === guidance ? selected : control} intent={action("ai.terrain.settings.set", { guidance })}>{guidance}</Button>
        ))}

        <Button nodeKey="seed-next" bounds={{ x: 14, y: 370, width: 288, height: 32 }} style={control} intent={action("ai.terrain.seed.next", { current_seed: snapshot.seed })}>Use next deterministic seed</Button>

        <Panel nodeKey="status" bounds={{ x: 12, y: 430, width: 292, height: 122 }} style={statusStyle}>
          <Text nodeKey="status-label" bounds={{ x: 12, y: 12, width: 268, height: 20 }}>Generation: {snapshot.state}</Text>
          <Text nodeKey="status-time" bounds={{ x: 12, y: 42, width: 268, height: 20 }}>Elapsed: {snapshot.elapsedMs === null ? "--" : `${snapshot.elapsedMs.toFixed(1)} ms`}</Text>
          <Text nodeKey="status-seed" bounds={{ x: 12, y: 70, width: 268, height: 20 }}>Rendered seed: {snapshot.lastSeed ?? "--"}</Text>
          <Text nodeKey="status-error" bounds={{ x: 12, y: 94, width: 268, height: 24 }}>Error: {snapshot.errorCode ?? "none"}</Text>
        </Panel>

        <Button nodeKey="render-once" bounds={{ x: 12, y: 590, width: 292, height: 72 }} style={busy ? control : render} enabled={!busy} intent={renderIntent}>{busy ? "GENERATING" : "RENDER ONCE"}</Button>
        <Text nodeKey="render-note" bounds={{ x: 14, y: 680, width: 286, height: 86 }}>One click creates one idempotent generation command. The resulting GPU texture replaces the current preview slot after completion.</Text>
      </Panel>
    </Surface>
  );
}
