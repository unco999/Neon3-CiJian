import React from "react";
import { NeonUiClient } from "./client.js";
import { UiWorkbenchSurface } from "./examples/UiWorkbenchSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";
import type { UiSurfaceSnapshot } from "./protocol.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:workbench -- <ui-runtime-port>");
const client = new NeonUiClient(createLoopbackTransport({ port }));

async function submit(snapshot: UiSurfaceSnapshot) {
  await new Promise<void>((resolve, reject) => {
    const root = createNeonRoot({
      submit: async fragment => {
        const response = await client.submitFragment(fragment);
        if (response.status !== "accepted") throw new Error(response.error?.message ?? "fragment rejected");
        resolve();
        return response;
      },
      onError: reject,
    });
    root.render(React.createElement(UiWorkbenchSurface, { snapshot: { revision: snapshot.revision, diagnosticsExpanded: snapshot.value.diagnostics === "expanded", inspectorTab: snapshot.value.inspector.tab } }));
  });
}

let snapshot = await client.surfaceSnapshot();
await submit(snapshot);
snapshot = await client.surfaceEvent({ type: "DIAGNOSTICS_TOGGLE" }, snapshot);
await submit(snapshot);
snapshot = await client.surfaceEvent({ type: "INSPECTOR_TAB_SELECT", tab: "materials" }, snapshot);
await submit(snapshot);
console.log(JSON.stringify({ status: "accepted", surface_revision: snapshot.revision, state: snapshot.value }));
