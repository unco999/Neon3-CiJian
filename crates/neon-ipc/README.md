# neon-ipc

Length-prefixed JSON RPC and event transport over loopback TCP for Neon3.

Provides `RpcClient` / `RpcServer` and `EventClient` used by Neon3 services and
external host clients (such as a game engine) to talk to the renderer and UI
runtime over the `neon3.rpc` / `neon3.event` contracts.

## License

MIT OR Apache-2.0
