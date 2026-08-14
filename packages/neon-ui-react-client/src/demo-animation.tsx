import React from "react";
import { NeonUiClient } from "./client.js";
import { UiAnimationSurface, type UiAnimationPhase } from "./examples/UiAnimationSurface.js";
import { createLoopbackTransport } from "./node-transport.js";
import { createNeonRoot } from "./renderer.js";

const port = Number(process.argv[2] ?? "40100");
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) throw new Error("usage: npm run demo:animation -- <ui-runtime-port>");
const client = new NeonUiClient(createLoopbackTransport({ port }));
const wait = (milliseconds: number) => new Promise<void>(resolve => setTimeout(resolve, milliseconds));

async function submit(revision: number, phase: UiAnimationPhase) {
  await new Promise<void>((resolve, reject) => {
    const root = createNeonRoot({
      submit: async fragment => {
        const response = await client.submitFragment(fragment);
        if (response.status !== "accepted") throw new Error(response.error?.message ?? "animation fragment rejected");
        console.log(JSON.stringify({ revision, phase, request_id: response.request_id, status: response.status }));
        resolve();
        return response;
      },
      onError: reject,
    });
    root.render(React.createElement(UiAnimationSurface, { revision, phase }));
  });
}

await submit(1, "left");
await wait(1500);
await submit(2, "right");
await wait(1500);
await submit(3, "left");
await wait(1500);
