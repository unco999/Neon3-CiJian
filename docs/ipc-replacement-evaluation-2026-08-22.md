# Neon3 IPC 替换调研报告

日期: 2026-08-22
结论: **不要整体替换 Neon3 的协议和 IPC 语义；应替换 `neon-ipc` 的同步 transport 实现。**

## 一、结论先行

当前性能瓶颈主要不是 length-prefixed JSON，而是以下实现方式:

1. `RpcServer::serve_until` 一次只处理一个连接和一个 request，handler 同步执行。
2. `RpcClient::call` 连接模型不支持 request multiplex，很多路径每次请求重新建立 TCP 连接。
3. 长任务、host forward、GPU 等待和 RPC handler 混在同一个控制循环。
4. 订阅、事件、RPC 没有统一的异步 backpressure 和取消模型。
5. 高频视觉反馈仍有跨进程 fragment 提交；这不应成为 pointer/hover 的实时路径。

因此，建议保留:

```text
neon-protocol 的 RpcRequest/RpcResponse、request_id、revision、epoch、sequence、错误码
neon3.rpc / neon3.event 的语义
服务所有权和 WGPU 唯一 owner 约束
CLI/AI 可读取的 JSON 控制面
```

建议替换:

```text
neon-ipc 的同步 TcpStream + serve_until
每请求一个连接的 client 使用方式
同步 handler 直接执行长任务的服务模型
```

## 二、现状问题

### P0: 服务端串行 handler

证据: `crates/neon-ipc/src/lib.rs:221-241`

当前 `serve_until` 的流程是:

```text
accept
read one request
execute handler synchronously
write response
accept next connection
```

任何一个 handler 的 host RPC、GPU 等待、文件操作、AI 推理或慢客户端，都可能阻塞同一服务的健康检查、调试、输入和其他业务请求。

这不是换 JSON 就能解决的问题，必须改为异步 accept/read/dispatch 模型。

### P0: 高频路径不应该走 RPC

WGPU 是窗口、hit-test、pointer capture 和最终像素的 owner。hover、pointer move、拖拽预览和动画采样必须在 WGPU 进程本地完成。

控制面只发送:

```text
interaction.begin
interaction.end
interaction.cancel
semantic commit
```

最终 fragment 是可靠状态更新，不是每帧动画数据。

### P1: 每请求连接和缺少 multiplex

证据: `crates/neon-ipc/src/lib.rs:101-133`

当前 client 的 `call` 是单 stream request/response。服务端也按“一条连接一个请求”处理。结果是:

- 连接建立、TCP 参数设置和 JSON 编解码重复发生。
- 一个持久连接不能安全承载多个 request_id 请求。
- 订阅、RPC response 和异步 delivery 没有统一复用模型。

### P1: deferred queue 需要自驱动 dispatcher

当前 UI host forward 已改成有序 worker，但服务循环仍然是 request 驱动的。没有后续请求时，队列完成结果不能及时被 UI Runtime 应用。

后续应增加独立 dispatcher 或 event-loop wakeup，不能依赖“下一次 request 到达”推动异步工作。

## 三、候选方案比较

| 方案 | 适合程度 | 优点 | 主要代价 | 结论 |
| --- | --- | --- | --- | --- |
| `tokio` + `tokio-util` codec | **最高** | 保留现有 JSON envelope；异步 accept、multiplex、timeout、cancel、stream 都可控；迁移最小 | 需要自己实现 pending map、server dispatch 和 event backpressure | **推荐第一阶段** |
| `tonic` + protobuf | 高 | 成熟 HTTP/2 multiplex、streaming、deadline、生态完整 | 需要迁移 schema/codegen；AI/CLI JSON 需要 gateway；不直接解决领域 job 模型 | 第二阶段或公开远程 API |
| `tarpc` | 中 | Rust typed RPC、transport 可插拔、比 tonic 更接近 Rust 内部服务 | 生态和调试工具弱于 tonic；事件流、公开 JSON、版本兼容仍需自建 | 仅适合纯 Rust 内部控制面 |
| `interprocess` | 中 | Windows named pipe/Unix local socket transport 方向明确 | 只是 transport，不提供 RPC、request_id、job、订阅和 backpressure 语义 | 作为第二 transport adapter |
| `ipc-channel` | 中低 | 进程间 channel 抽象、serde 支持 | 不适合现有公开 RPC/CLI/AI 语义；远程或 TCP fallback 不自然 | 不建议作为总线 |
| Cap'n Proto RPC | 中低 | 高性能 typed serialization/RPC | schema、工具链和调试成本高；JSON public contract 仍需另一套 | 只有性能测量证明 JSON/HTTP2 不够时考虑 |
| QUIC/QUIC RPC | 低 | multiplex、stream、拥塞控制 | 同机 IPC 过重；认证、连接和调试复杂 | 不用于第一阶段本机 IPC |

## 四、推荐目标架构

### 4.1 控制面

保持现有 envelope，transport 改为异步长连接:

```text
RpcConnection
  writer task -> bounded outbound queue -> socket
  reader task -> response/event demux
  pending: HashMap<RequestId, oneshot::Sender<RpcResponse>>
  timeout/cancel per request
```

服务端:

```text
accept loop
  -> per-connection read loop
  -> request validation
  -> bounded service queue
  -> short handler or job spawn
  -> response/event writer
```

同一个连接可以并发发送多个 request，但 response 必须用 `request_id` 配对。事件 delivery 不能抢占 response channel，应使用独立的 bounded event queue。

### 4.2 长任务

长任务禁止在 RPC handler 内等待:

```text
request -> accepted { job_id }
job worker -> queued/loading/progress/ready/failed/cancelled
event stream -> job status
debug.job.get -> latest status
```

适用对象:

- host publication
- AI generation
- resource import/preload
- GPU readback/capture
- project transaction
- world surface synchronization

### 4.3 高频数据面

不要把 hover/pointer move 改成高吞吐 RPC。优先级顺序应为:

1. WGPU 进程本地处理 raw input、hit-test、prediction、animation track、buffer offset。
2. 低频 semantic begin/end/cancel 走可靠 RPC。
3. 只有性能测量证明 TCP batch 不够时，才启用 SPSC shared-memory sample ring。

共享内存 ring 仍然不承载 GPU 资源、项目状态或权威领域状态。

## 五、第一阶段施工方案

### Step 1: 保持公共协议不变

不要先改 `neon-protocol`。为所有现有 method 保持 JSON compatibility，新增 transport-independent traits:

```rust
trait RpcTransport {
    async fn call(&self, request: RpcRequest) -> Result<RpcResponse, TransportError>;
    async fn subscribe(&self, request: SubscriptionRequest) -> Result<EventStream, TransportError>;
}
```

旧的同步 client 暂时保留为 compatibility adapter，避免一次性改完所有 service。

### Step 2: `neon-ipc` 引入 async multiplex client

建议依赖:

```toml
tokio = { version = "1", features = ["net", "io-util", "sync", "time", "rt-multi-thread"] }
tokio-util = { version = "0.7", features = ["codec"] }
```

第一版仍使用 JSON:

```text
LengthDelimitedCodec -> serde_json -> RpcRequest/RpcResponse
```

先获得:

- persistent connection
- request_id demux
- per-request timeout
- cancellation
- bounded outbound queue
- event stream

### Step 3: 服务端 handler 与任务解耦

将 `serve_until` 替换为:

```text
serve_async
serve_connection
dispatch_request
spawn_job
```

服务 handler 不得直接等待另一个服务的慢 RPC。`service.health`、`debug.*` 和 shutdown 必须拥有独立调度优先级。

### Step 4: 分离 transport adapter

抽象:

```text
TcpLoopbackTransport   默认，跨平台，支持 CLI/调试/远程显式配置
WindowsNamedPipe       本机 editor profile 的可选 transport
UnixDomainSocket       Linux/macOS 的可选 transport
```

协议语义不依赖 transport。named pipe 不应直接进入 public JSON 或 service method。

### Step 5: 迁移服务顺序

1. `neon-eventd`
2. `neon-ui-runtime`
3. `neon-wgpu-runtime`
4. `neon-projectd`
5. `neon-cli` compatibility client

每迁移一个 service，必须同时保留 old/new transport scenario，对比 request_id、revision、event sequence 和错误码。

## 六、是否直接使用 tonic

不建议现在把 Neon3 全部改成 tonic。

适合 tonic 的情况:

- 需要跨机器部署。
- 需要强 schema codegen 和多语言 client。
- 公开 API 已稳定。
- 可以接受 protobuf 作为 canonical wire schema。

现在不适合的原因:

- Neon3 仍在建立 protocol 和 service ownership。
- CLI/AI 需要可读 JSON contract。
- WGPU 高频数据面不应该变成 gRPC streaming。
- tonic 不能自动修复 GPU lock、同步 host、错误的 render boundary 或无界缓存。

后续可以让 tonic 成为远程/public adapter，但不应取代本机高频数据面。

## 七、必须消除的“锁死帧”设置

这些问题即使换 IPC 库也仍然存在:

1. WGPU runtime 内对 GPU/runtime mutex 的无限 `yield_now` 重试。
2. pointer handler 在 GPU 锁不可用时无界等待。
3. camera mutex 临界区内做 UDP 序列化和发送。
4. hover 每次移动触发 GPU hit pass/readback。
5. readback ring 固定 3 slot，slot 忙时只能丢反馈，缺少明确 latest-value 诊断。
6. `RpcServer::serve_until` handler 内执行同步长任务。
7. event publisher 每个变量变化单独建立连接并等待 ack。
8. 1 MiB JSON frame 上限没有 chunk/job fallback。
9. image/text/idempotency/cache 缺少统一 LRU、TTL 和 byte budget。
10. 固定 16ms render sleep/debounce 和固定 world quad 上限，超限可能 panic 或产生错误节奏。

## 八、验收指标

### IPC 控制面

- `service.health` 在一个 1 秒慢 RPC 运行时仍能在 p99 < 20ms 返回。
- 同一长连接支持至少 128 个 in-flight request，request_id 配对正确。
- request timeout、cancel、断线重连不会遗留 pending entry。
- event queue overflow 有稳定错误码和计数。

### 高频交互

- 120Hz pointer/hover 注入时，WGPU 本地视觉反馈不等待 IPC。
- 60Hz/120Hz/240Hz 输入下，latest-value 只保留最新 sequence。
- 轨迹类 sample 出现 gap 时，发出 `interaction_sample_overflow`，不静默继续。
- host 延迟 250ms 时，第二个面板仍在 16ms 内开始本地动画。

### GPU

- 每帧报告 `queue_write_count`、`upload_bytes`、`hit_readback_busy`、`active_animation_count`。
- 1/10/50 个 surface 的 frame p95、upload bytes、draw calls 可比较。
- 512/2048/10000 节点压力测试不出现 O(N²) 爆炸。
- 超过 capacity 返回 `capacity_exceeded`/job failed，不使用 assert 终止服务。

## 九、最终建议

给施工方的明确任务不是“换一个 IPC 库”，而是:

```text
保留 neon-protocol
重写 neon-ipc 为 tokio async multiplex transport
将 RPC handler 和长任务解耦
将 WGPU 高频交互留在 WGPU 进程本地
增加 TCP/named-pipe transport adapter
用 tonic 作为未来远程 API 选项，而不是现在的全量替换
```

这条路线能解决当前的阻塞和连接开销，同时不破坏 Neon3 已经建立的 ownership、revision、AI/CLI 可观察性和跨平台协议边界。

## 十、施工进度（2026-08-22）

第一阶段施工已落地，全部为增量改动，未改动 `neon-protocol` 与所有权模型。

### 已完成的 IPC 层改造（`crates/neon-ipc`）

- 新增 `src/async_rpc.rs`：
  - `AsyncRpcClient`：持久连接 + `request_id` 多路复用、per-request timeout/cancel、有界 outbound 队列、`in_flight()`/`orphan_responses()` 观测计数、`Clone` 共享同一连接。
  - `AsyncRpcServer`：accept/read 循环不阻塞在单个 handler 上，全局 semaphore 限流；`serve`（同步 handler 走 spawn_blocking）/`serve_async`（异步 handler）；`serve_until` 支持 stop 谓词（等价旧 `serve_until` 的 shutdown）；handler panic 映射为 `Failed` 响应而非终止服务。
  - `RpcTransport` trait：transport 无关契约（`impl Future + Send`），为 named pipe / unix socket / 远程 tonic 留适配口。
- 同步桥接（把 tokio 完全封装在 `neon-ipc` 内部，调用方无需加 tokio 依赖）：
  - `AsyncRpcServer::bind_blocking` / `bind_blocking_with` / `serve_blocking` / `serve_until_blocking`。
  - `BlockingRpcClient`：同步 `call`（签名与旧 `RpcClient::call` 一致），内部持 runtime + 持久连接。
- `TransportError` 增加 `Cancelled` 变体；旧同步 `RpcClient/RpcServer/EventClient` 全部保留（wire 兼容）。

### 已切换的服务端（消除单线阻塞，P0）

| 服务 | 位置 | 状态 |
| --- | --- | --- |
| `neon-wgpu-runtime` `--headless-server` | `src/main.rs` | 已切 `serve_until_blocking` + `Arc<Mutex<WgpuRuntime>>` |
| `neon-wgpu-runtime` headless external | `src/lib.rs` `spawn_headless_external_server` | 已切（handler 原已 `Arc<Mutex>`） |
| `neon-wgpu-runtime` window server | `src/lib.rs` `spawn_window_server` | 已切 `serve_until_blocking` + `Arc<Mutex<WgpuRuntime>>`（shadow 技巧保持 handler 体不变） |
| `neon-projectd` | `src/lib.rs` `serve` | 已切 `serve_until_blocking` + `Arc<Mutex<Projectd>>` |

### 未改（需领域层设计，非 transport 替换）

- `neon-ui-runtime::serve_forwarder`：其 handler 串行是领域语义（host forward 顺序保证 + `active_host_forwards` 状态机 + deferred queue），并发化需要「自驱动 dispatcher + job 模型」，超出 transport 替换范围。
- 客户端每请求新建连接（`RpcClient::connect().call()` 的高频 forward 路径）：需引入连接池（`HashMap<SocketAddr, BlockingRpcClient>`）与生命周期管理，作为下一步。
- `neon-eventd`：已是每连接线程并发 + 事件长连接流，无需改。

### 待办

1. 全 workspace 编译回归 + 跑 `cargo test -p neon-ipc`（新用例 + 旧同步用例）。
2. 客户端连接池（ui-runtime forward 高频路径）。
3. named pipe / unix socket transport adapter。
4. `neon-ui-runtime` deferred queue 的自驱动 dispatcher。
