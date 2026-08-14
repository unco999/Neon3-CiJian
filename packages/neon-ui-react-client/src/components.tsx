import React from "react";
import type { NodeProps, SurfaceProps } from "./protocol.js";

export function Surface(props: SurfaceProps) {
  return React.createElement("neon-surface", props, props.children);
}

export function Panel(props: NodeProps) {
  return React.createElement("neon-panel", props, props.children);
}

export function Label(props: NodeProps) {
  return React.createElement("neon-label", props, props.children);
}

export function Button(props: NodeProps) {
  return React.createElement("neon-button", props, props.children);
}

export function Image(props: Omit<NodeProps, "children">) {
  return React.createElement("neon-image", props);
}
