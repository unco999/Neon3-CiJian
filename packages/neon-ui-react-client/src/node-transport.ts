import net from "node:net";
import type { RpcRequest, RpcResponse, RpcTransport } from "./protocol.js";

export function createLoopbackTransport(endpoint: { host?: string; port: number }, timeoutMs = 5000): RpcTransport {
  return {
    call(request: RpcRequest) {
      return new Promise<RpcResponse>((resolve, reject) => {
        const socket = net.createConnection({ host: endpoint.host ?? "127.0.0.1", port: endpoint.port });
        let buffer = Buffer.alloc(0);
        const timer = setTimeout(() => { socket.destroy(); reject(new Error("neon RPC timeout")); }, timeoutMs);
        socket.once("error", (error) => { clearTimeout(timer); reject(error); });
        socket.on("data", (chunk) => {
          buffer = Buffer.concat([buffer, chunk]);
          if (buffer.length < 4) return;
          const length = buffer.readUInt32BE(0);
          if (buffer.length < 4 + length) return;
          clearTimeout(timer); socket.end();
          try { resolve(JSON.parse(buffer.subarray(4, 4 + length).toString("utf8")) as RpcResponse); } catch (error) { reject(error); }
        });
        const payload = Buffer.from(JSON.stringify(request), "utf8");
        const frame = Buffer.allocUnsafe(4 + payload.length); frame.writeUInt32BE(payload.length, 0); payload.copy(frame, 4); socket.write(frame);
      });
    },
  };
}
