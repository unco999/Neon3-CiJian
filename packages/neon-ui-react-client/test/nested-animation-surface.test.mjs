import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { createNeonRoot } from "../dist/renderer.js";
import { UiNestedAnimationSurface } from "../dist/index.js";

async function fragmentFor(revision, phase) {
  return new Promise((resolve, reject) => {
    const root = createNeonRoot({ submit: resolve, onError: reject });
    root.render(React.createElement(UiNestedAnimationSurface, { revision, phase }));
  });
}

function findPanelWithLabel(node, text) {
  if (node.children.some(child => child.text?.value === text)) return node;
  for (const child of node.children) {
    const found = findPanelWithLabel(child, text);
    if (found) return found;
  }
  return null;
}

test("nested animation surface preserves identity across parent and child target changes", async () => {
  const overview = await fragmentFor(1, "overview");
  const focus = await fragmentFor(2, "focus");
  const release = await fragmentFor(3, "release");
  const overviewWorkspace = overview.root.children[1];
  const focusWorkspace = focus.root.children[1];
  const releaseWorkspace = release.root.children[1];
  const overviewPrimary = findPanelWithLabel(overview.root, "PRIMARY VIEW");
  const focusPrimary = findPanelWithLabel(focus.root, "PRIMARY VIEW");
  const releasePrimary = findPanelWithLabel(release.root, "PRIMARY VIEW");
  assert.equal(overviewWorkspace.node_id, focusWorkspace.node_id);
  assert.equal(focusWorkspace.node_id, releaseWorkspace.node_id);
  assert.notDeepEqual(overviewWorkspace.bounds, focusWorkspace.bounds);
  assert.equal(overviewPrimary.node_id, focusPrimary.node_id);
  assert.equal(focusPrimary.node_id, releasePrimary.node_id);
  assert.notDeepEqual(overviewPrimary.bounds, focusPrimary.bounds);
  assert.notDeepEqual(focusPrimary.bounds, releasePrimary.bounds);
  assert.equal(focusPrimary.enter_transition.duration_ms, 680);
  assert.ok(focus.root.children[1].children.length >= 3);
});
