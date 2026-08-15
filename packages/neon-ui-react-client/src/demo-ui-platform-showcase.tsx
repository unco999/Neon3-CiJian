import React from "react";
import { NeonUiClient } from "./client.js";
import { UiPlatformShowcaseSurface, type ShowcasePhase } from "./examples/UiPlatformShowcaseSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:ui-platform -- <ui-runtime-port>");

const client = new NeonUiClient(createLoopbackTransport({ port }), "ui-platform-showcase");
const root = createNeonRoot({
  submit: async fragment => {
    const response = await client.submitFragment(fragment);
    if (response.status !== "accepted") throw new Error(response.error?.message ?? "UI showcase fragment rejected");
    return response;
  },
  onError: error => console.error(error),
});

let revision = 1;
let phase: ShowcasePhase = "overview";
function render() {
  root.render(React.createElement(UiPlatformShowcaseSurface, { revision, phase }));
}

render();
console.log(JSON.stringify({ status: "ready", surface_id: "surface.ui-platform-showcase", revision, phase }));
setInterval(() => {
  phase = phase === "overview" ? "motion" : "overview";
  revision += 1;
  render();
  console.log(JSON.stringify({ status: "submitted", surface_id: "surface.ui-platform-showcase", revision, phase }));
}, 2400);
