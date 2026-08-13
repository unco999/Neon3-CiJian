# Neon3 首个工作计划：协议骨架与第一个可验收垂直切片

## 0. 计划用途

这不是产品需求说明，也不是一次性重写任务。这是一份供 `D:\goal` 自动驱动器和 low 模型逐步执行的施工合同。

施工目录：`D:\Neon3`

施工入口：`D:\goal\Start-GoalDriver.cmd`

本计划的唯一施工目标是：在不迁移 Neon2 旧功能的前提下，建立 Neon3 的最小多进程工作区，并证明第一条公开协议链路可以被 headless client 调用、被 UI runtime 接收、被 WGPU runtime 作为唯一 renderer 接收一个静态 UI fragment。

本计划完成后，不代表 Neon3 已经完成编辑器、地形、资源浏览或完整 UI。它只代表新的边界、协议、进程骨架和第一条可诊断垂直链路成立。

## 1. 开始前必须遵守的规则

### 1.1 必读文件

开始任何编辑前，按下面顺序读取：

1. `D:\Neon3\AGENTS.md`
2. `D:\Neon3\plan\neon3-first-work-plan.md`
3. `D:\Neon3` 当前目录列表
4. 如果已存在：`Cargo.toml`、`crates\`、`tests\` 和本计划的 `Progress` 区域
5. 如果已经有施工记录：`D:\Neon3\plan\construction-log.jsonl`、`D:\Neon3\plan\progress.json`

不要因为旧工作区存在就读取或修改 `D:\Neon2`。本计划只允许在 `D:\Neon3` 创建新骨架。

### 1.2 不允许做的事

- 不修改、删除或迁移 `D:\Neon2` 中的代码。
- 不把 Neon2 的 crate 复制到 Neon3 作为“临时实现”。
- 不建立 React、Tauri、WebView 或 DOM renderer。
- 不让 `neon-ui-runtime`、`neon-terrain-runtime`、`neon-resource-runtime`、`neon-projectd` 创建窗口或依赖 `wgpu`。
- 不让任何非 `neon-wgpu-runtime` 进程创建 `winit::Window`、`wgpu::Device`、`wgpu::Queue`、texture、buffer、pipeline 或 render graph。
- 不使用共享纹理、D3D12 handle、CPU frame readback 或共享内存传递正式画面。
- 不通过跨服务 Rust crate 直接访问另一个服务的内部状态。
- 不把 UI element ID 写入跨进程协议。
- 不添加静默成功；所有拒绝必须有稳定错误码、消息和 `request_id`。
- 不在没有测试或没有更新本计划 Progress 的情况下声称里程碑完成。
- 不启动 Neon3 窗口进行交互验收，除非用户在当前会话明确授权。
- 不把“代码编译通过”描述成 `wgpu-rendered` 或 `interactive-accepted`。

### 1.3 每次工作的固定循环

每次 low 模型恢复、继续或开始新 cycle，都必须执行以下固定循环：

1. 读取 `AGENTS.md` 和本计划的 `Progress`。
2. 执行 `git status --short`；如果 Neon3 还不是 Git 仓库，只记录这一事实，不要伪造状态。
3. 找到下面第一个 `未完成` 且依赖已满足的里程碑。
4. 只处理这个里程碑，不提前实现后续服务。
5. 先写或更新对应测试，再写实现；如果测试结构尚不存在，先建立最小测试入口。
6. 运行该里程碑列出的最小检查。检查失败时留在当前里程碑，诊断和修复原因。
7. 更新本文件 `Progress`，写明文件、命令、结果、剩余风险和下一步。
8. 如果仓库已启用 Git 且该里程碑是独立小功能，检查 diff 后创建一个专用 commit。
9. 到达预算边界前必须写入可恢复的 checkpoint，不能只在聊天文本中描述进度。

## 2. 目标架构

最终进程方向必须保持单向：

```text
neon-cli / AI client
        |
        | length-prefixed JSON over loopback TCP
        v
neon-sessiond       可选监督与服务发现
        |
        +--> neon-projectd          项目与资产唯一写者
        +--> neon-ui-runtime        无窗口 UI declaration / intent
        +--> neon-terrain-runtime   无窗口地形领域逻辑
        +--> neon-resource-runtime  无窗口资源领域逻辑
        +--> neon-wgpu-runtime      唯一窗口、唯一 wgpu owner、最终合成
```

第一期不要求所有进程都已经有完整领域功能，但代码目录和依赖方向必须让错误架构难以出现。

第一期必须证明的所有权：

| 能力 | 唯一 owner |
| --- | --- |
| OS window / surface | `neon-wgpu-runtime` |
| wgpu instance / adapter / device / queue | `neon-wgpu-runtime` |
| final composition | `neon-wgpu-runtime` |
| UI declaration and intent mapping | `neon-ui-runtime` |
| project file mutation | `neon-projectd` |
| automated typed commands | `neon-cli` |
| trace and command journal | `neon-observability` plus each service instance |

## 3. 交付范围

### 3.1 必须交付

- 一个根 `Cargo.toml` workspace。
- `neon-protocol`：versioned RPC envelope、response、error、client identity、revision、AssetRef、service metadata。
- `neon-ipc`：length-prefixed JSON framing 和 loopback TCP 的最小 transport API。
- `neon-observability`：TraceRecord、CommandReceipt、DebugSnapshot、有限 command journal。
- `neon-ui-schema`：UiFragment、UiNode、UiEffect 和 typed intent 的最小 schema。
- `neon-wgpu-runtime`：唯一允许依赖 `winit` / `wgpu` 的服务骨架；第一期可以使用空 render graph 或 test-only headless backend，但必须有静态 ownership test。
- `neon-ui-runtime`：无窗口、无 wgpu；能够生成一个静态 `UiFragment` 并通过协议发送给 WGPU runtime。
- `neon-cli`：能够调用 `service.health`、`service.describe` 和提交一个静态 UI fragment 命令，输出机器可读 JSON。
- 至少一个 headless integration scenario，验证 `cli -> ui-runtime 或 wgpu-runtime -> response -> trace`。
- 每个服务的 `service.health`、`service.describe`、`service.shutdown` 最小方法契约。
- 本计划 Progress 的完整施工记录。

### 3.2 第一期明确不交付

- 不实现完整的 `neon-terrain-runtime` 业务逻辑。
- 不实现资源导入、资产数据库、项目事务或真实编辑器。
- 不实现完整窗口视觉样式、字体系统、复杂 layout 或 terrain rendering。
- 不实现跨平台 named pipe、远程 TCP、认证和加密。
- 不实现共享 GPU memory。
- 不实现 AI 自主修改项目文件的特殊旁路。

## 4. 里程碑总表

状态值只能使用：`未完成`、`进行中`、`已完成`、`阻塞`。

| ID | 名称 | 依赖 | 验收层级 | 状态 |
| --- | --- | --- | --- | --- |
| M0 | 工作区与施工记录 | 无 | contract-ready | 已完成 |
| M1 | Cargo workspace 与依赖边界 | M0 | contract-ready | 已完成 |
| M2 | 公共协议 schema | M1 | contract-ready | 已完成 |
| M3 | IPC framing 与 request lifecycle | M2 | contract-ready | 已完成 |
| M4 | 可观察性与 command journal | M2 | contract-ready | 已完成 |
| M5 | UI declaration schema | M2 | contract-ready | 已完成 |
| M6 | WGPU runtime ownership skeleton | M3、M5 | gpu-ready / composition-ready | 未完成 |
| M7 | UI runtime 静态 fragment sender | M3、M5 | service-ready | 未完成 |
| M8 | CLI 与 headless vertical slice | M4、M6、M7 | service-ready / composition-ready | 未完成 |
| M9 | 第一阶段审计与用户窗口验收准备 | M8 | acceptance handoff | 未完成 |
| M10 | 高速交互数据面评估与可选 fast path | M9 | latency-ready | 未完成 |

## 5. 里程碑详细施工说明

## M0：工作区与施工记录

状态：已完成

本里程碑由本计划创建动作完成。它的作用是让后续低模型有稳定的工作合同。

必须存在：

- `D:\Neon3\AGENTS.md`
- `D:\Neon3\plan\neon3-first-work-plan.md`
- `D:\goal\goal-driver.json` 指向本计划
- `D:\goal\goal-prompt.md` 要求按本计划施工

不要在 M0 中创建 Cargo 代码。

## M1：Cargo workspace 与依赖边界

目标：创建空的 Neon3 workspace 和每个第一期 crate 的最小目录，先让边界可以被编译器和测试检查。

允许修改或创建：

- `.gitignore`
- `Cargo.toml`
- `crates/neon-protocol/Cargo.toml` 与 `src/lib.rs`
- `crates/neon-ipc/Cargo.toml` 与 `src/lib.rs`
- `crates/neon-observability/Cargo.toml` 与 `src/lib.rs`
- `crates/neon-ui-schema/Cargo.toml` 与 `src/lib.rs`
- `crates/neon-wgpu-runtime/Cargo.toml` 与 `src/main.rs`
- `crates/neon-ui-runtime/Cargo.toml` 与 `src/main.rs`
- `crates/neon-cli/Cargo.toml` 与 `src/main.rs`
- `tests/` 下仅允许建立测试入口和 fixture 目录

依赖要求：

- `neon-protocol` 只依赖序列化和错误处理所需的公共库，不依赖业务 runtime。
- `neon-ipc` 可以依赖 `neon-protocol`，不得依赖 `wgpu`、`winit` 或 UI crate。
- `neon-observability` 可以依赖 `neon-protocol`，不得依赖 renderer。
- `neon-ui-schema` 可以依赖 `neon-protocol`，不得依赖 `wgpu`、`winit`。
- `neon-ui-runtime` 依赖 protocol、ipc、observability、ui-schema，不依赖 wgpu/winit。
- `neon-wgpu-runtime` 是唯一可以依赖 wgpu/winit 的 crate。
- `neon-cli` 是 client，不得依赖任何 runtime 的内部模块。

施工步骤：

1. 运行 `git rev-parse --is-inside-work-tree`。如果失败，在 `D:\Neon3` 运行 `git init`，创建最小 `.gitignore`，并记录这是 Neon3 的初始仓库建立；不要触碰 `D:\Neon2` 的 Git 仓库。
2. 创建 workspace members，并为每个 crate 建立能编译的空入口。
3. 只加入第一期实际需要的依赖，不要预先加入完整编辑器依赖。
4. 给 WGPU crate 加一个注释或模块文档，明确它是唯一 GPU owner。
5. 给 UI、IPC、protocol crate 加编译期文档，明确它们不得创建 GPU 对象。
6. 添加一个简单的 workspace structure test 或脚本检查，确认非 WGPU crate 的 manifest 不包含 `wgpu` 和 `winit`。

最小检查：

```powershell
cargo check --workspace
cargo test --workspace
```

完成条件：

- workspace 能成功解析和编译。
- 依赖方向满足上表。
- 没有创建任何窗口。
- 如果 M1 创建了 Git 仓库，检查 staged diff 后创建一个只包含 M1 workspace 文件的初始 commit。
- 在 Progress 记录实际命令和输出结果。

失败处理：

- 若 cargo 不可用，记录完整错误并标记阻塞，不要假设完成。
- 若依赖版本冲突，减少依赖或使用 workspace 统一版本，不能绕过检查。
- 若 low 模型想一次创建所有服务，停止并只完成 M1。

## M2：公共协议 schema

目标：建立 transport 独立的公开协议类型。协议必须能被 serde 序列化、反序列化和测试。

建议类型：

- `ProtocolVersion { major, minor }`
- `RequestId(String)`
- `ClientKind`：`cli`、`ui_runtime`、`terrain_runtime`、`resource_runtime`、`projectd`、`wgpu_runtime`
- `ClientIdentity { kind, instance_id, pid, origin }`
- `ServiceName`
- `Revision(u64)`
- `AssetRef { project_id, asset_id, revision, kind }`
- `RpcRequest { protocol, version, request_id, client, target, method, params, expected_revision, idempotency_key }`
- `RpcResponse { request_id, status, revision, result, snapshot, error }`
- `RpcStatus`：`accepted`、`rejected`、`failed`
- `RpcError { code, message, current_revision, object_id }`
- `ServiceHealth`
- `ServiceDescription { service, protocol_version, endpoint, epoch, capabilities }`
- `ServiceEvent { epoch, sequence, payload }`

协议规则：

- 字段名固定，使用 snake_case 或明确的 serde rename，整个 workspace 统一。
- 请求必须保留 `request_id`。
- mutation 的 `expected_revision` 和 `idempotency_key` 必须可表达；如果某个方法是只读，可以显式为 null。
- 不允许把 `UiElementId`、窗口坐标或 Rust 内部指针放入公共 schema。
- 错误 code 使用稳定机器可读值，例如 `invalid_request`、`revision_conflict`、`not_found`、`unsupported_method`、`service_unavailable`。
- 对未知字段的兼容策略必须写成测试，不能靠未说明的默认行为。

必须写的测试：

1. 一个完整 request JSON fixture 可以 round-trip。
2. 一个 accepted response fixture 可以 round-trip。
3. 一个 revision conflict response 保留 request_id 和 current_revision。
4. 缺少必需字段时反序列化失败或转换成稳定错误。
5. `AssetRef` 不包含本地路径作为跨服务身份。

最小检查：

```powershell
cargo test -p neon-protocol
cargo check --workspace
```

完成条件：protocol crate 可以被 IPC、UI schema 和 CLI 使用；不要在本里程碑创建真实 socket。

## M3：IPC framing 与 request lifecycle

目标：实现 transport 独立 API 背后的第一种 transport：loopback TCP 上的 length-prefixed JSON。

必须实现：

- 长度前缀编码和解码。
- 最大 frame size，拒绝超大 frame，不能无限分配内存。
- 完整 frame、半 frame、多 frame 粘连的测试。
- request/response correlation：response 的 `request_id` 必须匹配请求。
- 基本连接错误映射为稳定 transport error，不要 panic。
- 连接关闭和超时的明确结果。
- 一个只负责 loopback endpoint 的 server/client helper。

不能实现：

- 共享内存传输。
- GPU 资源传输。
- 隐式 retry 写命令。
- 把 transport error 当成业务 accepted。

推荐 API 形状：

```text
encode_frame(bytes) -> bytes
decode_frames(buffer) -> frames + remaining
RpcClient::connect(endpoint)
RpcClient::call(request) -> response
RpcServer::bind(endpoint)
RpcServer::serve(handler)
```

测试：

1. 单元测试 framing 边界。
2. client/server loopback 测试一个 health request。
3. 错误 request_id 测试必须拒绝响应。
4. 超过 max frame 测试必须返回 `frame_too_large`。
5. 并发两个 request 时 response 不得串线。

最小检查：

```powershell
cargo test -p neon-ipc
cargo test --workspace
```

## M4：可观察性与 command journal

目标：让 low 模型和人类可以通过结构化记录知道请求现在处于什么阶段，而不是猜日志。

必须实现：

- `TraceRecord`：`sequence`、`epoch`、`timestamp_unix_ms`、`service`、`level`、`event`、`request_id`、可选 session/job/context、revision 前后、data。
- `CommandReceipt`：received、validated、accepted/rejected、completed/failed。
- `DebugSnapshot`：service、epoch、revision、health、capabilities、active_jobs。
- 有限长度 journal，超限按最旧记录淘汰。
- 按 request_id、session_id、job_id、revision 查询的纯内存 API。
- 脱敏规则：不能记录 password、token、secret、密钥或完整本地凭据。

推荐稳定事件：

```text
command.received
command.validated
command.accepted
command.rejected
command.completed
command.failed
snapshot.published
service.started
service.stopped
```

测试：

1. 同一 request_id 的完整生命周期可以查询。
2. sequence 单调递增。
3. 新 epoch 不会继续使用旧 sequence。
4. journal 超出容量后保留最新记录。
5. 脱敏测试确认敏感字段不会出现在序列化输出。

最小检查：

```powershell
cargo test -p neon-observability
cargo test --workspace
```

## M5：UI declaration schema

目标：定义 UI runtime 发送给 WGPU runtime 的声明，而不是把 DOM 或 React element 作为协议。

最小类型：

- `UiFragmentId`
- `UiFragment { fragment_id, revision, root, effects }`
- `UiNode { node_id, kind, bounds, visible, enabled, text_key, children }`
- `UiNodeKind`：先只需要 `panel`、`label`、`button`。
- `UiBounds`：声明逻辑布局数据，不等于 wgpu buffer。
- `UiEffect`：第一期可以为空或只包含 semantic action。
- `UiIntent`：例如 `UiIntent::Invoke { action }`，不得包含跨服务 element ID 业务命令。
- `UiCommand::SubmitFragment` 和 `UiCommand::RemoveFragment`。

规则：

- text 使用稳定 `text_key`；需要显示文字时可以有 fixture translation，但不要把 i18n 系统扩展成第一期目标。
- 节点 ID 只能在 fragment 内部稳定，不得进入 terrain/project 协议。
- schema 只表达声明，不创建 GPU 对象。
- fragment revision 独立于 project revision，但字段含义必须明确。

测试：

1. 一个静态 fragment JSON fixture round-trip。
2. 空 children、多个 children、disabled button 的 schema 测试。
3. 非法 bounds、空 fragment id 或未知 node kind 的拒绝测试。
4. 序列化结果不含 wgpu handle、窗口对象或本地路径。

最小检查：

```powershell
cargo test -p neon-ui-schema
cargo test --workspace
```

## M6：WGPU runtime ownership skeleton

目标：建立唯一 GPU owner 的服务入口和 command handler。第一期重点是所有权证明，不是画完整 UI。

必须实现：

- `neon-wgpu-runtime` 二进制入口。
- `service.health` 和 `service.describe`。
- `wgpu.ui.submit_fragment`、`wgpu.ui.remove_fragment` 的协议 handler。
- 内存中的 fragment registry，按 fragment id 和 revision 更新。
- `wgpu.render.diagnostics` 返回 graph revision、fragment 数量、window/headless 状态。
- 每个 command 产生 observability receipt。
- WGPU/winit 初始化必须只出现在这个 crate 的代码中。

窗口策略：

- 可以先提供 headless/test mode，优先完成无窗口集成测试。
- 如果实现真实 window，必须有显式 `--interactive` 或等价 flag；默认测试不能打开窗口。
- 不得为了测试把 GPU 初始化复制到 UI runtime 或 CLI。

静态 ownership 检查：

- 使用脚本或测试扫描第一期 crate manifests，只有 `neon-wgpu-runtime` 能依赖 `wgpu` / `winit`。
- 检查 UI runtime 和 CLI 源码不出现 `wgpu::`、`winit::`、`Window`、`Device`、`Queue` 等 owner token；检查要避免误报注释，必要时用编译结构代替文本扫描。
- 如果修改 WGSL，必须加入 Naga parse/validate test；本里程碑没有 WGSL 时不要添加假 shader。

测试：

1. headless health/describe。
2. submit fragment 后 diagnostics 的 registry 数量和 revision 正确。
3. stale revision 被拒绝，错误为 `revision_conflict`。
4. remove fragment 是幂等的，并返回明确结果。
5. 非 WGPU 服务不链接 wgpu/winit。

最小检查：

```powershell
cargo test -p neon-wgpu-runtime
cargo check -p neon-wgpu-runtime
cargo test --workspace
```

## M7：UI runtime 静态 fragment sender

目标：让无窗口 UI runtime 生成并提交一个静态 fragment；它不渲染像素。

必须实现：

- `neon-ui-runtime` 二进制入口。
- `service.health`、`service.describe`、`service.shutdown`。
- 从固定 typed declaration 创建一个 `UiFragment`。
- 通过 `neon-ipc` 调用 WGPU runtime 的 `wgpu.ui.submit_fragment`。
- 保存可丢弃的 fragment cache；不能把它当作最终业务真相。
- 收到 WGPU accepted/rejected 后写 trace。

必须明确：

- UI runtime 不拥有 window。
- UI runtime 不拥有 wgpu resource。
- UI runtime 不根据 element ID 直接修改 terrain/project 状态。
- UI runtime 重启后应能在未来通过 snapshot 重新生成 fragment；第一期可以使用固定 fixture，但必须在 Progress 写明这是 fixture 限制。

测试：

1. UI runtime 生成的 JSON 能被 ui-schema 解析。
2. UI runtime 的依赖和源码不包含 wgpu/winit。
3. 与 test WGPU server 的 submit 集成测试。
4. WGPU 返回 revision conflict 时 UI runtime 暴露 rejected，不静默成功。

最小检查：

```powershell
cargo test -p neon-ui-runtime
cargo check -p neon-ui-runtime
cargo test --workspace
```

## M8：CLI 与 headless vertical slice

目标：不打开窗口，通过一个命令完整证明公开协议、UI fragment、WGPU registry 和 trace 能串起来。

推荐 scenario：`ui.static-fragment.submit.v1`。

执行顺序：

1. 启动或在测试内创建 WGPU runtime test server。
2. 调用 `service.health`，断言状态是 healthy。
3. 调用 `service.describe`，读取 protocol version、epoch、capabilities。
4. 生成唯一 `request_id`、`idempotency_key` 和 fragment revision。
5. 发送 `wgpu.ui.submit_fragment`。
6. 等待明确 response，不使用固定 sleep 猜 Ready。
7. 查询 `wgpu.render.diagnostics`。
8. 查询 `debug.command.get { request_id }` 或 journal filter。
9. 输出机器可读 JSON：scenario、status、steps、request_ids、trace records、diagnostics。

CLI 输出要求：

- 默认输出 compact JSON，适合脚本解析。
- 错误退出码非零。
- 每一步包含 method、target、status、request_id、revision 或 error.code。
- 不把成功判断建立在日志文本或窗口截图上。

测试：

1. 完整 scenario pass。
2. WGPU server 拒绝时 scenario fail 且显示稳定 error code。
3. 重复 idempotency key 不产生重复 mutation。
4. 断开后重新连接先获取 snapshot/describe，再发送新 revision command。
5. 结果 JSON 可以写入 fixture 并被再次解析。

最小检查：

```powershell
cargo test --workspace
cargo run -p neon-cli -- --help
cargo run -p neon-cli -- scenario ui.static-fragment.submit.v1 --headless
```

如果最后一条命令依赖外部运行时且当前还没有 test server，则先实现 test server，再运行 scenario；不能把“命令不存在”记录成通过。

## M9：第一阶段审计与用户验收准备

目标：整理交付证据，不擅自打开窗口。

必须完成：

- 检查所有第一期 crate 的依赖边界。
- 检查所有 service method、error code、revision 和 epoch 语义。
- 检查每个 mutation 都有 request_id 和 idempotency_key。
- 检查 journal 能按 request_id 查询。
- 检查 UI/CLI 没有 GPU owner。
- 检查 WGPU runtime 是唯一窗口和 GPU owner。
- 运行 workspace tests 和 M8 scenario。
- 更新 Progress 为明确的 acceptance level：至少 `contract-ready`、`service-ready`、`gpu-ready`；只有真正完成最终 target capture 才能写 `wgpu-rendered`。

用户拥有的后续验收清单：

1. 用户明确授权启动 Neon3 WGPU 窗口。
2. 运行约定的 `cargo run -p neon-wgpu-runtime -- --interactive` 或实际命令。
3. 使用 CLI/API 提交静态 fragment。
4. 用户确认窗口中显示的最终像素来自 WGPU runtime，而不是 DOM。
5. 记录窗口尺寸、平台、GPU adapter、render graph revision 和结果。

在用户未完成上述步骤前，不能把 M9 写成 `interactive-accepted`。

## M10：高速交互数据面评估与可选 fast path

目标：在第一条 UI vertical slice 已稳定后，按 `AGENTS.md` 第 26 节评估连续交互的延迟，并且只有在
coalesced TCP batch 已证明不满足预算时才增加共享内存 SPSC ring buffer。

前置条件：M9 已完成，且用户或产品契约已经给出目标交互和延迟预算。例如：拖拽 local preview 的帧延迟、
terrain stroke domain consume latency、gizmo commit latency。没有预算时，M10 保持未完成，不得以猜测
引入共享内存。

固定顺序：

1. 定义一个 interaction fixture，例如面板拖拽、brush stroke 或 gizmo transform；写出 sample rate、
持续时间、目标帧率和 p95/p99 延迟预算。
2. 实现或测量 Layer 1 WGPU local preview 和 Layer 2 coalesced TCP batch 的 trace timestamps。
3. 用 headless scenario 记录 baseline 吞吐、p50、p95、p99、最大 queue depth 和丢失/合并数。
4. 若 baseline 满足预算，在 Progress 标记“shared-memory not justified”，M10 以 TCP batch 方案完成，
   不创建共享内存代码。
5. 若 baseline 不满足预算，先记录可复现的测量证据，再提交一个独立的 shared-memory design record，
   包含 ABI、SPSC ownership、overflow、epoch、capability negotiation 和 fallback。
6. 仅在设计记录批准后实现 SPSC ring，并遵守 `AGENTS.md` 第 26 节的 header/record/trace/test 要求。
7. 用相同 scenario 比较 TCP 与 shared-memory 数据面；两者都必须保留可靠 begin/end/cancel/commit RPC。

禁止：

- 将每个 raw input event 发成 request/response RPC。
- 将 UI panel drag 的每帧布局 mutation 发到项目服务。
- 使用共享内存传递纹理、GPU handle、Rust pointer、JSON、项目状态或业务权威对象。
- 因为 ring overflow 而静默丢弃 brush/stroke 轨迹。
- 在未测量 baseline 时把 shared memory 作为默认路径。

M10 最小验收：

```powershell
cargo test --workspace
cargo test -p neon-ipc -- interaction
cargo run -p neon-cli -- scenario interaction.<fixture>.latency.v1 --headless
```

如果 fast path 被实现，追加 ABI layout、wraparound、overflow、epoch mismatch、fallback 和两个模式结果
一致性的 focused tests。M10 的 Progress 必须记录硬件/OS、样本数量、采样率、p50/p95/p99、overflow count、
实际目标预算、选择 TCP 或 shared memory 的理由，以及任何仍需用户完成的 interactive acceptance。

## 6. 低模型执行格式

每次执行只允许输出并执行以下格式的工作记录：

```text
[GOAL]
plan: neon3-first-work-plan.md
milestone: Mx
state: in_progress

[BEFORE]
read: ...
status: ...
dependencies: ...

[CHANGE]
files: ...
purpose: ...

[CHECK]
command: ...
result: passed|failed|not_run
important_output: ...

[PROGRESS]
milestone: Mx
state: ...
acceptance_level: ...
completed: ...
remaining: ...
next_exact_step: ...
blocker: none|...
```

如果输出中没有 `next_exact_step`，就不能开始下一次修改。

## 7. Progress

本区域是跨 session 和 context compaction 的唯一施工进度账本。每完成一个有意义的部分，必须在同一个 change 中更新它。不要删除旧记录，只追加新记录。

### 当前状态

- 当前里程碑：`M5`
- 当前状态：`已完成`
- acceptance level：`contract-ready`（M5 static declaration fixture、validation 与 serialization tests 均通过）
- 下一步：下一 cycle 重新读取固定上下文，选择 M6 并先建立 headless ownership/service tests
- 用户拥有的验收：尚未开始；没有授权启动 Neon3 窗口

### 记录规则

每条记录必须包含：日期、milestone、状态、改动文件、实际检查命令、检查结果、commit（若有）、剩余工作和下一步。示例：

```text
2026-08-13 | M1 | 进行中
files: Cargo.toml, crates/...
checks: cargo check --workspace = passed; cargo test --workspace = not_run
commit: `5a39b54` (`Add public protocol schema`); this Progress bookkeeping update is committed separately
remaining: protocol types not implemented
next: create M2 request/response fixtures
user_acceptance: not requested; no window launched
```

### 施工日志

2026-08-13 | M5 | 已完成
files: `Cargo.lock`, `crates/neon-ui-schema/Cargo.toml`, `crates/neon-ui-schema/src/lib.rs`, `tests/fixtures/ui/static-fragment.json`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ui-schema` = passed（6 tests）; `cargo test --workspace` = passed; `cargo check --workspace` = passed; `git diff --check` = passed; 未启动窗口
commit: pending（仅提交 M5 files 和本条 Progress 记录；不包含用户/并行修改的 `.gitignore` 或未跟踪 `AGENTS.md`）
remaining: M6 WGPU runtime ownership skeleton
next: 重新读取 `AGENTS.md`、Progress、目录和 Git 状态后，为 `neon-wgpu-runtime` 先新增 headless service/ownership tests
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M5 | 进行中
files: `crates/neon-ui-schema/Cargo.toml`, `crates/neon-ui-schema/src/lib.rs`, `tests/fixtures/ui/static-fragment.json`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ui-schema` = not-run; `cargo test --workspace` = not-run; M5 无 service，故 `service.describe` / snapshot = not available; 未启动窗口
commit: none
remaining: 运行 M5 schema validation/fixture tests 与 workspace tests
next: 运行 `cargo test -p neon-ui-schema`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M4 | 已完成
files: `Cargo.lock`, `crates/neon-observability/Cargo.toml`, `crates/neon-observability/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-observability` = passed（6 tests）; `cargo test --workspace` = passed; `cargo check --workspace` = passed; `git diff --check` = passed; 未启动窗口
commit: pending（仅提交 M4 files 和本条 Progress 记录；不包含用户/并行修改的 `.gitignore` 或未跟踪 `AGENTS.md`）
remaining: M5 UI declaration schema
next: 重新读取 `AGENTS.md`、Progress、目录和 Git 状态后，为 `neon-ui-schema` 先新增 static fragment schema tests
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M4 | 进行中
files: `crates/neon-observability/Cargo.toml`, `crates/neon-observability/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-observability` = not-run; `cargo test --workspace` = not-run; M4 无 service，故 `service.describe` / snapshot = not available; 未启动窗口
commit: none
remaining: 运行 M4 journal/redaction contract tests 与 workspace tests
next: 运行 `cargo test -p neon-observability`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 已完成
files: `Cargo.lock`, `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = passed（8 tests）; `cargo test --workspace` = passed; `cargo check --workspace` = passed; `git diff --check` = passed; 未启动窗口
commit: pending（仅提交 M3 IPC 和本条 Progress 记录；不包含未跟踪 `.opencode/`、`AGENTS.md`）
remaining: M4 可观察性与 command journal
next: 重新读取 `AGENTS.md`、Progress、目录和 Git 状态后，为 `neon-observability` 先新增 journal 与脱敏 contract tests
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 进行中
files: `crates/neon-ipc/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = failed（45+ duplicate-definition/API-mismatch errors；同一文件被并行追加为两套不兼容实现）; 未启动窗口
commit: none
remaining: 已按计划 API 收敛为唯一 loopback length-prefixed JSON 实现及完整 M3 test suite；需运行 M3 最小检查并修复结果
next: 运行 `cargo test -p neon-ipc`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M0 | 已完成
files: `plan/neon3-first-work-plan.md`
checks: 计划已写入；Neon3 当前只有 `AGENTS.md`，尚无 Cargo workspace；未启动窗口
commit: none（`D:\Neon3` 当前不是 Git 仓库）
remaining: M1 到 M9
next: 创建 workspace 和第一期 crate 空骨架
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 阻塞
files: `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = failed; `cargo test --workspace` = failed。已排查：先缺少直接 `serde` 依赖，随后发现并行编辑追加第二套不兼容 IPC 实现；当前同一文件包含重复 `TransportError`、`encode_frame`、`decode_frames`、`RpcClient`、`RpcServer`、helper 与 test module，编译报告 45+ duplicate-definition/API-mismatch errors；未启动窗口
commit: none
remaining: 合并或选择唯一的 M3 IPC API 和对应测试，消除冲突后运行 M3 最小检查
next: 用户指定保留当前文件前半段实现、后半段实现，或授权我以计划 API 为准统一重写 `crates/neon-ipc/src/lib.rs`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 进行中
files: `crates/neon-ipc/Cargo.toml`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = failed（并行变更造成重复 `serde_json.workspace` manifest key）; `cargo test --workspace` = failed（同一原因）; 未启动窗口
commit: none
remaining: 移除重复 manifest key 后重跑 M3 检查
next: 运行 `cargo test -p neon-ipc`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 进行中
files: `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = failed（缺少直接 `serde` 依赖，无法解析 transport serialization trait bounds）; `cargo test --workspace` = failed（同一原因）; 未启动窗口
commit: none
remaining: 添加 `serde.workspace = true` 并重跑 M3 检查
next: 运行 `cargo test -p neon-ipc`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M3 | 进行中
files: `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-ipc` = not-run; `cargo test --workspace` = not-run; M3 无 service，故 `service.describe` / snapshot = not available; 未启动窗口
commit: none
remaining: 运行 M3 framing/lifecycle tests 与 workspace tests
next: 运行 `cargo test -p neon-ipc`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M2 | 已完成
files: `crates/neon-protocol/src/lib.rs`, `tests/fixtures/protocol/request.json`, `tests/fixtures/protocol/accepted-response.json`, `tests/fixtures/protocol/revision-conflict-response.json`, `tests/protocol_contract.rs`（并行工作产生的同范围 contract coverage）, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-protocol` = failed（初始 JSON 字段顺序断言）；`cargo check --workspace` = passed; `cargo test -p neon-protocol` = passed（13 tests）; `cargo check --workspace` = passed; 未启动窗口
commit: `8876071` (`Define public protocol schema`); completion bookkeeping record follows in a separate commit
remaining: M3 IPC framing 与 request lifecycle
next: 下一 cycle 重新读取固定上下文，选择 M3 后先为 length-prefixed frame 边界建立测试
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M2 | 已完成
files: `plan/neon3-first-work-plan.md`
checks: `git diff --check` = passed; M2 required checks remain `cargo test -p neon-protocol` = passed（13 tests）与 `cargo check --workspace` = passed; 未启动窗口
commit: `8876071`（公共协议 schema）；本条 Progress bookkeeping commit 待创建
remaining: M3 IPC framing 与 request lifecycle
next: 下一 cycle 重新读取 `AGENTS.md`、Progress、目录与 Git 状态，选择 M3 后先建立 framing boundary tests
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M2 | 进行中
files: `crates/neon-protocol/src/lib.rs`, `tests/fixtures/protocol/request.json`, `tests/fixtures/protocol/accepted-response.json`, `tests/fixtures/protocol/revision-conflict-response.json`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-protocol` = not-run; `cargo check --workspace` = not-run; M2 无 service，故 `service.describe` / snapshot = not available; 未启动窗口
commit: none
remaining: 运行 M2 contract tests 与 workspace check
next: 运行 `cargo test -p neon-protocol`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M2 | 进行中
files: `crates/neon-protocol/src/lib.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-protocol` = failed（fixture round-trip 测试比较 JSON 字段顺序而非结构化值）; `cargo check --workspace` = passed; 未启动窗口
commit: none
remaining: 以结构化 JSON 比较修复 fixture round-trip tests 后重跑 M2 检查
next: 运行 `cargo test -p neon-protocol`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M1 | 进行中
files: `.gitignore`, `Cargo.toml`, `crates/neon-protocol/Cargo.toml`, `crates/neon-protocol/src/lib.rs`, `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `crates/neon-observability/Cargo.toml`, `crates/neon-observability/src/lib.rs`, `crates/neon-ui-schema/Cargo.toml`, `crates/neon-ui-schema/src/lib.rs`, `crates/neon-wgpu-runtime/Cargo.toml`, `crates/neon-wgpu-runtime/src/main.rs`, `crates/neon-ui-runtime/Cargo.toml`, `crates/neon-ui-runtime/src/main.rs`, `crates/neon-cli/Cargo.toml`, `crates/neon-cli/src/main.rs`, `tests/.gitkeep`, `tests/fixtures/.gitkeep`, `plan/neon3-first-work-plan.md`
checks: `git rev-parse --is-inside-work-tree` = failed（尚未初始化）; `git init` = passed; `cargo check --workspace` = not-run; `cargo test --workspace` = not-run; 未启动窗口
commit: none
remaining: 运行 M1 最小检查，检查 diff 后创建 M1 初始提交
next: 运行 `cargo check --workspace`
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M1 | 已完成
files: `.gitignore`, `Cargo.toml`, `Cargo.lock`, `crates/neon-protocol/Cargo.toml`, `crates/neon-protocol/src/lib.rs`, `crates/neon-ipc/Cargo.toml`, `crates/neon-ipc/src/lib.rs`, `crates/neon-observability/Cargo.toml`, `crates/neon-observability/src/lib.rs`, `crates/neon-ui-schema/Cargo.toml`, `crates/neon-ui-schema/src/lib.rs`, `crates/neon-wgpu-runtime/Cargo.toml`, `crates/neon-wgpu-runtime/src/main.rs`, `crates/neon-ui-runtime/Cargo.toml`, `crates/neon-ui-runtime/src/main.rs`, `crates/neon-cli/Cargo.toml`, `crates/neon-cli/src/main.rs`, `tests/.gitkeep`, `tests/fixtures/.gitkeep`, `plan/neon3-first-work-plan.md`
checks: `git rev-parse --is-inside-work-tree` = failed（初始化前）; `git init` = passed; `cargo check --workspace` = passed; `cargo test --workspace` = passed（1 个依赖边界测试通过）; 未启动窗口
commit: `d378d0b` (`Create Neon3 workspace skeleton`); Progress completion record follows in a separate bookkeeping commit
remaining: M2 公共协议 schema
next: 读取当前 Progress、目录和 Git 状态后，为 `neon-protocol` 新增 request/response fixture round-trip 测试
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M2 | 已完成
files: `crates/neon-protocol/src/lib.rs`, `crates/neon-protocol/tests/protocol_contract.rs`, `plan/neon3-first-work-plan.md`
checks: `cargo test -p neon-protocol` = passed（7 tests: 1 dependency-boundary unit test, 6 protocol contract tests）; `cargo check --workspace` = passed; 未启动窗口
commit: none
remaining: M3 IPC framing 与 request lifecycle
next: 读取当前 Progress、目录和 Git 状态后，为 `neon-ipc` 建立 framing boundary tests
user_acceptance: 未开始；未授权启动 Neon3 窗口

2026-08-13 | M1 | 已完成
files: `plan/neon3-first-work-plan.md`
checks: `git diff --check` = passed; `git status --short` = passed（M1 workspace 已提交，预存未跟踪 `Neon3/.git` 未纳入）；未启动窗口
commit: `d378d0b`（workspace 骨架）；本条 Progress bookkeeping commit 待创建
remaining: M2 公共协议 schema
next: 下一 cycle 重新读取 `AGENTS.md`、Progress、目录与 Git 状态，选择 M2 并先新增 protocol fixture tests
user_acceptance: 未开始；未授权启动 Neon3 窗口
