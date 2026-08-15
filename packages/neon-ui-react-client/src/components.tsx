import React from "react";
import type { NodeProps, SurfaceProps, TextInputProps } from "./protocol.js";

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

export function RenderSurface(props: Omit<NodeProps, "children">) {
  return React.createElement("neon-render-surface", props);
}

export function TextInput(props: TextInputProps) {
  return React.createElement("neon-text-input", props);
}
