import type React from "react";

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type Revision = number;
export type SurfaceId = string;
export type NodeKey = string;
export type Bounds = { x: number; y: number; width: number; height: number };
export type Rgba = [number, number, number, number];

export type AssetRef = {
  project_id: string;
  asset_id: number;
  revision: number;
  kind: string;
};

export type UiStyle = {
  background_color?: Rgba;
  border_color?: Rgba;
  border_width?: number;
  corner_radius?: number;
  opacity?: number;
};

export type UiLayout = {
  mode?: "absolute" | "overlay" | "row" | "column";
  padding?: [number, number, number, number];
  margin?: [number, number, number, number];
  gap?: number;
  min_size?: [number, number] | null;
  max_size?: [number, number] | null;
  preferred_size?: [number, number] | null;
  clip?: boolean;
  scroll_offset?: [number, number];
};

export type UiIntentSpec = { action: string; params: JsonValue };
export type UiIntent = { kind: "invoke"; action: string; params: JsonValue };
export type UiTransition = { durationMs: number; easing?: "linear" | "ease_in" | "ease_out" | "ease_in_out"; from?: { bounds?: Bounds; opacity?: number } };

export type UiNode = {
  node_id: string;
  kind: "panel" | "label" | "button" | "image";
  bounds: Bounds;
  layout: UiLayout | null;
  visible: boolean;
  enabled: boolean;
  text_key: string | null;
  text: { kind: "literal"; value: string } | { kind: "key"; key: string; arguments: JsonValue } | null;
  image: AssetRef | null;
  style: UiStyle;
  enter_transition: { delay_ms: number; duration_ms: number; easing: "linear" | "ease_in" | "ease_out" | "ease_in_out"; from: { bounds: Bounds | null; background_color: null; border_color: null; border_width: null; corner_radius: null; opacity: number | null } } | null;
  children: UiNode[];
};

export type UiEffect = { kind: "semantic_action"; action: string } | { kind: "semantic_intent"; intent: UiIntent } | { kind: "bound_semantic_intent"; node_id: string; intent: UiIntent };
export type UiFragment = {
  fragment_id: SurfaceId;
  revision: Revision;
  root: UiNode;
  effects: UiEffect[];
};

export type UiFragmentSubmission = { schema_version: 1; fragment: UiFragment };
export type UiCommand = { kind: "submit_fragment"; submission: UiFragmentSubmission };

export type RpcRequest = {
  protocol: "neon3.rpc";
  version: { major: 1; minor: number };
  request_id: string;
  client: { kind: "ui_react_client"; instance_id: string; pid: number; origin: string };
  target: string;
  method: string;
  params: JsonValue;
  expected_revision: Revision | null;
  idempotency_key: string | null;
};

export type RpcError = {
  code: string;
  message: string;
  current_revision: Revision | null;
  object_id: string | null;
};

export type RpcResponse = {
  request_id: string;
  status: "accepted" | "rejected" | "failed";
  revision: Revision | null;
  result: JsonValue;
  snapshot: JsonValue;
  error: RpcError | null;
};

export type RpcTransport = {
  call(request: RpcRequest): Promise<RpcResponse>;
};

export type UiSurfaceEvent = { type: "DIAGNOSTICS_TOGGLE" } | { type: "INSPECTOR_TAB_SELECT"; tab: "overview" | "materials" | "history" };
export type UiSurfaceSnapshot = { revision: Revision; value: { diagnostics: "collapsed" | "expanded"; inspector: { tab: "overview" | "materials" | "history" } }; available_events: Array<UiSurfaceEvent["type"]> };

export type SubmitFragment = (fragment: UiFragment) => Promise<RpcResponse> | RpcResponse | void;

export type SurfaceProps = {
  surfaceId: SurfaceId;
  revision: Revision;
  bounds: Bounds;
  visible?: boolean;
  enabled?: boolean;
  style?: UiStyle;
  children?: React.ReactNode;
};

export type NodeProps = {
  nodeKey: NodeKey;
  bounds: Bounds;
  visible?: boolean;
  enabled?: boolean;
  layout?: UiLayout;
  style?: UiStyle;
  textKey?: string;
  textArguments?: JsonValue;
  asset?: AssetRef;
  intent?: UiIntentSpec;
  event?: UiSurfaceEvent;
  enterTransition?: UiTransition;
  children?: React.ReactNode;
};
