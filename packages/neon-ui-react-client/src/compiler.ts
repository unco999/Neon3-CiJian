import type React from "react";
import type { AssetRef, Bounds, JsonValue, NodeKey, RenderSurfaceRef, UiFragment, UiIntent, UiIntentSpec, UiLayout, UiNode, UiStyle, UiSurfaceEvent, UiTransition } from "./protocol.js";

type HostType = "neon-surface" | "neon-panel" | "neon-label" | "neon-button" | "neon-image" | "neon-render-surface";
type HostProps = Record<string, unknown>;
type TextNode = { kind: "text"; value: string; hidden: boolean };
export type HostNode = { kind: "host"; type: HostType; props: HostProps; children: Child[]; hidden: boolean };
export type Child = HostNode | TextNode;

export type NeonContainer = {
  children: Child[];
};

export function createHost(type: string, props: HostProps): HostNode {
  if (!isHostType(type)) throw new Error(`unsupported Neon host type: ${type}`);
  return { kind: "host", type, props: sanitizeProps(props), children: [], hidden: false };
}

export function createText(value: string): TextNode {
  return { kind: "text", value, hidden: false };
}

export function append(parent: { children: Child[] }, child: Child) {
  parent.children.push(child);
}

export function insert(parent: { children: Child[] }, child: Child, before: Child) {
  const index = parent.children.indexOf(before);
  if (index < 0) parent.children.push(child);
  else parent.children.splice(index, 0, child);
}

export function remove(parent: { children: Child[] }, child: Child) {
  const index = parent.children.indexOf(child);
  if (index >= 0) parent.children.splice(index, 1);
}

export function compileContainer(container: NeonContainer): UiFragment {
  const surface = container.children.filter((child): child is HostNode => child.kind === "host" && child.type === "neon-surface");
  if (surface.length !== 1 || container.children.length !== 1) throw new Error("a Neon root must contain exactly one Surface");
  const node = surface[0];
  const surfaceId = requiredString(node.props.surfaceId, "surfaceId");
  const revision = requiredRevision(node.props.revision);
  const rootBounds = requiredBounds(node.props.bounds, "Surface.bounds");
  const effects: UiFragment["effects"] = [];
  const root: UiNode = {
    node_id: nodeId(surfaceId, "@root"), kind: "panel", bounds: rootBounds, layout: null,
    visible: boolProp(node.props.visible, true), enabled: boolProp(node.props.enabled, true),
    text_key: null, text: null, image: null, surface: null, style: styleProp(node.props.style), enter_transition: null,
    children: compileChildren(node.children, surfaceId, "@root", effects),
  };
  return { fragment_id: surfaceId, revision, root, effects };
}

function compileChildren(children: Child[], surfaceId: string, parentPath: string, effects: UiFragment["effects"]): UiNode[] {
  const nodes: UiNode[] = [];
  const seen = new Set<string>();
  for (const child of children) {
    if (child.kind !== "host" || child.type === "neon-surface") continue;
    const key = requiredNodeKey(child.props.nodeKey);
    if (seen.has(key)) throw new Error(`duplicate nodeKey among siblings: ${key}`);
    seen.add(key);
    const path = `${parentPath}/${key}`;
    const kind = hostKind(child.type);
    const intentSpec = child.props.intent as UiIntentSpec | undefined;
    const surfaceEvent = child.props.event as UiSurfaceEvent | undefined;
    if (intentSpec && surfaceEvent) throw new Error(`${path} cannot declare both intent and event`);
    if (surfaceEvent && kind !== "button") throw new Error(`${path}.event is only valid on Button`);
    const intent = surfaceEvent ? toSurfaceEventIntent(surfaceId, surfaceEvent) : intentSpec ? toWireIntent(intentSpec) : undefined;
    const id = nodeId(surfaceId, path);
    if (intent) {
      validateIntent(intent);
      effects.push({ kind: "bound_semantic_intent", node_id: id, intent });
    }
    const asset = child.props.asset as AssetRef | undefined;
    if (kind === "image" && !asset) throw new Error(`Image ${key} requires asset`);
    const surface = child.props.surface as RenderSurfaceRef | undefined;
    if (kind === "render_surface" && (!surface || typeof surface.target_id !== "string" || !surface.target_id.trim())) {
      throw new Error(`RenderSurface ${key} requires surface.target_id`);
    }
    const text = textProp(child);
    const textKey = typeof child.props.textKey === "string" ? child.props.textKey : null;
    nodes.push({
      node_id: id, kind, bounds: requiredBounds(child.props.bounds, `${path}.bounds`),
      layout: (child.props.layout as UiLayout | undefined) ?? null,
      visible: boolProp(child.props.visible, true), enabled: boolProp(child.props.enabled, true),
      text_key: textKey, text: textKey ? { kind: "key", key: textKey, arguments: (child.props.textArguments as JsonValue | undefined) ?? {} } : text,
      image: asset ?? null, surface: surface ?? null, style: styleProp(child.props.style), enter_transition: transitionProp(child.props.enterTransition),
      children: compileChildren(child.children, surfaceId, path, effects),
    });
  }
  return nodes;
}

function textProp(node: HostNode): UiNode["text"] {
  const text = node.children.filter((child): child is TextNode => child.kind === "text").map((child) => child.value).join("");
  return text.length > 0 ? { kind: "literal", value: text } : null;
}

function sanitizeProps(props: HostProps): HostProps {
  for (const forbidden of ["id", "elementId", "targetId", "currentTargetId", "onClick", "onPointerDown", "onPointerUp", "onPointerMove"]) {
    if (forbidden in props) throw new Error(`${forbidden} is not part of the Neon3 declaration contract`);
  }
  const { children: _children, key: _key, ref: _ref, ...clean } = props;
  return clean;
}

function hostKind(type: HostType): UiNode["kind"] {
  switch (type) { case "neon-panel": return "panel"; case "neon-label": return "label"; case "neon-button": return "button"; case "neon-image": return "image"; case "neon-render-surface": return "render_surface"; default: throw new Error(`invalid node type: ${type}`); }
}

function isHostType(type: string): type is HostType { return ["neon-surface", "neon-panel", "neon-label", "neon-button", "neon-image", "neon-render-surface"].includes(type); }
function requiredString(value: unknown, name: string): string { if (typeof value !== "string" || !value.trim()) throw new Error(`${name} is required`); return value; }
function requiredNodeKey(value: unknown): NodeKey { const key = requiredString(value, "nodeKey"); if (!/^[a-z][a-z0-9._-]*$/.test(key)) throw new Error(`nodeKey must be a semantic kebab-case token: ${key}`); return key; }
function requiredRevision(value: unknown): number { if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error("Surface.revision must be a non-negative integer"); return value as number; }
function requiredBounds(value: unknown, name: string): Bounds { if (!value || typeof value !== "object") throw new Error(`${name} is required`); const bounds = value as Bounds; if (![bounds.x, bounds.y, bounds.width, bounds.height].every((part) => typeof part === "number" && Number.isFinite(part)) || bounds.width < 0 || bounds.height < 0) throw new Error(`${name} is invalid`); return bounds; }
function boolProp(value: unknown, fallback: boolean): boolean { return value === undefined ? fallback : value === true; }
function styleProp(style: unknown): UiStyle { return (style as UiStyle | undefined) ?? {}; }
function toWireIntent(spec: UiIntentSpec): UiIntent { validateIntent(spec); return { kind: "invoke", action: spec.action, params: spec.params }; }
function validateIntent(intent: UiIntentSpec) { if (!intent || typeof intent.action !== "string" || !intent.action.trim()) throw new Error("intent.action is required"); }
function toSurfaceEventIntent(surfaceId: string, event: UiSurfaceEvent): UiIntent {
  if (!event || typeof event !== "object" || typeof event.type !== "string") throw new Error("Button.event is invalid");
  const params = { schema_version: 1, surface_id: surfaceId, event };
  if (event.type === "DIAGNOSTICS_TOGGLE") return { kind: "invoke", action: "ui.surface.event", params };
  if (event.type === "INSPECTOR_TAB_SELECT" && ["overview", "materials", "history"].includes(event.tab)) return { kind: "invoke", action: "ui.surface.event", params };
  throw new Error("Button.event is invalid");
}
function transitionProp(value: unknown): UiNode["enter_transition"] {
  if (!value) return null;
  const transition = value as UiTransition;
  if (!Number.isSafeInteger(transition.durationMs) || transition.durationMs <= 0) throw new Error("enterTransition.durationMs must be positive");
  const opacity = transition.from?.opacity;
  if (opacity !== undefined && (typeof opacity !== "number" || opacity < 0 || opacity > 1)) throw new Error("enterTransition.from.opacity is invalid");
  return { delay_ms: 0, duration_ms: transition.durationMs, easing: transition.easing ?? "ease_out", from: { bounds: transition.from?.bounds ?? null, background_color: null, border_color: null, border_width: null, corner_radius: null, opacity: opacity ?? null } };
}
function equalJson(left: unknown, right: unknown): boolean { return JSON.stringify(left) === JSON.stringify(right); }

function nodeId(surfaceId: string, path: string): string {
  let hash = 2166136261;
  for (const byte of new TextEncoder().encode(`${surfaceId}\0${path}`)) { hash ^= byte; hash = Math.imul(hash, 16777619); }
  return `node.${(hash >>> 0).toString(16).padStart(8, "0")}`;
}
