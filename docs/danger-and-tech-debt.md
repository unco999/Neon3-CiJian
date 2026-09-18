# Neon3 现实代码危险警告与低效代码盘点

> 本文档是
>
> **只读盘点**
>
> （2026-09-18），未修改任何代码。
> 给后续 AI 看的红线清单：这些地方碰之前必须知道风险。



***

## 1. 数字概览（按行数）



| crate               | .rs 文件数 | 总行数        | unwrap  | expect | panic | unsafe |
| ------------------- | ------- | ---------- | ------- | ------ | ----- | ------ |
| neon-wgpu-runtime   | 32      | **52 624** | 112     | 69     | 0     | **32** |
| neon-ui-runtime     | 24      | **28 719** | **192** | 30     | 0     | 0      |
| neon-gpu-ecs        | 25      | 4 581      | —       | —      | —     | —      |
| neon-editor         | 13      | 3 476      | —       | —      | —     | —      |
| neon-wgpu-ai        | 12      | 3 064      | —       | —      | —     | —      |
| neon-dev            | 1       | 2 557      | 31      | 4      | 1     | 3      |
| neon-editor-runtime | 2       | 2 030      | —       | —      | —     | —      |
| neon-protocol       | 2       | 1 296      | —       | —      | —     | —      |
| neon-ipc            | 2       | 1 264      | —       | —      | —     | —      |
| neon-eventd         | 2       | 1 253      | —       | —      | —     | —      |
| neon-cli            | 2       | 1 329      | 47      | 0      | 1     | 0      |
| neon-projectd       | 2       | 552        | —       | —      | —     | —      |
| neon-world-bridge   | 1       | 627        | —       | —      | —     | —      |
| neon-ui             | 1       | 71         | —       | —      | —     | —      |
| neon3-runtime       | 2       | 622        | —       | —      | —     | —      |
| neon-observability  | 1       | 318        | —       | —      | —     | —      |
| neon-languages      | 1       | 439        | —       | —      | —     | —      |
| neon-android-host   | 2       | 286        | —       | —      | —     | —      |

**两个上帝文件**：



* `crates/neon-wgpu-runtime/src/lib.rs` — **16 185 行单文件**

* `crates/neon-ui-runtime/src/lib.rs` — 估计 20 000+ 行



***

## 2. 🔴 高危：会导致进程崩溃或 GPU 挂掉的地方

### 2.1 `neon-ui-runtime/src/lib.rs` 有 192 个 `.unwrap()`

这是 UI 领域进程。任何一个 unwrap 触发，整个 UI 会话就崩。

UI 进程崩溃意味着：



* 用户所有已打开面板的语义状态丢失

* 正在拖拽的 card 直接消失

* eventd 里的事件队列没人消费

**禁止在以下场景新增 unwrap**：



* `handle(RpcRequest)` 函数里 —— 任何外部输入路径

* `apply_frame` / `dispatch_intent` 里 —— 来自 host 的事件路径

* NUI Flow 编译错误处理路径

**现有 unwrap 大多在测试代码和 fixture 里**，但必须逐个 audit 哪些在请求热路径上。改 lib.rs 时**新增代码不允许再用 unwrap**，必须用 `?` 或返回 `RpcError`。

### 2.2 `neon-wgpu-runtime/src/lib.rs` 有 32 处 `unsafe`

分类：



| 类型                                                                                                                                                                                                              | 行号                                              | 风险                                                                                                                                         |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Windows API（GetWindowLongPtrW / SetWindowLongPtrW / CreatePolygonRgn / SetWindowRgn / CoInitializeEx / CreateDispatcherQueueController / ChangeWindowMessageFilterEx / GetOpenFileNameW / CommDlgExtendedError） | 119/124/188/192/212/221/566/569/572/619/656/662 | 低：Win32 标准调用，参数正确即可                                                                                                                        |
| `unsafe impl Send/Sync for SharedSurface`                                                                                                                                                                       | **252-253**                                     | **中**：注释声称 "owning HeadlessExternalGpu serializes access through a Mutex"，但必须确认 SharedSurface 真的只被 Mutex 保护，否则跨线程竞态会破坏 D3D12 shared handle |
| wgpu HAL Dx12 直调（`as_hal::<Dx12>()` + `add_signal_fence`）                                                                                                                                                       | 4891/5017/6493/6496/6536/6539                   | **中高**：这是 external host interop 路径，fence 值算错会让 GPU 挂起或画面撕裂                                                                                 |
| `wgpu::SurfaceTargetUnsafe` 创建 surface                                                                                                                                                                          | 6730/6744/6746                                  | 中：headless/external surface 路径                                                                                                             |
| SPSC ring buffer 无界索引 `ring.get(index)`                                                                                                                                                                         | **4837/4847/4857**                              | **高**：`unsafe { ring.get(index) }` 这种写法可疑 ——`get()` 本身是 safe 的，包一层 unsafe 没意义；要么是 transmute，要么是 FFI。改这块之前必须读上下文                            |

### 2.3 单进程发布宿主 `neon3-runtime`

`crates/neon3-runtime/src/main.rs` 在一个进程里起 4 个线程（eventd/ui/wgpu/editor）。

**风险**：任何一个线程 panic 会带走整个进程，包括窗口。目前没有观察 - 重启（AGENTS.md 第 23 节要求的 supervision 没实现）。



***

## 3. 🟠 中危：架构违反或静默错误

### 3.1 CLI target 字段硬编码（`crates/neon-cli/src/lib.rs:351`）



```
target: ServiceName("wgpu-runtime".into()),
```

后果：



* 用户跑 `neon-cli debug snapshot <ui-endpoint>` 时，请求 envelope 里 target 写的是 `wgpu-runtime`，但实际打到 ui-runtime。

* ui-runtime forwarder 不校验 target，所以 "碰巧能用"。

* 一旦未来 ui-runtime 开始按 target 路由（这是 AGENTS.md 第 7 节要求的），**所有现存 CLI 命令会突然全部报 unsupported\_method**。

### 3.2 debug.snapshot.get 双份实现，行为不一致



* wgpu-runtime 版：返回 layout/hit/pointer/viewport/backdrop 一大堆内部态

* ui-runtime 版（`lib.rs:3255`）：**只返回 service/epoch/revision/health/capabilities/active\_jobs**，不返回当前 fragment

AI 查 UI 状态必须调两个 endpoint。任何一个改了字段，另一个不会跟着变。

### 3.3 三套进程拓扑并存



| 启动方式                            | 端口策略                        | 用途   |
| ------------------------------- | --------------------------- | ---- |
| `neon-dev case <name>`          | 随机                          | 开发自测 |
| `neon3-runtime serve`           | 固定 39101-39104              | 发布   |
| `scripts/run-neon-services.ps1` | 固定 39101-39104，domain 指向死端口 | 手动调试 |

**风险**：同一个 endpoint（39104）在 neon3-runtime 里是 editor-runtime，在 ps1 脚本里是 dead domain，在 Python SDK `cli.py:60` 里被硬编码成 domain endpoint。三方认知不一致。

### 3.4 没有 revision\_conflict 真实测试

AGENTS.md 第 13 节要求 `expected_revision` 不匹配返回 `revision_conflict`。

实际 5 个 scenario 里**没有一个**测 revision 冲突路径。并发写路径等于没测。

### 3.5 eventd 默认绑定 loopback，但 `neon3-runtime serve` 没显式关外网

已 grep 确认没有 `0.0.0.0` 绑定，但**没有显式的 "拒绝非 loopback" 开关**。任何一个服务改 bind 地址都会变成监听全网卡。AGENTS.md 第 13 节 "默认仅绑定 loopback" 是靠约定，不是靠代码强制。



***

## 4. 🟡 低效代码与技术债

### 4.1 上帝文件



* `crates/neon-wgpu-runtime/src/lib.rs`：16 185 行

* `crates/neon-ui-runtime/src/lib.rs`：估计 20 000+ 行

单文件 1 万行以上意味着：



* 编译时间长（cargo check 一次要几十秒）

* 任何改动都要全量重编

* merge conflict 高频

* 新人无法导航

**建议拆分**（不是现在做，列入债务）：



* wgpu-runtime/lib.rs 按子模块拆：`window_shell.rs` / `dx12_interop.rs` / `ui_renderer.rs` / `world_ui.rs` / `ai_engine_bridge.rs` / `rpc_handlers.rs`

* ui-runtime/lib.rs 拆：`forwarder.rs` / `nui_compiler.rs` / `host_adapter.rs` / `interaction.rs` / `journal.rs`

### 4.2 30+ probe bin 散落在 src/bin/



* `crates/neon-ui-runtime/src/bin/`：11 个

* `crates/neon-wgpu-runtime/src/bin/`：20+ 个

* `crates/neon3-runtime/src/bin/`：1 个

* `D:\Neon3Sdk\...\bin\*.py`：3 个

每个 bin 都是独立 main.rs，自己 spawn 服务、自己硬编码 endpoint。

**问题**：



* 重复代码（每个 bin 都写一遍 reserve\_loopback /wait\_healthy）

* 无法批量跑

* 不知道哪个还在用、哪个是死代码

* `cargo build --workspace` 会把它们全编译一遍，拖慢 CI

### 4.3 硬编码端口 39104

Python SDK `cli.py:60` 写死 `"127.0.0.1:39104"` 作为 domain endpoint。



* 在 neon3-runtime 单进程模式下，39104 是 editor-runtime（不是 domain）

* 在 neon-dev 多进程模式下，domain 是随机端口

* 在 ps1 脚本里，39104 没人监听

**这是一个三方不一致的硬编码。**

### 4.4 硬编码 case 名

`neon-dev/src/main.rs:55-62` 把 6 个 case 名写死：



```
kanban-reparent / asset-review / component-gallery / data-grid / scroll-view / virtual-list
```

新加一个 case 要改 Rust 代码、重新编译。AGENTS.md 第 20 节要求的 YAML scenario 没实现。

### 4.5 tests/ 目录是空壳



```
tests/

&#x20; fixtures/

&#x20;   ui/\*.nui

&#x20;   protocol/\*.json
```

**没有一个&#x20;**`tests/*.rs`**&#x20;集成测试文件。** 所有 "测试" 都是 probe bin（手跑）或硬编码 scenario。

### 4.6 死代码与残留



* `scripts/start-ui-case.cmd`：`cd packages/neon-ui-react-client`，目录已删，脚本自己第 25 行就报错

* `plan/neon3-ui-react-client.md`：React 客户端设计文档，React 客户端已删

* `crates/neon-wgpu-runtime/src/lib.rs.broken-backup`：已在 .gitignore

* 根目录～50 个临时 Python 脚本（`apply_changes_*.py`、`scope_*.py`、`heart*.py`、`shader_v*.py`、`recolor_*.py`）—— 已被 `*.py` 全局 ignore，但还在工作目录里

### 4.7 没有日志级别过滤

`neon-observability` 只有 `TraceLevel::{Info,Warn,Error}`，没有 runtime 动态调整。生产环境想关 debug trace 要重编译。



***

## 5. 🔵 与 AGENTS.md 设计的偏差

### 5.1 AGENTS.md 规划但**不存在**的 crate



| 规划名                     | 现状                   |
| ----------------------- | -------------------- |
| `neon-terrain-runtime`  | 不存在                  |
| `neon-resource-runtime` | 不存在                  |
| `neon-sessiond`         | 不存在（neon-dev 顶替部分职责） |
| `neon-testkit`          | 不存在                  |

### 5.2 AGENTS.md 没提但**实际存在**的 crate



| crate                                 | 职责                                   | 行数        |
| ------------------------------------- | ------------------------------------ | --------- |
| `neon-editor` / `neon-editor-runtime` | NUI Flow 代码编辑器（文档 / LSP/completion）  | 3476+2030 |
| `neon-gpu-ecs`                        | IR 驱动的 GPU ECS runtime（compute-only） | 4581      |
| `neon-wgpu-ai`                        | UNet/DDIM 地形生成推理（compute-only）       | 3064      |
| `neon-world-bridge`                   | world-space 相机 / 锚点同步契约              | 627       |
| `neon-android-host`                   | Android JNI host                     | 286       |
| `neon-languages`                      | 编辑器语言支持                              | 439       |
| `neon3-runtime`                       | 单进程发布宿主                              | 622       |
| `neon-ui`                             | 聚合 re-export（给外部 host 用）             | 71        |

**AGENTS.md 第 1 节进程图完全没画 editor-runtime /gpu-ecs/wgpu-ai /world-bridge。** 这些是后加的，架构文档没跟上。

### 5.3 AGENTS.md 第 11 节服务方法 vs 实际

AGENTS.md 列的方法集合 vs 实际 wgpu-runtime/lib.rs 实现的方法（从 grep 结果）：



* ✅ 已实现：`service.health/describe/shutdown`、`debug.snapshot.get/command.get/trace.query`、`debug.interaction.get/query`、`wgpu.ui.submit_fragment/remove_fragment`、`wgpu.render.diagnostics/graph.snapshot/target.capture/target.assert`、`wgpu.resource.inspect/preload/wait_ready`

* ❌ AGENTS.md 写了但**没实现**：`service.subscribe`、`debug.trace.subscribe`、`debug.journal.query`、`debug.replay.export`、`debug.diagnostics.get`、`debug.health.check`

* ➕ 实际有但 AGENTS.md 没写：`ui.flow.submit/compile`、`ui.host.inbound`、`ui.host.pointer_event`、`ui.host.keyboard_event`、`ui.input.frame`、`debug.window.input.*`、`debug.window.inspect`、`editor.document.*`、`editor.completion.request`、`render.surface.*`、`render.backend.negotiate`、`wgpu.world.info.configure`、`wgpu.world.camera.submit_frame`、`wgpu.shader.register/state`、`wgpu.ai.terrain.generate/model.status`



***

## 6. 🚫 不要碰的地方（除非你知道自己在干什么）



1. `crates/neon-wgpu-runtime/src/lib.rs:252-253`**&#x20;的&#x20;**`unsafe impl Send/Sync for SharedSurface`。删了会编译错，改了要 audit 所有访问点。

2. `crates/neon-wgpu-runtime/src/lib.rs:4837/4847/4857`**&#x20;的&#x20;**`ring.get(index)`**&#x20;unsafe 块**。这是 SPSC ring buffer 热路径，改错会丢交互事件。

3. `crates/neon-wgpu-runtime/src/lib.rs:5017/6493/6536`**&#x20;的 Dx12 interop fence**。fence 值算错会让外部 host 卡死。

4. `crates/neon-cli/src/lib.rs:351`**&#x20;的 target 字段**。在 CLI target 路由重构完成前，不要单独改这一行 —— 会让 ui-endpoint 调用全挂。

5. `crates/neon-ui-runtime/src/lib.rs:3255`**&#x20;的&#x20;**`debug.snapshot.get`**&#x20;返回结构**。Python SDK `ui.py:117` 和 Node SDK `ui.ts:119` 都按这个结构反序列化。加字段可以，改字段名会炸 SDK。

6. `crates/neon-wgpu-runtime/src/lib.rs:3366`**&#x20;的&#x20;**`debug.snapshot.get`**&#x20;返回结构**。`neon-cli debug snapshot` 直接打印整个 JSON。改字段名会让 `docs/ai-debug-workflow.md` 里的示例全失效。

7. **端口 39101-39104**。这是 `neon3-runtime serve` 的固定端口，Python SDK `cli.py` 默认连这些端口。改端口要同时改 SDK。

8. `crates/neon-wgpu-runtime/src/lib.rs`**&#x20;单文件 16185 行**。不要试图一次性拆分。要拆就一次拆一个子模块，拆完跑 `cargo check -p neon-wgpu-runtime`。



***

## 7. 测试工具与 git 提交纪律

### 7.1 已经在 .gitignore 里的（不要取消 ignore）



```
\*.py                                # 所有根目录 Python 脚本（临时工具）

/docs/ai-diary/                     # AI 工作日记

/docs/ai-debug-workflow.md          # AI 调试工作流（内部文档）

/.workbuddy/

/.plan-client/

/opencode.json

/artifacts/

/.release-tmp

/shots

/\*.png (current\_state.png 等)
```

### 7.2 本次新增的文档



| 文件                                                          | 是否进 git                         |
| ----------------------------------------------------------- | ------------------------------- |
| `docs/neon-cli-sdk-debug-refactor.md`                       | **进 git**（重构资料，公开）              |
| `docs/ai-diary/2026-09-18-cli-sdk-debug-refactor-survey.md` | **不进 git**（ai-diary 整目录 ignore） |
| `docs/danger-and-tech-debt.md`（本文档）                         | **进 git**（危险警告，团队共享）            |

### 7.3 后续 AI 产生的临时文件



* 截图 probe 输出（`*.png`）→ 已经被 `*.png` 模式部分覆盖，但 `current_state.png` 是显式列的。新截图建议放 `/shots/` 目录（已 ignore）。

* 一次性 Python 脚本 → 已经被 `*.py` 全局 ignore。**不要把 .py 脚本移到别的扩展名**，否则会进 git。

* probe 输出 JSON → 不要 commit。

* 不要把 `target/` 目录里的东西加进 git。

### 7.4 绝对不要 commit 的东西



* 任何包含 token / 密码 / 私钥的文件

* `examples/android-runtime/local.properties`（已 ignore）

* `examples/android-runtime/app/src/main/jniLibs/`（已 ignore，38-241 MB .so）

* 个人 WorkBuddy /plan-client 配置（已 ignore）



***

## 8. 给后续 AI 的操作清单

如果你要改代码，按这个顺序：



1. **先读** `AGENTS.md` + 本文档 + `docs/neon-cli-sdk-debug-refactor.md`。

2. **不要动**第 6 节列的 8 个地方，除非你改的就是它们。

3. **新增代码不允许 unwrap/expect/panic**。所有外部输入路径必须返回 `RpcError`。

4. **新增 RPC method** 时：

* 先在 `neon-protocol` 里加错误码

* 服务端 handler 返回结构化 `RpcError { code, message, details }`

* 在 SDK 的 `constants.py` / `constants.ts` 里同步加常量

* 更新本文档第 5.3 节的方法清单

1. **跑验证**：

* `cargo check --workspace`

* `cargo test -p neon-ui-schema -p neon-protocol -p neon-observability`

* 改动 wgpu-runtime 时跑 `cargo test -p neon-wgpu-runtime --lib`

1. **写日记**：在 `docs/ai-diary/` 写当天记录（已 gitignore，不影响公开仓库）。