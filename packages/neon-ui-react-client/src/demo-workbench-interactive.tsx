import React from "react";
import { NeonUiClient } from "./client.js";
import { UiWorkbenchSurface } from "./examples/UiWorkbenchSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";
import type { UiSurfaceSnapshot } from "./protocol.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:workbench:interactive -- <ui-runtime-port>");
const client = new NeonUiClient(createLoopbackTransport({ port }));
const wait = (milliseconds: number) => new Promise<void>(resolve => setTimeout(resolve, milliseconds));
const root = createNeonRoot({
  submit: async fragment => {
    const response = await client.submitFragment(fragment);
    if (response.status !== "accepted") throw new Error(response.error?.message ?? "workbench fragment rejected");
    return response;
  },
  onError: error => console.error(error),
});

function render(snapshot: UiSurfaceSnapshot) {
  root.render(React.createElement(UiWorkbenchSurface, {
    snapshot: {
      revision: snapshot.revision,
      diagnosticsExpanded: snapshot.value.diagnostics === "expanded",
      inspectorTab: snapshot.value.inspector.tab,
    },
  }));
}

let snapshot = await client.surfaceSnapshot();
render(snapshot);
console.log(JSON.stringify({ status: "ready", surface_revision: snapshot.revision, state: snapshot.value }));
for (;;) {
  await wait(50);
  const next = await client.surfaceSnapshot();
  if (next.revision === snapshot.revision) continue;
  snapshot = next;
  render(snapshot);
  console.log(JSON.stringify({ status: "rendered", surface_revision: snapshot.revision, state: snapshot.value }));
}
