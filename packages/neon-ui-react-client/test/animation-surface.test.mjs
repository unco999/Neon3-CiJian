import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { createNeonRoot } from "../dist/renderer.js";
import { UiAnimationSurface } from "../dist/index.js";

async function fragmentFor(revision, phase) {
  return new Promise((resolve, reject) => {
    const root = createNeonRoot({ submit: resolve, onError: reject });
    root.render(React.createElement(UiAnimationSurface, { revision, phase }));
  });
}

test("React animation surface retains identity and changes only target state", async () => {
  const left = await fragmentFor(1, "left");
  const right = await fragmentFor(2, "right");
  const leftCard = left.root.children.find(node => node.children.some(child => child.text?.value === "LEFT TARGET"));
  const rightCard = right.root.children.find(node => node.children.some(child => child.text?.value === "RIGHT TARGET"));
  assert.equal(leftCard.node_id, rightCard.node_id);
  assert.equal(leftCard.bounds.x, 120);
  assert.equal(rightCard.bounds.x, 940);
  assert.equal(rightCard.enter_transition.duration_ms, 900);
  assert.equal(rightCard.enter_transition.easing, "ease_in_out");
});
