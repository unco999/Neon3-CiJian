import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { createNeonRoot } from "../dist/renderer.js";
import { TerrainEditorSurface } from "../dist/index.js";

const snapshot = {
  projectRevision: 42,
  terrainId: 12,
  terrainName: "North Basin",
  mode: "water_paint",
  bindingState: "needs_selection",
  tools: [
    { mode: "select", label: "Select", active: false, rowKey: "tool-select" },
    { mode: "water_inject", label: "Water inject", active: true, rowKey: "tool-water-inject" },
  ],
  materials: [
    { rowKey: "material-water-primary", label: "Water primary", assetRevision: 5, state: "unbound" },
    { rowKey: "material-water-foam", label: "Foam layer", assetRevision: 2, state: "ready" },
  ],
  history: [],
  diagnostics: [
    { rowKey: "diag-revision", label: "Revision", value: "42", tone: "normal" },
    { rowKey: "diag-gpu", label: "GPU", value: "ready", tone: "normal" },
  ],
};

test("complex terrain editor compiles into one fragment with typed intents", async () => {
  const fragmentPromise = new Promise((resolve, reject) => {
    const root = createNeonRoot({ submit: resolve, onError: reject });
    root.render(React.createElement(TerrainEditorSurface, { snapshot }));
  });
  const fragment = await fragmentPromise;
  assert.equal(fragment.fragment_id, "surface.editor.terrain-workbench");
  assert.equal(fragment.revision, 42);
  assert.ok(fragment.root.children.length >= 5);
  assert.ok(fragment.effects.some((effect) => effect.intent?.action === "terrain.tool.select"));
  assert.ok(fragment.effects.some((effect) => effect.intent?.action === "resource.pick.open"));
  const serialized = JSON.stringify(fragment);
  assert.equal(serialized.includes("elementId"), false);
  assert.equal(serialized.includes("640060"), false);
});
