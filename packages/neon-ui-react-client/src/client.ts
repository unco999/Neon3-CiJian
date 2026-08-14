import type { JsonValue, Revision, RpcRequest, RpcResponse, RpcTransport, UiFragment, UiIntent, UiIntentSpec, UiSurfaceSnapshot } from "./protocol.js";

export class NeonUiClient {
  constructor(private readonly transport: RpcTransport, private readonly instanceId = `react-${crypto.randomUUID()}`) {}

  submitFragment(fragment: UiFragment, expectedRevision: Revision | null = null): Promise<RpcResponse> {
    return this.call("ui-runtime", "ui.fragment.submit", { kind: "submit_fragment", submission: { schema_version: 1, fragment } } as unknown as JsonValue, expectedRevision, `fragment:${fragment.fragment_id}:${fragment.revision}`);
  }

  dispatchIntent(intent: UiIntentSpec, expectedRevision: Revision | null = null): Promise<RpcResponse> {
    const wireIntent: UiIntent = { kind: "invoke", action: intent.action, params: intent.params };
    return this.call("ui-runtime", "ui.intent.dispatch", wireIntent, expectedRevision, `intent:${intent.action}:${JSON.stringify(intent.params)}`);
  }

  async surfaceSnapshot(): Promise<UiSurfaceSnapshot> {
    const response = await this.call("ui-runtime", "ui.surface.snapshot.get", {}, null, `surface-snapshot:${crypto.randomUUID()}`);
    if (response.status !== "accepted" || !response.result) throw new Error(response.error?.message ?? "UI surface snapshot was rejected");
    return response.result as unknown as UiSurfaceSnapshot;
  }

  async surfaceEvent(event: { type: "DIAGNOSTICS_TOGGLE" } | { type: "INSPECTOR_TAB_SELECT"; tab: UiSurfaceSnapshot["value"]["inspector"]["tab"] }, snapshot: UiSurfaceSnapshot): Promise<UiSurfaceSnapshot> {
    const response = await this.call("ui-runtime", "ui.surface.action", { event }, snapshot.revision, `surface-event:${event.type}:${snapshot.revision}:${"tab" in event ? event.tab : ""}`);
    if (response.status !== "accepted" || !response.result) throw new Error(response.error?.message ?? "UI surface action was rejected");
    return response.result as unknown as UiSurfaceSnapshot;
  }

  private call(target: string, method: string, params: JsonValue, expectedRevision: Revision | null, idempotencyKey: string): Promise<RpcResponse> {
    const request: RpcRequest = { protocol: "neon3.rpc", version: { major: 1, minor: 0 }, request_id: crypto.randomUUID(), client: { kind: "ui_react_client", instance_id: this.instanceId, pid: 0, origin: "neon-ui-react-client" }, target, method, params, expected_revision: expectedRevision, idempotency_key: idempotencyKey };
    return this.transport.call(request);
  }
}
