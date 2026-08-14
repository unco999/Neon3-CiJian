import React from "react";
import { NeonUiClient } from "./client.js";
import { TerrainEditorSurface, type TerrainEditorSnapshot } from "./examples/TerrainEditorSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:terrain -- <ui-runtime-port>");

const snapshot: TerrainEditorSnapshot = {
  projectRevision: 42,
  terrainId: 12,
  terrainName: "North Basin",
  mode: "water_paint",
  bindingState: "needs_selection",
  tools: [
    { rowKey: "tool-select", mode: "select", label: "Select", active: false },
    { rowKey: "tool-raise", mode: "raise", label: "Raise", active: false },
    { rowKey: "tool-smooth", mode: "smooth", label: "Smooth", active: false },
    { rowKey: "tool-water-inject", mode: "water_inject", label: "Water inject", active: true },
  ],
  materials: [
    { rowKey: "material-water-primary", label: "Water primary", assetRevision: 5, state: "unbound" },
    { rowKey: "material-water-foam", label: "Foam layer", assetRevision: 2, state: "ready" },
  ],
  history: [],
  diagnostics: [
    { rowKey: "diag-revision", label: "Project revision", value: "42", tone: "normal" },
    { rowKey: "diag-binding", label: "Water binding", value: "needs selection", tone: "warning" },
    { rowKey: "diag-gpu", label: "GPU readiness", value: "ready", tone: "normal" },
  ],
};

const client = new NeonUiClient(createLoopbackTransport({ port }));
await new Promise<void>((resolve, reject) => {
  const root = createNeonRoot({
    submit: async (fragment) => {
      const response = await client.submitFragment(fragment);
      if (response.status !== "accepted") throw new Error(response.error?.message ?? "UI fragment was rejected");
      console.log(JSON.stringify({ request_id: response.request_id, status: response.status, revision: response.revision }));
      resolve();
      return response;
    },
    onError: reject,
  });
  root.render(React.createElement(TerrainEditorSurface, { snapshot }));
});
