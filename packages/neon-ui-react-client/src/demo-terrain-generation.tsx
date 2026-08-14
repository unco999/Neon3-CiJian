import React from "react";
import { NeonUiClient } from "./client.js";
import { TerrainGenerationSurface } from "./examples/TerrainGenerationSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";
import type { AiTerrainPanelSnapshot } from "./protocol.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:terrain-generation -- <ui-runtime-port>");

const client = new NeonUiClient(createLoopbackTransport({ port }), "terrain-generation-demo");
const wait = (milliseconds: number) => new Promise<void>((resolve) => setTimeout(resolve, milliseconds));
const root = createNeonRoot({
  submit: async (fragment) => {
    const response = await client.submitFragment(fragment);
    if (response.status !== "accepted") throw new Error(response.error?.message ?? "terrain generation fragment rejected");
    return response;
  },
  onError: (error) => console.error(error),
});

function render(snapshot: AiTerrainPanelSnapshot) {
  root.render(React.createElement(TerrainGenerationSurface, {
    snapshot: {
      revision: snapshot.revision,
      condition: snapshot.condition,
      guidance: snapshot.guidance,
      steps: snapshot.steps,
      seed: snapshot.seed,
      lastSeed: snapshot.last_seed,
      size: snapshot.size,
      targetId: snapshot.target_id,
      state: snapshot.state,
      jobId: snapshot.job_id,
      elapsedMs: snapshot.elapsed_ms,
      errorCode: snapshot.error_code,
    },
  }));
}

let snapshot = await client.aiTerrainSnapshot();
render(snapshot);
console.log(JSON.stringify({ status: "ready", surface_revision: snapshot.revision, condition: snapshot.condition }));
for (;;) {
  await wait(50);
  const next = await client.aiTerrainSnapshot();
  if (next.revision === snapshot.revision) continue;
  snapshot = next;
  render(snapshot);
  console.log(JSON.stringify({ status: "rendered", surface_revision: snapshot.revision, generation_state: snapshot.state, job_id: snapshot.job_id }));
}
