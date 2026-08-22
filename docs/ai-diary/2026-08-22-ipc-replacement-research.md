日期: 2026-08-22
主题: 调研 Neon3 是否应整体替换 IPC
涉及的 crate / 文件路径:
  - crates/neon-ipc/src/lib.rs
  - crates/neon-ipc/Cargo.toml
  - Cargo.toml
  - docs/ipc-replacement-evaluation-2026-08-22.md

发现的问题:
  - 当前 `RpcServer::serve_until` 是单连接单请求、同步 handler 模型，慢任务会阻塞服务循环。
  - `RpcClient` 没有 request multiplex、取消、统一事件流和 bounded backpressure。
  - 这些问题主要属于 transport/runtime 实现，不是 `neon3.rpc` envelope 或 JSON 语义本身。
  - 高频 pointer/hover 不应通过任何 RPC 库实现实时视觉反馈，必须由 WGPU 本地处理。

采取的方案:
  - 研究 tokio + tokio-util codec、tonic、tarpc、interprocess、ipc-channel、Cap'n Proto 和 QUIC 的适配边界。
  - 形成报告，建议保留 `neon-protocol`，先将 `neon-ipc` 重写为 async persistent multiplex transport，再按需增加 Windows named pipe adapter。
  - 明确 tonic 作为未来远程/public API 选项，不作为当前所有本机 IPC 的一次性替换。

当前状态: 已完成

未完成事项与下一步:
  - 尚未修改 IPC 代码；下一步应先实现 async compatibility adapter 和 transport contract tests。
  - 施工前需要声明 p95/p99 latency、in-flight request 数、event overflow 和 reconnect 验收预算。

测试与验证结果:
  - 本次为静态调研，没有运行测试或启动服务。
  - 已核对 `Cargo.toml`、`neon-ipc/Cargo.toml` 和 `neon-ipc/src/lib.rs` 的实际实现。
