import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { createNeonRoot } from "../dist/renderer.js";
import {
  TERRAIN_PARENT_LABELS,
  TERRAIN_RELIEF_LABELS,
  TERRAIN_SUB_LABELS,
  TERRAIN_TEXTURE_LABELS,
  TERRAIN_WATER_LABELS,
  TerrainGenerationSurface,
} from "../dist/index.js";

const snapshot = {
  revision: 12,
  condition: { sub: 6, parent: 1, relief: 3, texture: 2, water: 2 },
  guidance: 7,
  steps: 2,
  seed: 42,
  lastSeed: null,
  size: 32,
  targetId: "ai.terrain.preview",
  state: "idle",
  jobId: null,
  elapsedMs: null,
  errorCode: null,
};

test("terrain generation vocabularies mirror the UNet conditioning tables", () => {
  assert.equal(TERRAIN_SUB_LABELS.length, 23);
  assert.equal(TERRAIN_PARENT_LABELS.length, 8);
  assert.equal(TERRAIN_PARENT_LABELS[1], "desert");
  assert.deepEqual(TERRAIN_RELIEF_LABELS, ["flat", "low", "mid", "high", "extreme"]);
  assert.deepEqual(TERRAIN_TEXTURE_LABELS, ["smooth", "undulating", "fine_ridged", "coarse_rugged"]);
  assert.deepEqual(TERRAIN_WATER_LABELS, ["land", "water_edge", "water_lots"]);
});

test("one render button emits one complete generation intent and a GPU surface panel", async () => {
  const fragmentPromise = new Promise((resolve, reject) => {
    const root = createNeonRoot({ submit: resolve, onError: reject });
    root.render(React.createElement(TerrainGenerationSurface, { snapshot }));
  });
  const fragment = await fragmentPromise;
  const generate = fragment.effects.filter((effect) => effect.intent?.action === "ai.terrain.generate");
  assert.equal(generate.length, 1);
  assert.deepEqual(generate[0].intent.params, {
    condition: snapshot.condition,
    guidance: 7,
    steps: 2,
    seed: 42,
    size: 32,
    target_id: "ai.terrain.preview",
  });
  assert.ok(fragment.effects.some((effect) => effect.intent?.action === "ai.terrain.condition.set"));

  const find = (node, predicate) => predicate(node) ? node : node.children.map((child) => find(child, predicate)).find(Boolean);
  const preview = find(fragment.root, (node) => node.kind === "render_surface");
  assert.deepEqual(preview.surface, { target_id: "ai.terrain.preview" });
  const renderButton = find(fragment.root, (node) => node.node_id === generate[0].node_id);
  assert.equal(renderButton.enabled, true);
});
