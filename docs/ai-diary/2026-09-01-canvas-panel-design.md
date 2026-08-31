---
date: 2026-09-01
topic: Canvas 类型组件面板的 NUI / WGPU 架构调研
type: research
status: completed
---

## 涉及的 crate / 文件路径

- `AGENTS.md`
- `docs/nui-flow-ai-authoring.md`
- `plan/neon3-nui-flow.md`
- `plan/neon3-ui-ir.md`
- `crates/neon-ui-schema/src/lib.rs`
- `crates/neon-ui-runtime/src/nui_flow.rs`
- `crates/neon-wgpu-runtime/src/ui_renderer.rs`
- `crates/neon-wgpu-runtime/src/world_ui_pipeline.rs`
- `crates/neon-wgpu-runtime/src/lib.rs`
- `crates/neon-world-bridge/src/lib.rs`

## 发现的问题

- 需求中的“canvas”可能指无限二维编辑画布、世界/场景视图或自定义图表画布；三者的内容数据和输入语义不同，不能只增加一个允许任意绘制代码的通用组件。
- NUI Flow V1 是声明式格式，明确禁止 shader、回调、坐标、GPU handle 与领域规则；让 Flow 承载 canvas draw list 或脚本会破坏既有边界。
- 当前 `render <key>` 已下沉为 `UiNodeKind::RenderSurface`，并生成稳定、renderer-owned 的 `RenderSurfaceRef { target_id: "render.<key>" }`。WGPU runtime 已能创建、写入、采样这类 renderer-private texture，因此不需要建立第二套跨进程 GPU 纹理协议。

## 采取的方案

- 建议第一阶段不新建 `canvas` 基础节点，而是将现有 NUI `render` / `RenderSurface` 作为 Canvas Panel 的内容槽位。
- 外层 `panel` 继续负责 NUI 布局、边框、裁剪、标题栏和普通控件；内部 `render` 只提供内容 viewport。
- 内容按领域拆成受限、版本化的 canvas scene / command schema，通过公开 RPC 由领域 runtime 提交给唯一 WGPU owner；WGPU runtime 持有 GPU buffer、texture、pipeline 与最终合成。
- 原始指针与局部 hover/pan/zoom/selection preview 留在 WGPU runtime；仅 begin/end/cancel/commit 和合并后的语义操作跨 IPC。

## 当前状态

已完成调研与架构建议；产品需求已进一步澄清为：持久化的 NUI 特有 `input canvas_data` 数据类型驱动二维点线 Canvas 绘制。未修改产品代码或协议。

## 未完成事项与下一步

- 先定义封闭的、可序列化的 `CanvasDataV1`，作为 NUI `canvas_data` 输入类型的持久化值；首版只覆盖 points 与 polylines。
- 明确 Canvas 节点是否可以嵌套普通 NUI 子节点，以及是否需要通过语义事件支持选择/编辑。首版可先限定为 display-only。
- 为其新增 schema、Flow parser/lowerer、WGPU pass、JSONL probe 与最终 composition capture scenario。

## 测试与验证结果

- 已实际读取并核对 Flow authoring、NUI Flow V1、UI IR、UI schema、WGPU render-surface 和 world bridge 实现。
- 已确认：`render` 被解析为 `UiNodeKind::RenderSurface`（`nui_flow.rs`），其 `target_id` 自动为 `render.<node-key>`；WGPU 侧 `ensure_render_surface` / `register_render_surface` 创建并采样 renderer-private target。
- 本次为设计调研，无代码行为变更，因此未启动服务、未运行 probe 或编译测试。
- 澄清后的设计仍遵循：数据可随 revisioned NUI input snapshot 持久化；仅 WGPU Runtime 把该数据转换为 GPU buffers/pipelines 与最终像素。

## 实现追加（Canvas V1）

### 采取的方案

- 已新增 `UiNodeKind::Canvas`、`UiCanvasData`、`UiCanvasPoint`、`UiCanvasLine`、`UiEffect::CanvasData` 与 `UiBoundProperty::CanvasData`。
- Flow 支持 `input guides canvas_data default canvas:empty` 和 `canvas guide_overlay data $guides`；`canvas_data` 走现有可靠、revisioned 的 `UiInputFrame`，不可声明 `emitevent`。
- V1 只支持有稳定 ID、有限局部逻辑坐标、RGBA、正半径/宽度的 points 与水平/垂直 lines。最多各 10,000 条；对角线显式拒绝，避免标准 panel primitive 被错误当成通用线渲染。
- WGPU 在 Canvas 节点 flatten 时原位展开 marks，保持 sibling/fragment painter order，并从 Canvas bounds 裁剪；未创建第二个窗口、GPU owner、共享纹理或旁路传输。
- 增加 `crates/neon-wgpu-runtime/src/bin/canvas_panel_probe.rs`，用公开 RPC 启动真实 headless WGPU runtime、提交固定 1 point/2 line Canvas、查询 diagnostics/capture，并输出 JSONL 与可靠退出码。

### 当前状态

Canvas V1 已完成：只读、数据驱动，适用于 UI 图分割器展示边框和间距 guide。验收等级为 `contract-ready`、`service-ready` 与 headless `composition-ready`；未启动交互窗口，未声称 `interactive-accepted`。

### 未完成事项与下一步

- 斜线、多段线、Bezier 需要独立 instanced line GPU pipeline 和 ABI layout test，不能直接放宽 V1 schema。
- 点选、框选和拖动需要独立的 revisioned domain semantic contract。
- 可编辑业务数据的唯一持久化写入仍应由领域 runtime / `neon-projectd` 完成；`canvas_data` 是其 NUI presentation 值。

### 测试与验证结果

- `cargo test -p neon-ui-schema canvas -- --nocapture`：2 passed。
- `cargo test -p neon-ui-runtime nui_flow::tests::canvas -- --nocapture`：2 passed；`nui_flow::tests::nine_slice`：2 passed。
- `cargo build -p neon-wgpu-runtime --bin neon-wgpu-runtime --bin canvas_panel_probe`：通过，存在既有 unused/dead-code warnings。
- `target\\debug\\canvas_panel_probe.exe`：通过。JSONL producer：`canvas_data_version=1`, `point_count=1`, `line_count=2`；consumer：`fragment_count=1`, `graph_revision=1`, `capture_target=ui.color.v1`；最终 `result=passed`。
- `cargo test -p neon-ui-schema -p neon-ui-runtime -p neon-wgpu-runtime`：未通过；schema（31）与 UI runtime（112）通过，WGPU suite 出现两个既有 world-panel / drag-drop 断言失败，随后进程 `0xc0000005`。已单独复现，不归因于 Canvas。
- `cargo fmt --check` 与 `git diff --check`：通过；Git 输出 CRLF conversion warnings，非失败。

## GPU Pipeline 修正（后续追加）

### 发现的问题

- 首次实现错误地将 Canvas 图元展开为普通 Panel rectangle。这不能正确表达斜线，也不能算 Canvas 的专用 GPU primitive。
- 首次 probe 只断言 headless RPC submission/target metadata，未证明真实 window composition 中有对应像素。

### 采取的方案

- 移除 `UiCanvasLine` 的水平/垂直限制。
- 新增专用 `CANVAS_SHADER`、`UiCanvasInstance`、Canvas vertex buffer 和 `neon3-ui-canvas-pipeline`。
- Canvas vertex shader 根据 `start/end/width` 计算方向与法线，生成任意方向线段 quad；point 通过 fragment SDF-style circle discard 形成圆形点。
- Canvas instances 在 WGPU final composition pass 内绘制，而不再转换成 `UiNodeKind::Panel` 或中间 render texture。
- probe 新增 `--window` 模式：启动 `neon-wgpu-runtime --window-server`、提交 NUI Canvas、调用 WGPU final target capture，读取 PNG 并统计红色 point / 青色 line 像素。

### 实际验证

- `target\\debug\\canvas_panel_probe.exe --window`：通过。WGPU 输出 `neon-wgpu surface present mode: Mailbox`，说明真实窗口 surface 启动。
- 实际 JSONL 最终记录：`mode=window`，capture artifact 为 `C:\\Users\\10540\\AppData\\Local\\Temp\\neon3-canvas-window-probe.png`，`red_pixels=72`，`cyan_pixels=1213`，`result=passed`。
- 固定输入包含一条斜线 `[24,16] -> [220,150]`，所以青色像素验证覆盖专用任意方向 line pipeline，不是 axis-aligned panel fallback。
- Commit：`71e9653 fix: render NUI canvas with GPU primitives`。

## NUI Flow 到真实窗口链路补齐

### 采取的方案

- `UiRuntime::handle_external_input_frame` 现在对包括 `canvas_data` 在内的外部输入，立即刷新 active fragment 并通过 `ui.fragment.submit` 转发到 WGPU；不再只更新 adapter 等待 motion 或下一次事件。
- `neon-ui-runtime` 新增 `canvas_window_probe`，实际启动 `neon-wgpu-runtime --window-server` 与 `neon-ui-runtime --forward-server`。
- 探针先调用 `ui.flow.submit` 发送真实 NUI Flow，再调用 `ui.input.frame` 提交持久化 `canvas_data`，读取 `debug.ui.host.snapshot`，最后请求窗口最终 composition capture 并解码 PNG 像素。

### 实际验证

- `cargo build -p neon-ui-runtime --bin neon-ui-runtime --bin canvas_window_probe`：通过。
- `target\\debug\\canvas_window_probe.exe`：通过。实际日志包含 `neon-ui-runtime received ui.flow.submit request=canvas-flow`，随后 `canvas.data` 的 `response_revision=2` 与 `canvas.snapshot.snapshot_contains_canvas_data=true`。
- 窗口最终 capture：`format=bgra8unorm-srgb`、`frame_sequence=1`、`composition_revision=2`、`red_pixels=208`、`cyan_pixels=1694`、`result=passed`。
- 这证明完整路径为：`NUI Flow -> UI Runtime -> canvas_data UiInputFrame -> UI fragment revision 2 -> WGPU window-server -> final composition PNG`。
- 新增 UI Runtime 依赖 `png = 0.18` 仅用于验收探针解码 renderer 生成的 PNG；不改变运行时传输协议。
