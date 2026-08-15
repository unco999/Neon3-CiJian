import test from "node:test";
import assert from "node:assert/strict";
import { append, compileContainer, createHost, createText } from "../dist/compiler.js";
import React from "react";
import { createNeonRoot } from "../dist/renderer.js";

function surface(children, revision = 1) {
  const root = { children: [] };
  const node = createHost("neon-surface", {
    surfaceId: "surface.terrain.inspector",
    revision,
    bounds: { x: 0, y: 0, width: 420, height: 240 },
  });
  for (const child of children) append(node, child);
  append(root, node);
  return root;
}

function panel(nodeKey, children = [], extra = {}) {
  const node = createHost("neon-panel", { nodeKey, bounds: { x: 0, y: 0, width: 100, height: 40 }, ...extra });
  for (const child of children) append(node, child);
  return node;
}

test("compiles stable semantic node paths without numeric element ids", () => {
  const button = createHost("neon-button", {
    nodeKey: "water-tool",
    bounds: { x: 4, y: 4, width: 90, height: 28 },
    intent: { action: "terrain.tool.select", params: { tool: "water_inject" } },
  });
  append(button, createText("Water tool"));
  const fragment = compileContainer(surface([panel("toolbar", [button])]));
  assert.equal(fragment.fragment_id, "surface.terrain.inspector");
  assert.match(fragment.root.children[0].node_id, /^node\.[0-9a-f]{8}$/);
  assert.equal(fragment.root.children[0].children[0].text.value, "Water tool");
  assert.deepEqual(fragment.effects, [{ kind: "bound_semantic_intent", node_id: fragment.root.children[0].children[0].node_id, intent: { kind: "invoke", action: "terrain.tool.select", params: { tool: "water_inject" } } }]);
  assert.equal(JSON.stringify(fragment).includes("elementId"), false);
});

test("Button event compiles to the typed UI surface intent", () => {
  const button = createHost("neon-button", {
    nodeKey: "materials-tab",
    bounds: { x: 4, y: 4, width: 90, height: 28 },
    event: { type: "INSPECTOR_TAB_SELECT", tab: "materials" },
  });
  const fragment = compileContainer(surface([panel("toolbar", [button])]));
  assert.deepEqual(fragment.effects, [{
    kind: "bound_semantic_intent",
    node_id: fragment.root.children[0].children[0].node_id,
    intent: { kind: "invoke", action: "ui.surface.event", params: { schema_version: 1, surface_id: "surface.terrain.inspector", event: { type: "INSPECTOR_TAB_SELECT", tab: "materials" } } },
  }]);
  assert.throws(() => compileContainer(surface([panel("not-a-button", [], { event: { type: "DIAGNOSTICS_TOGGLE" } })])), /only valid on Button/);
  assert.throws(() => compileContainer(surface([createHost("neon-button", { nodeKey: "ambiguous", bounds: { x: 0, y: 0, width: 20, height: 20 }, event: { type: "DIAGNOSTICS_TOGGLE" }, intent: { action: "ui.surface.event", params: {} } })])), /both intent and event/);
});

test("node ids are stable across rerenders and revisions", () => {
  const make = () => compileContainer(surface([panel("toolbar", [panel("water-tool")])])).root.children[0].children[0].node_id;
  assert.equal(make(), make());
});

test("duplicate sibling node keys and legacy numeric ids are rejected", () => {
  assert.throws(() => compileContainer(surface([panel("toolbar", [panel("same"), panel("same")])])), /duplicate nodeKey/);
  assert.throws(() => compileContainer(surface([createHost("neon-panel", { id: 640060, nodeKey: "panel", bounds: { x: 0, y: 0, width: 10, height: 10 } })])), /id is not/);
});

test("RenderSurface compiles an opaque renderer target reference", () => {
  const preview = createHost("neon-render-surface", {
    nodeKey: "terrain-preview",
    bounds: { x: 0, y: 0, width: 128, height: 128 },
    surface: { target_id: "ai.terrain.preview" },
  });
  const fragment = compileContainer(surface([preview]));
  assert.equal(fragment.root.children[0].kind, "render_surface");
  assert.deepEqual(fragment.root.children[0].surface, { target_id: "ai.terrain.preview" });
  assert.equal(JSON.stringify(fragment).includes("texture"), false);
  assert.throws(
    () => compileContainer(surface([createHost("neon-render-surface", { nodeKey: "bad", bounds: { x: 0, y: 0, width: 1, height: 1 } })])),
    /requires surface.target_id/,
  );
});

test("TextInput declares a bounded local editor and a commit intent", () => {
  const input = createHost("neon-text-input", {
    nodeKey: "title", bounds: { x: 4, y: 4, width: 180, height: 28 }, value: "Draft", maxLength: 256,
    intent: { action: "ui.showcase.text.commit", params: {} },
  });
  const fragment = compileContainer(surface([input]));
  assert.equal(fragment.root.children[0].kind, "text_input");
  assert.deepEqual(fragment.root.children[0].text, { kind: "literal", value: "Draft" });
  assert.equal(fragment.effects[0].intent.action, "ui.showcase.text.commit");
  assert.throws(() => compileContainer(surface([createHost("neon-text-input", { nodeKey: "bad", bounds: { x: 0, y: 0, width: 1, height: 1 }, value: "x" })])), /intent is required/);
  assert.throws(() => compileContainer(surface([createHost("neon-text-input", { nodeKey: "long", bounds: { x: 0, y: 0, width: 1, height: 1 }, value: "x", maxLength: 64, intent: { action: "ui.showcase.text.commit", params: {} } })])), /fixed at 256/);
});

test("Flex declarations preserve wire names and reject invalid factors", () => {
  const child = panel("auto-label", [], { layout: { flex_basis: null, flex_grow: 1, flex_shrink: 0, align_self: "stretch" } });
  const root = panel("row", [child], { layout: { mode: "row", justify_content: "space_between", align_items: "center", gap: 8 } });
  const fragment = compileContainer(surface([root]));
  assert.deepEqual(fragment.root.children[0].layout, { mode: "row", justify_content: "space_between", align_items: "center", gap: 8 });
  assert.equal(fragment.root.children[0].children[0].layout.flex_grow, 1);
  assert.throws(() => compileContainer(surface([panel("bad", [], { layout: { flex_grow: -1 } })])), /flex_grow is invalid/);
});

test("React reconciler produces a protocol fragment without DOM", async () => {
  const fragmentPromise = new Promise((resolve, reject) => {
    const root = createNeonRoot({
      submit: (fragment) => { resolve(fragment); },
      onError: reject,
    });
    root.render(React.createElement(
      "neon-surface",
      { surfaceId: "surface.react.case", revision: 4, bounds: { x: 0, y: 0, width: 240, height: 120 } },
      React.createElement("neon-label", { nodeKey: "title", bounds: { x: 8, y: 8, width: 120, height: 24 } }, "React declaration"),
    ));
  });
  const fragment = await fragmentPromise;
  assert.equal(fragment.fragment_id, "surface.react.case");
  assert.equal(fragment.revision, 4);
  assert.equal(fragment.root.children[0].text.value, "React declaration");
});

test("client submits declarations to ui-runtime, never directly to renderer", async () => {
  let request;
  const transport = { call: async (value) => { request = value; return { request_id: value.request_id, status: "accepted", revision: 1, result: null, snapshot: null, error: null }; } };
  const { NeonUiClient } = await import("../dist/client.js");
  const client = new NeonUiClient(transport, "react-test");
  await client.submitFragment({ fragment_id: "surface.test", revision: 1, root: { node_id: "node.root", kind: "panel", bounds: { x: 0, y: 0, width: 1, height: 1 }, layout: null, visible: true, enabled: true, text_key: null, text: null, image: null, surface: null, style: {}, enter_transition: null, children: [] }, effects: [] });
  assert.equal(request.target, "ui-runtime");
  assert.equal(request.method, "ui.fragment.submit");
  assert.equal(request.client.origin, "neon-ui-react-client");
});
