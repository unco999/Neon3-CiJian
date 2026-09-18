# Neon3 CLI 统一 SDK Debug 流程 — 重构资料与开放接口设计

> 本文档是给实施 AI 的**资料包**，不是决策书。所有结论都来自对 `D:\Neon3` 与 `D:\Neon3Sdk` 的只读调查（2026-09-18）。
> 目标：让 `neon-cli`（Rust，workspace 内 `crates/neon-cli`）成为**所有 SDK debug / 诊断 / 自测流程的唯一入口**，收敛目前散落在三个仓库、三套进程拓扑、三十多个 probe bin 里的调试能力。

---

## 0. 一句话现状

现在有**两个 CLI**：

| CLI | 位置 | 干什么 |
|---|---|---|
| `neon-cli`（Rust） | `D:\Neon3\crates\neon-cli` | 内部调试用，子命令：`scenario` / `debug` / `event`。target 硬编码 `wgpu-runtime`。 |
| `neon3-sdk`（Python，即 `neon3-sdk cli.py`） | `D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\cli.py` | 对外发布的 PyPI 包，只有 `dev up` 和 `calculator` 两个命令，**不做 debug**。 |

另外还有第三个"启动器"：`neon-dev`（Rust，`crates/neon-dev/src/main.rs`，2674 行），负责 `case <name>` 启动多进程 + 跑 scenario + 截图。

实施目标：**让 `neon-cli` 吃掉 `neon-dev` 的启动/scenario/截图能力，同时让 `neon3-sdk`（Python/Node/Rust SDK）不再自己实现 debug 子命令，而是通过协议把 debug 请求统一打到 `neon-cli` 提供的"单 endpoint 聚合调试面"**。

---

## 1. 调查范围与证据来源

只读调查过的路径：

- `D:\Neon3\crates\neon-cli\src\{main.rs,lib.rs}`（1138 行 lib）
- `D:\Neon3\crates\neon-dev\src\main.rs`（2674 行）
- `D:\Neon3\crates\neon-wgpu-runtime\src\{main.rs,lib.rs}`（lib 16185 行）
- `D:\Neon3\crates\neon-ui-runtime\src\{main.rs,lib.rs,host_adapter.rs}` + `src/bin/*.rs`（11 个 probe bin）
- `D:\Neon3\crates\neon3-runtime\src\main.rs`（单进程发布宿主）
- `D:\Neon3\crates\neon-editor-runtime`（editor 服务）
- `D:\Neon3\crates\neon-eventd` / `neon-projectd`
- `D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\*.py`（24 个源文件）
- `D:\Neon3Sdk\packages\node-sdk\src\*.ts`（client/session/render/ui/input/event/capabilities/routing/editor）
- `D:\Neon3Sdk\scripts\sync-neon3-stack.ps1`
- `D:\Neon3\docs\ai-debug-workflow.md`
- `D:\Neon3\docs\ai-diary\2026-09-18-nui-flow-diagnostics.md`
- `D:\Neon3\docs\neon3-ai-authoring-integration.md`
- `D:\Neon3\plan\neon3-ui-react-client.md`（已废弃，React 客户端目录已删）
- `D:\Neon3\AGENTS.md`（架构宪法）

---

## 2. 现状：进程拓扑有三套并存

### 2.1 `neon-dev case <name>`（开发自测主路径）

`crates/neon-dev/src/main.rs:160-365` 用 `reserve_loopback_endpoint()` 随机端口，spawn：

```
neon-eventd                          端口随机
neon-projectd                       仅 component-gallery case 起
neon-wgpu-runtime --window-server <wgpu> <ui> [<projectd>] --eventd <eventd>
demo_domain_controller               "host/domain" 进程（或 component_gallery_domain_controller）
neon-ui-runtime --forward-server <ui> <wgpu> <domain> --eventd <eventd>
nui_flow_demo <case> <ui>            一次性 submitter，跑完即退
```

case 名硬编码（`main.rs:55-62`）：`kanban-reparent` / `asset-review` / `component-gallery` / `data-grid` / `scroll-view` / `virtual-list`。

启动完打印 manifest JSON：

```json
{"status":"case_ready","case":"component-gallery",
 "eventd_endpoint":"127.0.0.1:52341",
 "ui_endpoint":"127.0.0.1:52342",
 "wgpu_endpoint":"127.0.0.1:52343",
 "domain_endpoint":"127.0.0.1:52344",
 "projectd_endpoint":"127.0.0.1:52345",
 "pid":12345}
```

### 2.2 `neon3-runtime serve --window`（发布物，根目录那个 .exe）

`crates/neon3-runtime/src/main.rs:233-468`，单进程内起 4 个线程，**固定端口**：

```
eventd  127.0.0.1:39101
ui      127.0.0.1:39102
wgpu    127.0.0.1:39103
editor  127.0.0.1:39104
```

注释里写着"scripts/cli.py or SDK"——指的就是 Python SDK 的 `cli.py`。这是打包分发形态，**不是 AI 自测路径**。

### 2.3 `scripts/run-neon-services.ps1`（手动）

直接 `Start-Process` 起 eventd 39101 / wgpu 39103 / ui 39102，domain 指向 39104 但没人监听（dead domain）。

### 2.4 `scripts/start-ui-case.cmd`（已死）

`cd packages/neon-ui-react-client && npm run`，但该目录在仓库里**已经不存在**，脚本第 25-29 行会自己报错退出。是旧 React 客户端时代的残留，应删除。

---

## 3. 现状：CLI 能力盘点

### 3.1 `neon-cli` 现有子命令（`crates/neon-cli/src/main.rs`）

```
neon-cli scenario <id> --headless        只支持 2 个 id：ui.static-fragment.submit.v1 / ui.detail-toggle.v1
                                          自己 spawn neon-wgpu-runtime --headless-server
                                          完全不走 ui-runtime / host / projectd
neon-cli debug snapshot <endpoint>        → debug.snapshot.get
neon-cli debug interaction get <ep> <id> → debug.interaction.get
neon-cli debug interaction query <ep>     → debug.interaction.query
neon-cli debug render capture <ep> <png>  → wgpu.render.target.capture
neon-cli debug world-ui capture <ep> <png> [--target T] [--redraw]
                                          → render.surface.capture_png
neon-cli debug world-ui camera <ep>       → wgpu.world.camera.submit_frame
neon-cli event snapshot <ep>              → event.snapshot
neon-cli event subscribe <ep> <filter>     → event.subscribe
```

### 3.2 已知 Bug：target 字段硬编码

`crates/neon-cli/src/lib.rs:351`：

```rust
let envelope = Envelope {
    protocol: Protocol::default(),
    version: ProtocolVersion::default(),
    request_id,
    client: ClientIdentity { kind: "cli".into(), instance_id, pid },
    target: ServiceName("wgpu-runtime".into()),   // ← 写死
    method,
    params,
    expected_revision: None,
    idempotency_key,
};
```

`docs/ai-debug-workflow.md` 让 AI 跑 `neon-cli debug snapshot <ui-endpoint>` 也能用，**纯粹是因为 ui-runtime 的 forwarder 按 method 路由、不校验 target 字段**（`crates/neon-ui-runtime/src/lib.rs:3560+` 的 `handle` 函数不看 `request.target`）。

这违反 AGENTS.md 第 7 节"统一 envelope 按 target 分发"。**重构时必须修**。

### 3.3 `neon-dev` 现有子命令（`crates/neon-dev/src/main.rs`）

```
neon-dev case <name>                    启动多进程，打印 manifest
neon-dev scenario <id>                  只支持 3 个 id：
                                        drag-card02-before / component-gallery-interactions / component-gallery-window-input
neon-dev capture-window <png>           调 wgpu.render.target.capture
neon-dev inspect-window                 调 debug.window.inspect
neon-dev probe-window [--case N] [--width W --height H]
neon-dev probe-window-metrics
neon-dev debug-interaction <interaction-id>
neon-dev status
```

### 3.4 Python SDK 的 `neon3-sdk` CLI（`D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\cli.py`）

```
neon3-sdk dev up [--neon-root ...] [--gallery] [--once]
                                        起 eventd/wgpu/ui 三个进程（固定端口 39101-39103）
neon3-sdk calculator [--headless] [--once]
                                        起 Python CalculatorServer 作为 domain，跑 1+1=3 场景
```

**它不做 debug。** 它的 debug 能力全部散在 SDK 各模块里（`ui.py` / `render.py` / `input.py` / `session.py`），通过直接 RPC 调用，没有统一 CLI 子命令。

---

## 4. 现状：SDK 已经在用哪些 RPC method

下面是从 Python SDK 和 Node SDK 源码里 grep 出来的**全部 RPC method**。这是重构 CLI 时必须覆盖的方法全集。

### 4.1 服务自描述

| method | target | 调用方 |
|---|---|---|
| `service.health` | 任意 | client.py / cli.py / 所有 probe |
| `service.describe` | 任意 | client.py / component_gallery_probe |
| `service.shutdown` | wgpu-runtime | android.py |

### 4.2 UI 层（ui-runtime）

| method | 说明 | SDK 调用点 |
|---|---|---|
| `ui.flow.submit` | 提交 .nui 源码 | ui.py:74, calculator.py:187, nui.py |
| `ui.flow.compile` | dry-run 编译（新） | 2026-09-18 新增，SDK 尚未接 |
| `ui.host.inbound` | 语义事件下发 | ui.py:95, session.py:251, calculator.py:204 |
| `ui.input.frame` | input frame 批量提交 | ui.py:100, session.py:334/350/393 |
| `ui.host.pointer_event` | 指针事件 | input.py:32, render.py:291 |
| `ui.host.keyboard_event` | 键盘事件 | input.py:39 |
| `debug.snapshot.get` | 服务快照 | ui.py:117, app.py:199, calculator.py:171 |
| `debug.ui.host.snapshot` | host 状态快照 | ui.py:120/136, session.py:156 |
| `debug.trace.query` | 查 trace | ui.py:129 |
| `debug.window.input.snapshot` | 输入快照 | input.py:42 |

### 4.3 WGPU 渲染层（wgpu-runtime）

| method | 说明 | SDK 调用点 |
|---|---|---|
| `wgpu.render.diagnostics` | 诊断 | client.py:107, render.py:260 |
| `wgpu.render.graph.snapshot` | render graph | render.py:267, component_gallery_probe |
| `wgpu.render.target.capture` | 截图 | render.py:282, neon-cli debug render capture |
| `wgpu.ui.fragment.snapshot` | 当前 fragment 内容 | cli.py:235, component_gallery_probe, inventory_sdk_probe |
| `wgpu.ui.set_view_extras` | 视图附加 | constants.py |
| `wgpu.shader.register` | 注册 shader | render.py:183 |
| `wgpu.shader.state` | shader 状态 | render.py:188 |
| `wgpu.world.info.configure` | world 配置 | render.py:288 |
| `wgpu.world.camera.submit_frame` | 相机帧 | render.py:285, neon-cli debug world-ui camera |
| `render.backend.negotiate` | backend 协商 | render.py:264 |
| `render.surface.open` | surface 打开 | render.py:294, api_contract_probe |
| `render.surface.acquire` | surface 获取 | render.py:369, api_contract_probe |
| `render.surface.frame` | surface 帧 | render.py:375, api_contract_probe |
| `render.surface.capture_png` | surface 截图 | render.py:386, neon-cli debug world-ui capture |

### 4.4 Editor 层（editor-runtime）

| method | 说明 |
|---|---|
| `editor.document.open` | editor.py:290 |
| `editor.document.snapshot.get` | editor.py:301 |
| `editor.document.change.apply` | editor.py:335 |
| `editor.document.change.commit` | editor.py:345 |
| `editor.completion.request` | editor.py:367 |
| `editor.document.close` | editor.py:386 |

### 4.5 Eventd

| method | 说明 |
|---|---|
| `event.snapshot` | neon-cli event snapshot |
| `event.subscribe` | neon-cli event subscribe |
| 事件名：`shader.event` / `ui.file_drop.accepted` / `ui.click_blank` | event.py |

### 4.6 WGPU Runtime lib.rs 里**还实现了但 SDK/CLI 都没封装**的方法

从 `crates/neon-wgpu-runtime/src/lib.rs` grep 出来的全部 method，**未被 SDK 或 neon-cli 调用**的有：

```
debug.command.get
debug.interaction.get
debug.interaction.query
debug.journal.query
debug.replay.export
debug.trace.subscribe
debug.ui.host.snapshot
debug.window.input.activate_target
debug.window.input.hover_target
debug.window.input.inspect
debug.window.input.snapshot
debug.window.inspect
service.subscribe
ui.host.pointer_event
ui.host.keyboard_event
ui.flow.submit
ui.flow.compile
ui.fragment.submit
ui.fragment.remove
ui.render.patch
wgpu.ui.submit_fragment
wgpu.ui.remove_fragment
wgpu.ui.animation.*          (一组)
wgpu.ui.hit_test
wgpu.ui.set_view_extras
wgpu.resource.inspect
wgpu.resource.preload
wgpu.resource.wait_ready
wgpu.terrain.attach_context
wgpu.terrain.apply_command
wgpu.terrain.resource_status
wgpu.ai.terrain.generate
wgpu.ai.terrain.model_status
```

**这些就是"开放接口"要补进 CLI 的候选清单。** 现在它们只能手写 JSON envelope 调，SDK 和 CLI 都没封装。

---

## 5. 现状：debug 能力双份实现

`debug.snapshot.get` / `debug.interaction.get` / `debug.interaction.query` / `debug.command.get` / `debug.trace.query` 这五个方法，**wgpu-runtime 和 ui-runtime 各自实现了一份**：

- wgpu-runtime 版（`crates/neon-wgpu-runtime/src/lib.rs:3366-3410`）：返回 layout_counters、layout、dropdown、text_input、active_transitions、pointer_delivery、viewport、shell_frame、window_backdrop——**渲染内部态**。
- ui-runtime 版（`crates/neon-ui-runtime/src/lib.rs:3255-3267`）：只返回 service/epoch/revision/health/capabilities/active_jobs——**几乎是空的**。

后果：AI 查一次交互要打两个 endpoint；ui-runtime 自称是"UI 布局声明权威"（AGENTS.md 第 3 节），但它的 snapshot 根本不返回当前 fragment / host 状态。Python SDK 的 `ui.py:117-129` 是靠**再调一次 `debug.ui.host.snapshot`** 才补到 host 状态的。

---

## 6. 现状：scenario / probe 代码分散

| 位置 | 内容 | 行数/数量 |
|---|---|---|
| `crates/neon-cli/src/lib.rs` | 2 个硬编码 scenario（ui.static-fragment.submit.v1 / ui.detail-toggle.v1） | ~1000 行 |
| `crates/neon-dev/src/main.rs` | 3 个硬编码 scenario + case 启动器 + capture/inspect/probe 辅助命令 | 2674 行 |
| `crates/neon-ui-runtime/src/bin/*.rs` | 11 个独立 probe bin（animation_showcase_interactive_probe / canvas_window_probe / flow_submit_probe / image_resource_probe / nui_flow_demo / nui_flow_diagnostics_probe / nui_guide_render / ui_host_animation_probe / ui_patch_contract_probe / asset_review_demo / demo_domain_controller / component_gallery_domain_controller / neon3_authoring_probe / nui_flow_code_editor_demo） | 每个 100-500 行 |
| `crates/neon-wgpu-runtime/src/bin/*.rs` | 20+ 个 probe bin（component_interaction_probe / glass_fusion_probe / text_material_probe / interaction_latency_probe / world_ui_perf_probe / acrylic_* / composition_* / window_backdrop_probe 等） | — |
| `crates/neon3-runtime/src/bin/code_editor_authority_probe.rs` | 编辑器权威链路 probe | — |
| `D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\bin\*.py` | api_contract_probe / component_gallery_probe / inventory_sdk_probe | 3 个 |
| `D:\Neon3Sdk\packages\node-sdk\src\bin\*.ts` | inventory-sdk-probe / 其他 | — |
| `tests/` | **只有 fixtures**（`tests/fixtures/ui/*.nui`、`tests/fixtures/protocol/*.json`），**没有一个 `tests/*.rs` 集成测试文件** | 空壳 |
| `cases/` | 3 个 .nui case（animation-showcase / component-showcase / grid-pulse） | — |

AGENTS.md 第 20 节要求"声明式 YAML scenario + runner"，第 24 节要求 `neon-testkit` crate。**这两个都不存在。**

---

## 7. 重构目标

### 7.1 最终形态

```
人类 / AI / SDK 任意语言绑定
        │
        │  统一走 neon-cli（单入口、单 binary）
        ▼
┌─────────────────────────────────────────────┐
│  neon-cli                                   │
│  ┌────────────┬────────────┬───────────────┐ │
│  │  dev up    │  debug ... │  scenario ... │ │
│  │  (启动器)   │  (诊断面)   │  (声明式回放)  │ │
│  └─────┬──────┴─────┬──────┴──────┬────────┘ │
└────────┼────────────┼─────────────┼──────────┘
         │            │             │
         ▼            ▼             ▼
    neon-eventd   neon-ui-runtime  neon-wgpu-runtime
    neon-projectd (无窗口 UI)      (唯一窗口/GPU)
    neon-editor-runtime             ▲
         │                          │
         └──── neon-terrain-runtime / resource-runtime (未来补齐)
```

### 7.2 设计原则

1. **`neon-cli` 是唯一 debug 入口。** Python SDK / Node SDK / Rust SDK / C SDK / C++ SDK 都不再自己实现 debug 子命令；它们要么直接调 RPC（保留编程能力），要么 spawn `neon-cli` 子进程拿 JSON 输出。
2. **CLI 自己当 service 路由器。** 调用方只需要知道一个 endpoint（或一个 manifest），CLI 根据 method 自动填正确的 `target` 字段，不再让用户手填 target、也不再硬编码。
3. **进程拓扑收敛到一套。** 保留 `neon3-runtime serve` 单进程模式给发布；开发自测统一用 `neon-cli dev up`，不再维护 `neon-dev` / `run-neon-services.ps1` / Python `cli.py dev up` 三套启动器。
4. **debug 面统一在 wgpu-runtime。** ui-runtime 的 `debug.snapshot.get` 要么补全成"返回当前 fragment + host 状态"，要么明确转发给 wgpu-runtime。消除双份实现。
5. **scenario 外部化。** 5 个硬编码 Rust scenario 迁成 YAML（放 `tests/scenarios/*.yaml`），CLI 内置 runner。30+ probe bin 里有用的收编为 scenario，没用的删。
6. **SDK 保持协议客户端。** SDK 仍然可以直接 RPC（这是给库用户的），但 SDK 自带的"CLI 工具"（`neon3-sdk` 命令）改为 thin wrapper，转发到 `neon-cli`。

---

## 8. 开放接口设计（CLI 子命令全集）

下面是实施 AI 要实现的 `neon-cli` 子命令树。所有命令默认输出**单行 JSON**（`--pretty` 美化），方便其他 AI 管道消费。

### 8.1 `neon-cli dev` — 启动与生命周期

```
neon-cli dev up [--profile editor|headless] [--project <path>] [--case <name>] [--once]
        启动 eventd / projectd / wgpu / ui / (editor / terrain / resource)
        输出 manifest JSON（含所有 endpoint / pid / epoch / capability）
        等价于现在的 neon-dev case + Python cli.py dev up

neon-cli dev status [--manifest <path>]
        查询所有服务健康

neon-cli dev logs --service <name> [--request <id>]
        拉结构化日志（替代现在 neon-dev logs 占位）

neon-cli dev restart --service <name>

neon-cli dev down [--manifest <path>]
        停掉本次 dev up 起的所有进程

neon-cli dev ps
        列出当前被本 CLI 管理的进程
```

### 8.2 `neon-cli debug` — 诊断面

#### 8.2.1 服务发现与健康

```
neon-cli debug services                       列出 manifest 里所有服务 + 健康
neon-cli debug describe <service>              service.describe
neon-cli debug health <service>               service.health
```

#### 8.2.2 状态快照

```
neon-cli debug snapshot [--service <name>]    debug.snapshot.get
        不带 --service 时，聚合调用所有已发现服务的 snapshot，合并成一个 JSON
        这是关键改进：现在 AI 要手动连两个 endpoint 各调一次

neon-cli debug ui fragment                    wgpu.ui.fragment.snapshot（当前 UI fragment）
neon-cli debug ui host                        debug.ui.host.snapshot（host/语义态）
neon-cli debug ui input                       debug.window.input.snapshot
neon-cli debug render diagnostics             wgpu.render.diagnostics
neon-cli debug render graph                   wgpu.render.graph.snapshot
neon-cli debug render inspect <resource-id>   wgpu.resource.inspect
```

#### 8.2.3 交互与 trace

```
neon-cli debug interaction list               debug.interaction.query
neon-cli debug interaction get <interaction-id>
neon-cli debug trace list [--service <n>] [--request <id>] [--from <seq>] [--limit N]
                                              debug.trace.query / debug.trace.subscribe
neon-cli debug command get <request-id>       debug.command.get
neon-cli debug journal query [--from <seq>] [--service <n>]
                                              debug.journal.query
neon-cli debug replay export <request-id>,... debug.replay.export
```

#### 8.2.4 截图与视觉验收

```
neon-cli debug capture <png> [--target ui.color.v1] [--redraw]
                                              wgpu.render.target.capture
neon-cli debug capture-surface <surface-id> <png>
                                              render.surface.capture_png
neon-cli debug assert <png> --expect <golden> [--threshold 0.01]
                                              wgpu.render.target.assert（新）
```

#### 8.2.5 输入注入（用于自动化）

```
neon-cli debug input hover <x> <y>            debug.window.input.hover_target
neon-cli debug input click <x> <y>            debug.window.input.activate_target（hit-test + 点击）
neon-cli debug input pointer <event-json>     ui.host.pointer_event
neon-cli debug input key <key>                ui.host.keyboard_event
```

#### 8.2.6 Editor 专用

```
neon-cli debug editor open <path>
neon-cli debug editor snapshot
neon-cli debug editor completion <prefix>
```

### 8.3 `neon-cli ui` — UI 操作

```
neon-cli ui submit <file.nui>                 ui.flow.submit
neon-cli ui compile <file.nui>                ui.flow.compile（dry-run，结构化 diagnostics）
neon-cli ui intent <intent> [--source <node-key>] [--payload-json '{...}']
                                              ui.host.inbound（语义 intent）
neon-cli ui frame <frame-json>                ui.input.frame
```

### 8.4 `neon-cli scenario` — 声明式回放

```
neon-cli scenario list
neon-cli scenario run <id> [--fixture <path>] [--out <json>]
        读 tests/scenarios/<id>.yaml，按 AGENTS.md 第 20 节格式执行，输出机器可读 JSON
neon-cli scenario new <id>                    生成模板
```

scenario YAML schema（沿用 AGENTS.md 第 20 节）：

```yaml
id: terrain.water.select-and-bind.v1
project_fixture: fixtures/terrain-water.neon
steps:
  - target: terrain-runtime
    method: terrain.tool.select
    params: { terrain_id: 12, tool: water_inject }
    expect:
      snapshot: { mode: water_paint, binding_state: needs_selection }
  - await:
      target: terrain-runtime
      snapshot: { binding_state: ready }
```

### 8.5 `neon-cli event` — 事件流

```
neon-cli event snapshot [--filter <name>]
neon-cli event subscribe <filter> [--timeout <secs>]
```

### 8.6 `neon-cli rpc` — 逃生舱

```
neon-cli rpc <method> [--service <name>] [--params-json '{...}'] [--idempotency-key ...]
        直接发任意 method。target 由 --service 指定；不传时 CLI 维护一张
        method → default target 路由表（从 service.describe capability 自动学）。
```

这是兜底：新 method 上线时不用等 CLI 封装，AI 可以直接 `rpc` 调用。

---

## 9. 关键实施任务清单（给实施 AI）

按优先级排序。每项都标注了涉及的文件。

### P0：修 target 路由 bug（必须先做）

- [ ] `crates/neon-cli/src/lib.rs:351` 把 `target: ServiceName("wgpu-runtime".into())` 改成从命令行参数或 manifest 读。
- [ ] 新增 `neon-cli rpc <method>` 子命令作为逃生舱。
- [ ] 让 CLI 维护一张 `method → default target` 路由表：

```rust
fn default_target(method: &str) -> &'static str {
    match method {
        m if m.starts_with("wgpu.") => "wgpu-runtime",
        m if m.starts_with("render.") => "wgpu-runtime",
        m if m.starts_with("debug.window.") => "wgpu-runtime",
        m if m.starts_with("debug.interaction.") => "wgpu-runtime",
        m if m.starts_with("ui.") => "ui-runtime",
        m if m.starts_with("debug.ui.") => "ui-runtime",
        m if m.starts_with("debug.trace.") | m.starts_with("debug.command.") | m.starts_with("debug.journal.") => "ui-runtime",
        m if m.starts_with("event.") => "eventd",
        m if m.starts_with("editor.") => "editor-runtime",
        m if m.starts_with("project.") | m.starts_with("asset.") | m.starts_with("transaction.") => "neon-projectd",
        m if m.starts_with("terrain.") => "neon-terrain-runtime",
        m if m.starts_with("resource.") => "neon-resource-runtime",
        _ => "ui-runtime",
    }
}
```

- [ ] 用 `service.describe` 的 capabilities 做运行时校验：method 打到不支持它的服务时报清晰错误，不 silent no-op。

### P1：合并启动器

- [ ] 把 `crates/neon-dev/src/main.rs` 的 `case <name>` 启动逻辑搬进 `crates/neon-cli`，子命令改名为 `neon-cli dev up --case <name>`。
- [ ] 保留 `neon-dev` crate 作为 thin wrapper（转发到 `neon-cli dev`）一个版本，对外不破坏。
- [ ] 把 Python SDK 的 `cli.py dev up` / `calculator` 改成 spawn `neon-cli dev up --gallery`。
- [ ] 删除 `scripts/run-neon-services.ps1` 和 `scripts/start-ui-case.cmd`。
- [ ] manifest JSON 落盘到 `<project>/.neon/manifest.json`，让后续命令不用每次都传 endpoint。

### P2：统一 debug 快照

- [ ] **ui-runtime 的 `debug.snapshot.get` 补全**（`crates/neon-ui-runtime/src/lib.rs:3255`）：当前只返回 revision/capabilities，要加上：
  - 当前已提交的 fragment 列表（surface_id / revision / sequence / program_revision）
  - host 状态（当前 input_revision / 未配对事件 / active interaction）
  - journal 最新 N 条
- [ ] `neon-cli debug snapshot` 不带 `--service` 时，按 manifest 并发调所有服务，合并成一个聚合 JSON：

```json
{
  "epoch": {...},
  "services": {
    "eventd": {...},
    "ui-runtime": {...},
    "wgpu-runtime": {...},
    "editor-runtime": {...}
  }
}
```

- [ ] wgpu-runtime 的 `debug.snapshot.get` 输出里加 `fragment_id` 字段，让 AI 不用再单独调 `wgpu.ui.fragment.snapshot`。

### P3：scenario 外部化

- [ ] 新建 `tests/scenarios/*.yaml`，把现在 5 个硬编码 scenario 迁过去：
  - `ui/static-fragment-submit.v1.yaml`（来自 neon-cli）
  - `ui/detail-toggle.v1.yaml`（来自 neon-cli）
  - `drag-card02-before.v1.yaml`（来自 neon-dev）
  - `component-gallery-interactions.v1.yaml`（来自 neon-dev）
  - `component-gallery-window-input.v1.yaml`（来自 neon-dev）
- [ ] 在 `crates/neon-cli` 里实现 YAML runner（不新建 `neon-testkit` crate，第一版直接放 CLI 里，避免过度工程）。
- [ ] runner 输出机器可读 JSON（沿用 AGENTS.md 第 20 节格式）。

### P4：收敛 probe bin

- [ ] 盘点 `crates/neon-ui-runtime/src/bin/*.rs` 和 `crates/neon-wgpu-runtime/src/bin/*.rs` 共 30+ 个 probe：
  - **被 scenario 覆盖的** → 删 bin，逻辑进 scenario YAML。
  - **一次性手测的** → 移到 `crates/neon-cli/src/probes/` 或删。
  - **通用工具性质的**（如 `nui_flow_diagnostics_probe`）→ 保留，但改成 `neon-cli ui compile` 子命令。
- [ ] `crates/neon3-runtime/src/bin/code_editor_authority_probe.rs` 迁成 `neon-cli scenario run editor-authority.v1`。
- [ ] `D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\bin\*.py` 三个 probe：保留 Python 侧 thin wrapper，实际调 `neon-cli scenario run`。

### P5：补 open 但未封装的 method

按第 4.6 节清单，给 CLI 加子命令：

- [ ] `debug.command.get` → `neon-cli debug command get <request-id>`
- [ ] `debug.journal.query` → `neon-cli debug journal query`
- [ ] `debug.trace.subscribe` → `neon-cli debug trace follow`（流式输出）
- [ ] `debug.window.input.hover_target` / `activate_target` → `neon-cli debug input ...`
- [ ] `wgpu.resource.inspect` / `preload` / `wait_ready` → `neon-cli debug resource ...`
- [ ] `wgpu.render.target.assert` → `neon-cli debug assert`
- [ ] `ui.flow.compile`（2026-09-18 新）→ `neon-cli ui compile <file>`，把 `RpcError.details.diagnostics[]` 漂亮打印
- [ ] `editor.*` 六个方法 → `neon-cli debug editor ...`

### P6：SDK 侧改造

- [ ] Python SDK 的 `cli.py`：删除 `NeonDevelopmentSession` 自己起进程的逻辑，改成 spawn `neon-cli dev up --json`，解析 manifest。
- [ ] Node SDK：加 `neon3-cli` bin（thin wrapper，spawn `neon-cli.exe`）。
- [ ] Rust SDK：加 `neon3-cli` bin（同上）。
- [ ] SDK 文档（`D:\Neon3Sdk\README.md`）更新：debug 流程统一指向 `neon-cli`。

### P7：清理

- [ ] 删除 `scripts/start-ui-case.cmd`（引用已不存在的 `packages/neon-ui-react-client`）。
- [ ] 删除 `plan/neon3-ui-react-client.md`（React 客户端已废弃）。
- [ ] 根目录 50+ 个临时 Python 脚本（`apply_changes_*.py`、`scope_*.py`、`heart*.py`、`shader_v*.py`、`recolor_*.py` 等）：迁到 `scripts/oneoff/` 或删。
- [ ] `tests/` 目录现在只有 fixtures，没有 `tests/*.rs`。迁完 scenario 后补 `tests/scenario_runner.rs`。

---

## 10. 与 AGENTS.md 的对照

| AGENTS.md 要求 | 本重构的对应动作 |
|---|---|
| 第 7 节：统一 envelope 按 target 分发 | P0 修 target 路由 bug |
| 第 12 节：CLI 是公开协议 client | P0-P5，CLI 覆盖所有 method |
| 第 18 节：debug.* 方法集合 | P5 补全 |
| 第 19 节：command journal | P5 加 `debug.journal.query` / `debug.command.get` |
| 第 20 节：声明式 YAML scenario | P3 |
| 第 21 节：分层验收 | scenario runner 输出 `acceptance_level` 字段 |
| 第 22 节：`wgpu.render.target.capture/assert` | P5 加 `neon-cli debug assert` |
| 第 23 节：`neon dev up/status/logs/restart` | P1 合并启动器 |
| 第 24 节：`neon-testkit` | P3 第一版直接放 CLI，不单独建 crate |
| 第 27 节：AI 工作日记 | 实施 AI 每天在 `docs/ai-diary/` 写日志 |

---

## 11. 验收方式（给实施 AI）

每完成一个 P 级别，必须跑：

```powershell
# 1. 启动
cargo run -p neon-cli -- dev up --case component-gallery --once
# 期望：输出 manifest JSON，所有服务 healthy

# 2. 聚合快照
cargo run -p neon-cli -- debug snapshot
# 期望：一个 JSON 包含所有服务状态，ui-runtime 部分有 fragment 列表

# 3. 跑一个 scenario
cargo run -p neon-cli -- scenario run component-gallery-interactions.v1
# 期望：JSON 输出 status=passed，trace_request_ids 非空

# 4. 截图
cargo run -p neon-cli -- debug capture target.png
# 期望：PNG 文件存在，非空

# 5. SDK 兼容
python -m neon3_sdk.cli dev up --gallery --once
# 期望：spawn neon-cli，正常工作

# 6. target 路由
cargo run -p neon-cli -- rpc ui.flow.compile --params-json '{"source":"version 1\nsurface x\n  text a value \"hi\""}'
# 期望：自动路由到 ui-runtime，返回 compile report
```

---

## 12. 已知风险与未决问题

1. **ui-runtime forwarder 不校验 target**：P0 修完 CLI 后，要不要让 forwarder 开始校验 target？建议**不要**——保持 forwarder 宽容，让 CLI 负责正确填 target。
2. **随机端口 vs 固定端口**：`neon-dev` 用随机端口，`neon3-runtime serve` 用固定 39101-39104。建议 `neon-cli dev up` 默认随机端口，`--fixed-ports` 切换到 39101-39104 兼容旧脚本。
3. **`neon-terrain-runtime` / `neon-resource-runtime` 还不存在**：路由表里已经给它们留了位置（`terrain.*` / `resource.*`），但现在调会报"service not found"。这是预期的。
4. **Python SDK 的 `NeonDevelopmentSession` 硬编码了 `--forward-server <ui> <wgpu> 127.0.0.1:39104`**（`cli.py:60`）——domain 指向一个不存在的 39104。P1 合并启动器时要改成读 manifest 里的 domain_endpoint。
5. **`neon3-runtime serve` 单进程模式**：它把四个服务塞进一个进程，和 `neon-cli dev up` 多进程模式是两套实现。短期保留（发布用），长期可以让 `neon-cli dev up --single-process` 复用同一段代码。
6. **scenario YAML schema**：AGENTS.md 第 20 节给了一个例子，但没有正式 schema。实施 P3 时要定一份（建议 JSON Schema 放 `tests/scenarios/schema.json`）。
7. **现有 `crates/neon-cli/src/lib.rs` 的 scenario 函数直接拿 `RpcClient` 对象**，不是子进程调用。迁成 YAML runner 时，runner 也要支持"同一进程内直接调"和"spawn 独立子进程"两种模式——CI 跑前者，开发调试跑后者。

---

## 13. 关键文件速查（给实施 AI 直接打开）

```
入口：
  D:\Neon3\crates\neon-cli\src\main.rs
  D:\Neon3\crates\neon-cli\src\lib.rs          ← target 硬编码 bug 在 351 行
  D:\Neon3\crates\neon-dev\src\main.rs         ← 启动器/scenario 搬迁源（2674 行）

服务端 handler：
  D:\Neon3\crates\neon-wgpu-runtime\src\lib.rs ← 16185 行，debug.snapshot 在 3366
  D:\Neon3\crates\neon-ui-runtime\src\lib.rs   ← forwarder 在 3560+，snapshot 在 3255
  D:\Neon3\crates\neon-eventd\src\lib.rs
  D:\Neon3\crates\neon-projectd\src\lib.rs

SDK 对照（看 SDK 期望什么）：
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\cli.py      ← 启动器
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\client.py   ← RPC envelope 规范
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\constants.py ← method 名常量
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\ui.py       ← debug.snapshot.get 用法
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\render.py   ← 截图/graph/diagnostics
  D:\Neon3Sdk\packages\python-sdk\src\neon3_sdk\input.py    ← pointer/keyboard
  D:\Neon3Sdk\packages\node-sdk\src\constants.ts           ← Node 侧 method 常量（同步 Python）
  D:\Neon3Sdk\scripts\sync-neon3-stack.ps1                  ← 发布流水线

文档：
  D:\Neon3\AGENTS.md                                       ← 架构宪法（必读）
  D:\Neon3\docs\ai-debug-workflow.md                        ← 现有 AI 工作流
  D:\Neon3\docs\neon3-ai-authoring-integration.md           ← nui_flow_diagnostics_probe 说明
  D:\Neon3\docs\nui-flow-diagnostics.md                     ← ui.flow.compile 新协议
  D:\Neon3\docs\ai-diary\2026-09-18-nui-flow-diagnostics.md  ← 最新一次架构改动
```

---

## 14. 不要做的事

- 不要新建 `neon-testkit` crate（P3 第一版直接放 CLI 里，避免过度工程）。
- 不要改 `neon3-runtime` 的发布模式（它是给打包用的，不是给开发调试用的）。
- 不要让 ui-runtime forwarder 开始严格校验 target（保持宽容，CLI 负责正确性）。
- 不要删 `crates/neon-wgpu-runtime/src/bin/*.rs` 里的 probe 之前，先确认它们没被 CI 引用（grep `sync-neon3-stack.ps1` 和 `.github/workflows/`）。
- 不要在没跑过 `neon-cli dev up --case component-gallery --once` 验证之前，就宣称 P1 完成。

---

## 15. 实施裁剪与当前优先级（2026-09-18 更新）

为了优先满足外部 SDK 查询页面实际运行状态，本轮实施顺序调整如下：

### 已优先实现

- P0 method -> target 路由与 `neon-cli rpc` 逃生舱。
- `debug snapshot [--manifest] [--service]` 聚合入口。
- 兼容旧 `eventd_endpoint` / `ui_endpoint` / `wgpu_endpoint` 等 manifest 字段，并支持 `services` 映射。
- 对每个服务保留 `health`、`describe`、snapshot method、request result 和 transport failure。
- 真实 loopback framed RPC JSONL probe。

### 暂缓，不作为 SDK 查询能力的前置条件

- `dev logs`、`dev restart`、`dev ps`：需要先确定 session supervisor 和日志持久化归属，当前不阻塞 SDK 查询。
- `debug trace follow` 长连接 CLI 包装：SDK 可先通过 `rpc debug.trace.query` 使用有限查询；持续订阅待事件/trace multiplex 方案稳定后再做。
- `debug assert <png>` golden image 比较：属于视觉回归验收，不属于页面状态查询；暂不阻塞协议查询面。
- `debug world-ui camera` 与历史 world-ui lab 专用命令：保留兼容入口，但不扩展为 SDK 默认能力。
- 删除 30+ probe bin 和迁移所有旧 `neon-dev` scenario：先等待 YAML runner 和 CI 引用盘点，避免破坏现有手测工具。

### 下一阶段

`neon-cli dev up` 仍需实现，但应优先产出稳定 `.neon/manifest.json`，让 SDK 可以完全脱离固定端口和手工 endpoint；完整 `neon-dev` 启动器迁移可以在此后分批进行。
- 不要把 Python SDK 改成必须依赖 Rust `neon-cli.exe`（SDK 要能在没装 Rust toolchain 的用户机器上跑；`neon-cli` 要随 SDK 一起发布为 prebuilt binary）。
