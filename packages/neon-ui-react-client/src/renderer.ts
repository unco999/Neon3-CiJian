import React from "react";
import Reconciler from "react-reconciler";
import { ConcurrentRoot, DefaultEventPriority } from "react-reconciler/constants.js";
import { append, compileContainer, createHost, createText, insert, remove, type HostNode, type NeonContainer } from "./compiler.js";
import type { SubmitFragment } from "./protocol.js";

type NeonChild = ReturnType<typeof createHost> | ReturnType<typeof createText>;
type NeonRootOptions = { submit: SubmitFragment; onError?: (error: unknown) => void };

let updatePriority = DefaultEventPriority;
const context = {};

const hostConfig: Record<string, any> = {
  isPrimaryRenderer: false, supportsMutation: true, supportsPersistence: false, supportsHydration: false,
  supportsMicrotasks: true, noTimeout: -1, NotPendingTransition: null, HostTransitionContext: { _currentValue: null, _currentValue2: null },
  now: Date.now, scheduleTimeout: setTimeout, cancelTimeout: clearTimeout, scheduleMicrotask: queueMicrotask,
  getCurrentEventPriority: () => updatePriority, getCurrentUpdatePriority: () => updatePriority,
  setCurrentUpdatePriority: (priority: number) => { updatePriority = priority; }, resolveUpdatePriority: () => updatePriority,
  trackSchedulerEvent: () => {}, resolveEventType: () => null, resolveEventTimeStamp: () => performance.now(),
  shouldAttemptEagerTransition: () => false, getInstanceFromNode: () => null, beforeActiveInstanceBlur: () => {}, afterActiveInstanceBlur: () => {},
  preparePortalMount: () => {}, prepareScopeUpdate: () => {}, getInstanceFromScope: () => null, requestPostPaintCallback: () => 0,
  detachDeletedInstance: () => {}, maySuspendCommit: () => false, maySuspendCommitOnUpdate: () => false, maySuspendCommitInSyncRender: () => false,
  preloadInstance: () => true, startSuspendingCommit: () => {}, suspendInstance: () => {}, suspendOnActiveViewTransition: () => false,
  waitForCommitToBeReady: () => null, getSuspendedCommitReason: () => null, resetFormInstance: () => {}, bindToConsole: () => () => {},
  supportsTestSelectors: false, findFiberRoot: () => null, getBoundingRect: () => ({ x: 0, y: 0, width: 0, height: 0 }),
  getTextContent: (instance: NeonChild) => instance.kind === "text" ? instance.value : "", isHiddenSubtree: (instance: NeonChild) => instance.hidden,
  matchAccessibilityRole: () => false, setFocusIfFocusable: () => false,
  setupIntersectionObserver: () => ({ disconnect() {}, observe() {}, unobserve() {} }),
  getRootHostContext: () => context, getChildHostContext: () => context, getPublicInstance: (instance: NeonChild) => instance,
  prepareForCommit: () => null,
  resetAfterCommit: (container: NeonContainer & { __submit?: SubmitFragment; __onError?: (error: unknown) => void }) => {
    const fragment = compileContainer(container);
    Promise.resolve(container.__submit?.(fragment)).catch((error) => container.__onError?.(error));
  },
  createInstance: (type: string, props: Record<string, unknown>) => createHost(type, props), createTextInstance: (text: string) => createText(text),
  appendInitialChild: (parent: NeonChild, child: NeonChild) => appendHost(parent, child), finalizeInitialChildren: () => false, shouldSetTextContent: () => false,
  appendChild: (parent: NeonChild, child: NeonChild) => appendHost(parent, child), appendChildToContainer: append,
  insertBefore: (parent: NeonChild, child: NeonChild, before: NeonChild) => insertHost(parent, child, before), insertInContainerBefore: insert,
  removeChild: (parent: NeonChild, child: NeonChild) => removeHost(parent, child), removeChildFromContainer: remove, clearContainer: (container: NeonContainer) => { container.children.length = 0; },
  resetTextContent: (instance: ReturnType<typeof createHost>) => { instance.children.length = 0; },
  commitTextUpdate: (instance: ReturnType<typeof createText>, _old: string, next: string) => { instance.value = next; },
  commitUpdate: (instance: ReturnType<typeof createHost>, _type: string, _old: Record<string, unknown>, next: Record<string, unknown>) => { instance.props = next; },
  commitMount: () => {}, hideInstance: (instance: NeonChild) => { instance.hidden = true; }, hideTextInstance: (instance: NeonChild) => { instance.hidden = true; },
  unhideInstance: (instance: NeonChild) => { instance.hidden = false; }, unhideTextInstance: (instance: NeonChild, text: string) => { instance.hidden = false; if (instance.kind === "text") instance.value = text; },
};

const Renderer = Reconciler(hostConfig);

function appendHost(parent: NeonChild, child: NeonChild) {
  if (parent.kind !== "host") throw new Error("text nodes cannot contain children");
  append(parent, child);
}

function insertHost(parent: NeonChild, child: NeonChild, before: NeonChild) {
  if (parent.kind !== "host") throw new Error("text nodes cannot contain children");
  insert(parent, child, before);
}

function removeHost(parent: NeonChild, child: NeonChild) {
  if (parent.kind !== "host") throw new Error("text nodes cannot contain children");
  remove(parent, child);
}

export type NeonRoot = { render(node: React.ReactNode): void; renderAndWait(node: React.ReactNode): Promise<void>; unmount(): void };

export function createNeonRoot(options: NeonRootOptions): NeonRoot {
  const container: NeonContainer & { __submit?: SubmitFragment; __onError?: (error: unknown) => void } = { children: [], __submit: options.submit, __onError: options.onError };
  const root = Renderer.createContainer(container, ConcurrentRoot, null, false, null, "neon-ui-react-client", options.onError ?? console.error, options.onError ?? console.error, options.onError ?? console.error, null);
  return {
    render(node) { Renderer.updateContainer(node, root, null, null); },
    renderAndWait(node) { return new Promise((resolve, reject) => { try { Renderer.updateContainer(node, root, null, resolve); } catch (error) { reject(error); } }); },
    unmount() { Renderer.updateContainer(null, root, null, null); },
  };
}
