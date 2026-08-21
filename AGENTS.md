# Neon3 多进程基础架构约定

本目录是 Neon3 的全新重构工作区。本文件定义不可随意绕过的进程边界、权威所有权、
渲染边界与公开通信协议。开始实现前必须先阅读本文件。

AI 创建或修改 NUI Flow 前，还必须阅读 [AI NUI Flow Authoring](docs/nui-flow-ai-authoring.md)
与 `plan/neon3-nui-flow.md`。NUI Flow 只允许声明式 UI 和受限的本地 presentation
statechart，不能承载领域规则、项目写入、代码执行或 GPU 资源操作。

## 1. 已确认的核心模型

Neon3 使用多个独立、可单独启动和重启的无窗口业务进程，但所有 wgpu 渲染集中在唯一的
`neon-wgpu-runtime` 进程中。

```text
neon-ui-runtime          无窗口，无 wgpu；UI 声明、语义控件、输入意图
neon-terrain-runtime     无窗口，无 wgpu；地形工具和领域逻辑
neon-resource-runtime    无窗口，无 wgpu；资源浏览、选择、导入领域逻辑
neon-projectd            无窗口，无 wgpu；项目文件、资产、事务、revision
neon-wgpu-runtime        唯一窗口、唯一 wgpu owner、唯一最终合成器
neon-cli                 无窗口；公开协议 client，供人类、脚本和 AI 使用
neon-sessiond            可选；服务发现、监督和本地 capability 注册
```

`neon-wgpu-runtime` 是唯一允许创建下列对象的进程：

- `winit::Window` 与平台 surface。
- `wgpu::Instance`、Adapter、Device、Queue。
- GPU texture、buffer、sampler、bind group、pipeline。
- render graph、GPU resident tables、最终 UI 和世界画面的合成。

因此 Neon3 不允许业务进程自行创建或管理 GPU 资源，也不允许未经协商的跨进程纹理传输。
正式渲染仍由 `neon-wgpu-runtime` 在同一个 GPU 进程内生成和合成；但经过明确的 backend/
adapter matching 后，`neon-wgpu-runtime` 可以向受控 external host consumer 导出只读共享
surface（例如 Windows D3D12 shared texture + shared fence）。这不是第二个 Neon GPU owner：
共享资源的创建、状态转换、generation、resize、device-lost 和 release 仍归
`neon-wgpu-runtime`，宿主只能按协议等待并采样。

## 2. 最终进程图

```text
                     +---------------------+
                     | neon-ui-runtime     |
                     | 无窗口 UI 领域层    |
                     +----------+----------+
                                |
                     UI declaration / intent
                                |
 +-----------------------+      |      +-----------------------+
 | neon-terrain-runtime  |------+------| neon-resource-runtime |
 | 地形领域状态机        |             | 资源领域状态机        |
 +-----------+-----------+             +-----------+-----------+
             |                                         |
             | typed render / domain commands           | AssetRef / jobs
             v                                         v
       +-------------------------------------------------------+
       | neon-wgpu-runtime                                     |
       | 唯一 winit 窗口 + 唯一 wgpu Device/Queue + 最终合成   |
       +--------------------------+----------------------------+
                                  |
                           project protocol
                                  v
                         +----------------+
                         | neon-projectd  |
                         | 项目唯一写者   |
                         +----------------+

neon-cli / AI 通过同一公开协议连接各服务，绝不模拟 UI 点击。
```

`neon-wgpu-runtime` 可以直接向 `neon-projectd` 查询只读数据或接收已验证的资源加载命令，
但项目写入仍只由 `neon-projectd` 执行。

## 3. 权威所有权

| 状态或职责 | 唯一权威模块 |
| --- | --- |
| OS 窗口、最终窗口 surface、鼠标捕获、键盘焦点 | `neon-wgpu-runtime` |
| 所有 GPU 资源、pipeline、render graph、resident handle | `neon-wgpu-runtime` |
| 最终 UI 像素、世界画面、viewport 和合成顺序 | `neon-wgpu-runtime` |
| UI 布局声明、控件语义、控件可见/禁用条件、intent 映射 | `neon-ui-runtime` |
| 地形 mode、brush、channel、水体绑定需求、preview/commit 语义 | `neon-terrain-runtime` |
| 资源选择会话、导入流程、资源浏览领域状态 | `neon-resource-runtime` |
| 项目文件、AssetId、资产目录、事务、文件锁、project revision | `neon-projectd` |
| 自动化命令执行 | `neon-cli` 或 AI client，均不是权威服务 |

每个进程只能修改自己拥有的业务状态。客户端缓存的 snapshot 只能用于显示和恢复，不能被
当成业务真相。

## 4. 严格边界

禁止：

```text
UI element ID -> 直接成为 terrain/resource/project 业务命令
React/TS local state -> 推断 brush/water/channel/tool mode
terrain-runtime -> 创建 wgpu resource、pipeline 或窗口
resource-runtime -> 直接修改 terrain-runtime 内存
任何 client -> 直接写项目文件
任何业务进程 -> 未经 backend/adapter matching 导出 GPU texture
任何 external host -> 修改、销毁或重解释 `neon-wgpu-runtime` 的共享 surface
```

允许：

```text
UI intent -> terrain-runtime domain command
terrain-runtime -> wgpu-runtime typed terrain render/update command
resource-runtime -> 返回稳定 AssetRef
projectd -> 发布 revisioned asset/project snapshot
wgpu-runtime -> 返回 GPU readiness 和 render diagnostics
```

`640060` 一类 element ID 只允许作为 `neon-wgpu-runtime` 内部 hit-test 细节；它绝不能
进入跨进程协议、项目文件、资源引用或领域状态。

## 5. 领域与渲染分离

`neon-terrain-runtime` 不返回 PNG、纹理、GPU handle 或“渲染图”。它只返回领域状态和
经过 schema 验证的 typed command。

例如：

```text
terrain.tool.select { tool=water_inject }
  -> terrain snapshot { mode=water_paint, binding=needs_selection }
  -> terrain effect { resource.pick.open, accepted_kind=water_material }

terrain.resource.bind { session_id, AssetRef }
  -> terrain snapshot { binding=loading }
  -> terrain render command { preload_water_material, AssetRef }

wgpu-runtime
  -> gpu status { binding=ready, resident_handle=[slot,generation] }

terrain-runtime
  -> terrain snapshot { mode=water_paint, binding=ready }
```

`neon-wgpu-runtime` 只执行已定义、已验证的 render/update command。它不判断
`WaterPaint` 是否需要水体材质，不决定工具 mode，也不从 UI 局部状态推断业务规则。

## 6. UI Runtime 的定位

`neon-ui-runtime` 是无窗口 UI 声明和交互语义模块，不是最终 renderer。

它负责：

- 根据 tool/project snapshot 产生 `UiFragment` 与布局声明。
- 将用户 intent 映射为语义 command。
- 接收 UI event，并向对应业务 runtime 发起请求。
- 维护可丢弃的显示缓存；重启后从 snapshot 恢复。

它不负责：

- winit window、wgpu device、GPU text/image/UI draw。
- terrain/resource/project 权威状态。
- 通过本地状态判断“当前是否应使用 water channel”。

`neon-wgpu-runtime` 接收 `UiFragment`，负责将其放入统一 window shell，并用 Neon2 的
wgpu UI/text/render graph 绘制最终像素。

受控 external host interop 只允许通过公开 `neon3.rpc`/`neon3.event` contract 建立。宿主
必须先报告实际 backend、adapter identity、进程 ID 和所需 transport；匹配失败时不得自动
降级到 PNG、CPU frame 或未经确认的 backend。原生 texture/fence handle 不进入 JSON 或
event journal，只能由本机 handle broker 按短期 session token 交付。

## 7. 控制面协议

第一阶段使用 length-prefixed JSON over loopback TCP。协议语义必须独立于 transport；后续
可增加 Windows named pipe、Unix domain socket 或远程安全 transport adapter。

不得把 Tauri IPC、React callback、共享内存地址或 UI element ID 定义为服务协议。

所有请求使用统一 envelope：

```json
{
  "protocol": "neon3.rpc",
  "version": 1,
  "request_id": "uuid",
  "client": {
    "kind": "ui_runtime",
    "instance_id": "uuid",
    "pid": 1234
  },
  "target": "terrain-runtime",
  "method": "terrain.tool.select",
  "params": { "tool": "water_inject" },
  "expected_revision": 42,
  "idempotency_key": "uuid"
}
```

成功响应：

```json
{
  "request_id": "uuid",
  "status": "accepted",
  "revision": 43,
  "result": {},
  "snapshot": {}
}
```

拒绝或冲突响应：

```json
{
  "request_id": "uuid",
  "status": "rejected",
  "error": {
    "code": "revision_conflict",
    "message": "资源已被其他请求更新",
    "current_revision": 43
  }
}
```

禁止 silent no-op。每个失败必须有稳定错误码、可读消息、`request_id` 和相关 revision。

## 8. 通用消息规则

所有 mutation 必须带：

- `request_id`：关联请求、效果和异步完成。
- `expected_revision`：乐观并发控制。
- `idempotency_key`：断线重试不得重复执行写操作。
- client identity / `origin`：审计与回送过滤。

所有异步工作必须带：

- `job_id` 或 `session_id`。
- 输入对象的 stable ID 与 revision。
- `queued`、`loading`、`ready`、`failed`、`cancelled` 等明确状态。

所有 snapshot/event 必须带服务 `epoch` 与单调递增的 `sequence`。客户端重连后先获取完整
snapshot，再恢复订阅流。

## 9. 资源选择标准流程

模块间只交换稳定 `AssetRef`：

```json
{
  "project_id": "uuid",
  "asset_id": 81,
  "revision": 5,
  "kind": "water_material"
}
```

水体工具流程：

```text
UI Runtime -> Terrain Runtime
  terrain.tool.select { water_inject }

Terrain Runtime -> UI Runtime
  terrain.snapshot { mode=water_paint, binding=needs_selection }
  ui.effect { resource.pick.open, session_id, accepted_kinds=[water_material] }

UI Runtime -> Resource Runtime
  resource.pick.open { request_id, accepted_kinds }

Resource Runtime -> UI Runtime
  resource.pick.result { request_id, AssetRef }

UI Runtime -> Terrain Runtime
  terrain.resource.bind { session_id, AssetRef }

Terrain Runtime -> WGPU Runtime
  terrain.gpu.preload_water_material { terrain_context_id, AssetRef }

WGPU Runtime -> Terrain Runtime
  terrain.gpu.resource_status { request_id, state=ready, resident_handle }

Terrain Runtime -> UI Runtime
  terrain.snapshot { mode=water_paint, binding=ready }
```

Resource Runtime 不直接写 Terrain Runtime；Terrain Runtime 不创建 GPU resource；只有
WGPU Runtime 产生 resident handle。

## 10. 共享内存与 GPU 传输

**正式渲染路径不使用共享内存传递画面。**

普通共享内存传递帧意味着：

```text
GPU readback -> CPU memory -> shared memory -> CPU upload -> GPU texture
```

这会造成同步等待、带宽浪费和高延迟，只能用于：

- 控制面 ring buffer 的未来优化。
- 小型 snapshot、日志、trace、profiling 数据。
- 崩溃诊断截图或测试 fixture。

因为所有 wgpu 资源集中在 `neon-wgpu-runtime`，正常路径无需共享纹理、D3D12 interop、
Vulkan external memory 或 Metal IOSurface。这是跨 Windows/Linux/macOS 的设计优势。

## 11. 基础服务方法

每个服务至少支持：

```text
service.health
service.describe
service.subscribe
service.shutdown
```

`service.describe` 返回 protocol version、endpoint、capability 与服务 epoch。调用方不得
假设某个可选能力存在。

### neon-projectd

```text
project.open
project.summary
project.subscribe
asset.list
asset.get
asset.create
asset.import.begin
asset.import.status
asset.rename
asset.delete
transaction.begin
transaction.commit
transaction.abort
```

### neon-resource-runtime

```text
resource.pick.open
resource.pick.cancel
resource.pick.result
resource.import.begin
resource.import.status
```

### neon-terrain-runtime

```text
terrain.session.summary
terrain.subscribe
terrain.tool.snapshot.get
terrain.tool.select
terrain.resource.bind
terrain.resource.cancel
terrain.preview.begin
terrain.preview.update
terrain.preview.commit
terrain.preview.cancel
```

### neon-wgpu-runtime

```text
wgpu.window.summary
wgpu.ui.submit_fragment
wgpu.ui.remove_fragment
wgpu.terrain.attach_context
wgpu.terrain.apply_command
wgpu.terrain.resource_status
wgpu.render.diagnostics
```

## 12. CLI 与 AI

CLI 和 AI 必须是公开协议 client，不能直接写项目文件、调用业务进程内部函数、模拟 UI 点击或
依赖屏幕像素：

```text
neon service describe --target terrain-runtime
neon project assets list
neon terrain tool select --terrain 12 --tool water_inject
neon terrain resource bind --session 92 --asset 81 --revision 5
```

AI 只能读取 revisioned snapshot、发送 typed command、等待 accepted/rejected/job status。

## 13. 并发、恢复与安全

- `neon-projectd` 是项目文件唯一写者，使用事务与 project revision。
- 每个领域 runtime 对自己的 state 使用独立 revision。
- `expected_revision` 不匹配必须返回 `revision_conflict`，不得覆盖。
- 非幂等写操作必须使用 `idempotency_key`。
- 任何 UI/领域 runtime 崩溃后，可通过服务 snapshot 恢复。
- 不可合并长操作使用带 TTL 的 lease，禁止无限期全局锁。
- 默认仅绑定 loopback；跨机器通信必须显式启用认证与加密。

## 14. 推荐 crate 骨架

```text
D:\Neon3\
  Cargo.toml
  crates\
    neon-protocol\          公开 schema、错误码、revision、AssetRef
    neon-ipc\               framed transport、RPC、订阅、重连
    neon-sessiond\          可选服务发现、监督、capability 注册
    neon-projectd\          项目与资产唯一写者
    neon-ui-schema\         UiFragment、UiEffect、Intent 定义
    neon-ui-runtime\        无窗口 UI declaration / interaction runtime
    neon-terrain-runtime\   无窗口地形领域 runtime
    neon-resource-runtime\  无窗口资源领域 runtime
    neon-wgpu-runtime\      唯一 winit + wgpu + 最终合成
    neon-cli\               公开协议 client
```

## 15. 实施顺序

1. 建立 `neon-protocol`、`neon-ipc` 与最小 `neon-sessiond`。
2. 建立 `neon-wgpu-runtime`，证明它是唯一 window/GPU owner，并能接收空的 `UiFragment`。
3. 建立 `neon-ui-runtime`，通过协议向 WGPU Runtime 提交一个静态 UI fragment。
4. 建立 `neon-projectd`，先提供 `project.summary`、`asset.list` 与订阅。
5. 建立 `neon-terrain-runtime`，先迁移 tool snapshot、内建工具选择和资源 bind 语义。
6. 建立 `neon-resource-runtime`，迁移 `resource.pick.open/result/cancel`。
7. 让 `neon-cli` 完成与 UI 相同的 terrain/resource 流程。
8. 只有上述 slice 稳定后，才迁移旧编辑器中的具体地形和资源功能。

## 16. 当前阶段约束

当前先建立新骨架和协议，未经明确批准不得迁移、删除或重写旧项目代码。

任何第一个垂直切片必须证明：

1. `neon-wgpu-runtime` 是唯一窗口与 GPU owner。
2. UI、Terrain、Resource 不共享进程内业务状态，也不创建 wgpu resource。
3. UI Runtime 重启后能从 Terrain/Resource/Project snapshot 恢复。
4. CLI 能走同一条 tool/resource command 链路。
5. 错误可以通过 request/session/revision 精确追踪。

## 17. AI、调试与验收优先

Neon3 必须将 AI、CLI、自动化测试和人类调试视为一等协议 client。任何功能若只能通过
手工点击、观察临时 UI 状态或读取某个进程的内存才能操作或验证，则该功能尚未完成。

目标工作流：

```text
人类 / AI / CLI
  -> 查询 capability 与 snapshot
  -> 提交 typed command
  -> 获得 accepted / rejected / job status
  -> 读取结构化 trace 与新的 revisioned snapshot
  -> 执行对应层级验收
```

禁止让 AI：

- 模拟鼠标坐标、按钮编号或 UI element ID。
- 直接修改项目文件、数据库、缓存或另一服务的进程内状态。
- 将截图识别结果当作唯一业务验收依据。
- 根据日志文本猜测 command 是否成功。

允许 AI：

- 通过 `neon-cli` 或同等 RPC client 查询服务、能力、snapshot、trace 和诊断。
- 发送带 revision 与 idempotency key 的 typed command。
- 创建隔离测试项目、执行声明式验收任务、读取机器可读结果。
- 请求最终窗口的 frame capture，作为视觉验收的补充证据。

## 18. 可观察性协议

所有服务必须提供结构化、可订阅的诊断流。日志可供人阅读，但不是唯一调试接口。

最小方法集合：

```text
debug.snapshot.get
debug.trace.subscribe
debug.trace.query
debug.command.get
debug.diagnostics.get
debug.health.check
```

每个 trace record 至少包含：

```json
{
  "sequence": 300,
  "epoch": 4,
  "timestamp_unix_ms": 0,
  "service": "terrain-runtime",
  "level": "info",
  "event": "terrain.resource.bind.completed",
  "request_id": "uuid",
  "session_id": "uuid",
  "project_id": "uuid",
  "context_id": "terrain:12",
  "revision_before": 42,
  "revision_after": 43,
  "data": {}
}
```

规则：

- `event` 使用稳定的点分名称，不用仅供阅读的自由文本代替。
- 同一 command 从接收、校验、执行、异步 job、完成或失败必须通过同一个 `request_id` 串联。
- 错误必须同时包含 stable `code`、可读 `message` 和关联对象 ID。
- trace 必须可按 request、session、job、context、asset、revision 和时间范围查询。
- 默认 trace 必须脱敏，严禁记录密码、token 或项目加密密钥。

建议公共 crate：

```text
neon-observability
  TraceRecord
  DiagnosticRecord
  CommandReceipt
  DebugSnapshot
  trace filter / retention contract
```

## 19. Command Journal 与回放

每个权威服务必须保留有限长度、可查询的 command journal。它用于定位状态问题和复现 bug，
不是项目持久化格式，也不能取代 `neon-projectd` 的事务日志。

```text
CommandReceived
CommandValidated
CommandAccepted | CommandRejected
JobStarted | JobProgress | JobFinished | JobFailed
SnapshotPublished
```

每条 journal record 必须保存：

- 原始 typed command 的规范化、脱敏表示。
- request/client/idempotency identity。
- 处理前后 revision。
- 产生的 effect、job 或 GPU command 的引用。
- 最终响应与失败 code。

服务应支持：

```text
debug.command.get { request_id }
debug.journal.query { from_sequence, filters }
debug.replay.export { request_ids }
```

回放只允许进入隔离的测试 session 或临时项目副本。绝不允许把生产项目上的 command journal
直接重放到当前用户项目。

## 20. 声明式验收任务

不要把验收写成“打开窗口后手点几下”。每个高层工作流都应有可执行的声明式 scenario。

示例：

```yaml
id: terrain.water.select-and-bind.v1
project_fixture: fixtures/terrain-water.neon
steps:
  - target: terrain-runtime
    method: terrain.tool.select
    params: { terrain_id: 12, tool: water_inject }
    expect:
      snapshot:
        mode: water_paint
        binding_state: needs_selection
  - target: resource-runtime
    method: resource.pick.resolve
    params: { request_from: previous, asset_id: 81, revision: 5 }
  - target: terrain-runtime
    method: terrain.resource.bind
    params: { session_from: step_1, asset_from: step_2 }
    expect_job: terrain.gpu.preload_water_material
  - await:
      target: terrain-runtime
      snapshot:
        binding_state: ready
```

scenario runner 必须输出机器可读 JSON：

```json
{
  "scenario": "terrain.water.select-and-bind.v1",
  "status": "passed",
  "steps": [],
  "trace_request_ids": [],
  "artifacts": []
}
```

建议公共 crate：

```text
neon-testkit
  fixture project lifecycle
  service launch and health wait
  scenario runner
  snapshot matcher
  trace matcher
  artifact manifest
```

## 21. 分层验收标准

每项功能必须明确自己通过了哪一层，不能把低层成功描述为最终可见功能成功。

| 层级 | 证明内容 | 典型执行者 |
| --- | --- | --- |
| `contract-ready` | protocol schema、serde、版本/错误码兼容 | 单元测试、CLI |
| `service-ready` | 领域 runtime 接收 command、发布正确 snapshot/effect | headless scenario |
| `gpu-ready` | `neon-wgpu-runtime` 成功应用 GPU command，资源/管线状态 Ready | headless GPU check |
| `composition-ready` | WGPU Runtime 在最终 composition graph 中挂载对应 UI/world surface | render graph assertion |
| `wgpu-rendered` | 最终 window target 已渲染可验证像素 | frame capture + pixel/assertion |
| `interactive-accepted` | 人类确认交互、布局、性能与实际工作流 | 用户验收 |

`wgpu-rendered` 不等于 `interactive-accepted`。自动化 visual check 不能替代人类体验验收，
但它必须能防止黑屏、未挂载 surface、错误 viewport、明显遮挡和资源未显示。

## 22. WGPU Runtime 的测试接口

因为 `neon-wgpu-runtime` 是唯一 renderer，它必须提供不依赖人工窗口点击的验收接口：

```text
wgpu.render.graph.snapshot
wgpu.render.target.capture
wgpu.render.target.assert
wgpu.render.diagnostics
wgpu.resource.inspect
wgpu.resource.wait_ready
```

`wgpu.render.target.capture` 只能捕获 WGPU Runtime 的最终合成 target 或明确命名的测试 target。
它不是让业务服务导出纹理的替代方案。

测试 capture 需要明确：

- target ID、尺寸、format、color space。
- 关联的 render graph revision。
- capture frame sequence。
- 是否使用 test adapter/headless surface。

每个 GPU command 都必须回报相关的 job/status，不允许 UI 通过“等待固定毫秒数”猜测 Ready。

## 23. 开发环境与服务生命周期

本地开发需要一条明确的 session 启动链，而不是手工打开若干随机进程：

```text
neon dev up --project <path> --profile editor
  -> 启动或连接 neon-sessiond
  -> 启动 neon-projectd
  -> 启动 neon-wgpu-runtime
  -> 启动 neon-ui-runtime
  -> 按需启动 neon-terrain-runtime / neon-resource-runtime
  -> 输出 service endpoint、epoch、PID、capability 和日志位置
```

每个服务启动后必须先完成：

```text
service.health
service.describe
service.register (如启用 sessiond)
```

每个服务应支持独立重启；调用方在收到 epoch 变化后必须丢弃旧 session/job token，重新请求
snapshot，不能继续提交旧 revision 的 command。

建议至少提供：

```text
neon dev status
neon dev logs --service terrain-runtime --request <id>
neon dev restart --service ui-runtime
neon debug trace --request <id>
neon test scenario <id>
```

## 24. 公共 workspace 增补

在本文件第 14 节 crate 骨架基础上，增加：

```text
neon-observability      结构化 trace、诊断、command receipt
neon-testkit            fixture、scenario、snapshot/trace matcher
neon-dev                本地 session 启动、状态、日志和重启命令
tests/
  protocol_contract/
  service_integration/
  gpu_acceptance/
  scenarios/
  fixtures/
```

依赖方向保持单向：所有服务可以依赖 `neon-protocol`、`neon-ipc`、
`neon-observability` 与 `neon-testkit`；业务 runtime 之间不得通过 Rust crate 直接相互依赖。

## 25. AI 实施纪律

AI 开始一项修改前必须：

1. 查询目标服务的 `service.describe` 与当前 snapshot。
2. 找到或新增对应的 contract test / scenario。
3. 声明改动影响的是 protocol、domain、GPU、composition 或 UI declaration 哪一层。

AI 完成一项修改后必须：

1. 执行该层允许的最小检查并报告实际结果。
2. 输出相关 request/job/session/revision 与 trace artifact。
3. 明确报告 acceptance level。
4. 不在未经用户授权时启动交互窗口或宣称人类视觉验收已经完成。

AI 在跨服务故障中必须先按 `request_id -> command journal -> snapshot -> job -> render diagnostics`
链路定位，不得先通过扩大超时、重试点击、添加局部 React state 或绕过协议来掩盖问题。

## 26. 高速交互与低延迟数据面

鼠标移动、持续按下、拖拽、滚轮、相机操控、brush stroke、gizmo 操作和其他高频交互不能把
每一个原始 input event 都变成跨进程 request/response。`neon-wgpu-runtime` 是 OS window、输入
焦点、hit-test、pointer capture 和最终像素的唯一 owner，因此它必须在本进程内立即处理原始输入
和对应的视觉反馈。

### 26.1 三层交互模型

```text
Layer 1: local real-time interaction
  owner: neon-wgpu-runtime
  data: raw pointer, keyboard, wheel, hit-test, capture, hover, preview
  rule: frame-local; never wait for IPC before visual feedback

Layer 2: reliable semantic control
  transport: normal neon-ipc RPC over loopback TCP
  data: interaction.begin, interaction.end, cancel, mode change, commit
  rule: revisioned, request_id, idempotency_key; never silently drop

Layer 3: optional high-frequency sample stream
  transport: negotiated shared-memory SPSC ring buffer
  data: fixed-size pointer/stroke/gizmo samples or latest-value parameters
  rule: no GPU resource, no business-state ownership, no Rust pointer, no JSON
```

普通 UI 面板拖拽优先由 Layer 1 完成。WGPU Runtime 可立即显示面板位置、hover、按下和拖拽预览，
仅在松开、取消或需要持久化布局时向业务服务发送一次 Layer 2 语义 command。不得为了更新每帧
像素把面板拖拽变成每帧跨进程 RPC。

Terrain painting、连续 gizmo transform、实时曲线编辑等需要领域服务持续参与的交互，可以在性能
测量证明 batch TCP 无法满足目标延迟后，启用 Layer 3。共享内存是受限的高频采样优化，不是第二个
权威状态系统，也不是正式画面/GPU 资源传输路径。

### 26.2 交互生命周期

所有连续交互必须有可靠的控制面生命周期：

```text
WGPU Runtime receives pointer down
  -> local hit-test and pointer capture
  -> immediate local preview
  -> RPC interaction.begin { interaction_id, kind, context, expected_revision }

While captured
  -> WGPU Runtime updates local preview every frame
  -> optional shared-memory samples or coalesced RPC batches reach domain runtime
  -> domain runtime publishes revisioned preview/effect status when required

Pointer up / explicit cancel / focus loss / service epoch change
  -> RPC interaction.end or interaction.cancel
  -> domain validates samples and commits or rejects
  -> WGPU Runtime clears prediction only after explicit final state or rejection
```

`pointer_id`、`interaction_id`、service `epoch` 和 per-interaction monotonic `sequence` 必须贯穿
begin、sample、end、trace 和 command journal。WGPU Runtime 在收到目标服务 epoch 变化时，必须取消
本地 capture/prediction，重新请求 snapshot；绝不能将旧 interaction 的 samples 提交给新 epoch。

### 26.3 共享内存 fast path 契约

共享内存仅在 `service.describe` / capability negotiation 明确支持后使用。默认 transport 仍为
length-prefixed JSON over loopback TCP；共享内存不可用、版本不匹配或 ring overflow 时必须有确定的
降级策略。

第一版只能使用单生产者、单消费者（SPSC）ring buffer。每个 ring 有唯一 writer 和唯一 reader：

```text
wgpu-runtime writer -> terrain-runtime reader
wgpu-runtime writer -> ui-runtime reader
```

不要让多个 runtime 写同一个 ring，不要把 ring 用作双向 RPC，不要将项目状态、资产表、可变 Rust
对象或 GPU handle 映射进去。反向反馈继续使用普通 RPC/event stream，或建立另一个独立 SPSC ring。

每个 ring header 至少包含：

```text
magic
schema_version
record_size
capacity
producer_epoch
consumer_epoch
write_sequence
read_sequence
overflow_count
closed_flag
```

每个 record 必须为固定大小、显式字节序、显式对齐的二进制 schema。最小 pointer sample 可采用：

```text
interaction_id: u128 or two u64 fields
sequence: u64
timestamp_monotonic_ns: u64
position_logical_x: f32
position_logical_y: f32
delta_x: f32
delta_y: f32
pressure: f32
buttons: u32
modifiers: u32
flags: u32
reserved: fixed zero padding
```

实际 ABI 必须用 `#[repr(C)]` Rust record 和显式 field offset/size/stride test 固化。不得假设 Rust
默认 layout、指针宽度或 host endianness。共享内存名称、访问权限、schema version、capacity 和双方
epoch 必须由可靠 RPC 协商，不能由硬编码全局名称猜测。

### 26.4 丢弃、合并和溢出规则

- hover、pointer move、滚轮和本地 preview 是 latest-value 类数据：可合并、可丢弃旧 sample，但必须
  保留最新 sequence。
- terrain stroke、gizmo transform 和连续编辑是轨迹类数据：不能无提示丢失 sample。消费者发现 sequence
  gap 或 `overflow_count` 增加时，必须发出稳定的 `interaction_sample_overflow` 诊断，并执行该交互类型
  定义的恢复策略。
- 恢复策略必须由领域契约定义，例如请求 keyframe、以最后完整 sequence 截断并取消、或采用 domain
  resampling；不能默默继续并假装完整轨迹没有缺口。
- `interaction.begin`、`interaction.end`、`interaction.cancel`、commit、revision conflict 和错误响应
  永远使用可靠 RPC，不进入可丢弃 ring。
- Ring 中的 sample 只是输入事实或预测提示；领域 runtime 仍是业务 preview/commit 的权威方，
  `neon-projectd` 仍是持久化写入的唯一 owner。

### 26.5 性能、诊断与验收

不要因为“可能更快”提前增加共享内存。先用可测量的 coalesced TCP batch 方案，并记录：

```text
input event timestamp
WGPU local preview frame timestamp
sample enqueue timestamp
domain consume timestamp
domain preview/commit response timestamp
final composition frame sequence
```

只有在目标硬件和目标交互负载下，TCP batch 的 p95/p99 延迟或吞吐不能满足已声明预算时，才可以实施
共享内存 fast path。每个 fast path 必须提供：

- capability 名称、schema version 和 fallback 行为。
- ring capacity、record size、overflow policy 和 max sample rate。
- 结构化 trace：begin、sample gap、overflow、consumer lag、end、cancel、epoch mismatch。
- headless producer/consumer tests、wraparound tests、epoch mismatch tests、overflow tests 和 ABI layout tests。
- 有共享内存/无共享内存两种模式下的同一 scenario 结果比较。

共享内存 fast path 的验收只证明数据面行为和延迟预算；它不能替代 `neon-wgpu-runtime` 最终 composition
的 `wgpu-rendered` 验收，也不能替代用户的 `interactive-accepted` 验收。

## 27. AI 工作日记(强制)

AI 在 Neon3 工作区进行任何实质性工作时,必须维护工作日志,便于人工复盘、恢复上下文和审计。

### 27.1 日志位置与命名

- 目录:`docs/ai-diary/`(不存在则创建)。
- 文件命名:`YYYY-MM-DD-主题-slug.md`。同一天多个主题可写多个文件,也可合并为一个当日文件。
- 格式:Markdown,包含 YAML 风格头部字段,便于检索。

### 27.2 必填内容

每个日志文件必须包含:

```text
日期(ISO 格式)
主题(一句话描述这一天做了什么)
涉及的 crate / 文件路径(列表)
发现的问题(症状、根因、证据)
采取的方案(改动说明)
当前状态(已完成 / 进行中 / 已回退)
未完成事项与下一步
测试与验证结果(实际命令与输出摘要,而非猜测)
```

### 27.3 写入时机(硬性规则)

- 当日工作结束时必须写入(无论如何都要有记录)。
- 修复、重构、跨进程改动完成或遇到卡点时,应随时追加或新建立日志。
- 如果某天没有代码改动,只做了调研或计划确认,也要写一条简短的记录(标记 `type: research`)。

### 27.4 展示约束

- 日志要如实记录失败和卡点,不得只写成功。
- 测试结果必须来自实际运行,写明命令与输出;禁止把"应该会通过"写成"已通过"。
- 涉及跨进程、GPU、IPC 的行为改动,要记录验证该行为所用的探针/测试入口及实际输出。

### 27.5 与 AGENTS.md 的关系

本节的"每日必须写入"是流程纪律,不是功能验收。任何功能验收仍必须遵循第 20-21 节的声明式
scenario 与分层验收标准,日志不能替代真实测试。
