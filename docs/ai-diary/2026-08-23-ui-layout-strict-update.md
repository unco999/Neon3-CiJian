---
date: 2026-08-23
topic: Neon3 UI strict layout update
type: implementation
---

## 涉及的 crate / 文件路径

- `crates/neon-wgpu-runtime/src/ui_renderer.rs`
- `docs/neon3-ui-layout-strict-update.md`

## 发现的问题

- `UiVisual` 原先没有保留逻辑排版几何，文本缓存使用投影后的视觉几何。
- `intrinsic_size()` 与实际 glyph layout 使用了两套换行循环。
- 显式尺寸在 `resolved_dimension()` 中会优先于 intrinsic 尺寸，可能造成内容被固定高度裁剪。
- 基线测试已有 5 个失败：component gallery、drop materialization、parent transition、text clip。

## 采取的方案

- 增加内部 `LogicalLayoutBox` 与 `FinalVisualTransform` 数据模型。
- `UiVisual` 增加 `logical_bounds`，WorldUi 投影后的 `bounds` 与逻辑几何分离。
- 增加 `break_text_lines()` / `measure_text_lines()`，统一 intrinsic 和实际文本换行规则。
- 文本缓存 key 改用逻辑几何，文本安全 inset 统一。
- `resolved_dimension()` 改为显式尺寸与 intrinsic 尺寸取最大值。
- 增加文本测量和容器布局 focused tests。
- 修复显式 fragment root clip 丢失：root 使用 viewport 做布局时保留 authored root clip。
- 修复父节点 GPU transition 的子节点起始位置被最终 clip 裁掉的问题。
- 修复滚动文本缓存滞留：缓存命中时按当前 sampled visual origin 平移 glyph rect，并刷新当前 text clip；缓存仍按 logical layout 复用。
- 修复 scroll panel 双重滚动：`resolve_children()` 不再在逻辑排版阶段扣除 scroll offset；显式 scroll viewport 不再被 intrinsic content 撑大，滚动统一在 composition 阶段应用一次。
- 将滚动子树文字从最终实例缓存中剔除：scroll ancestor 下每帧根据当前 sampled bounds/clip 重建 text instances；静态文字继续复用 logical layout cache。
- 开始交互索引重构：增加 `plan_index`（node path -> plan index）和 `hit_id_by_node`（node path -> hit id），focus 改为按当前 plan topmost 命中，不再遍历 HashMap；窗口坐标的 GPU readback 转换为 physical pixel。
- 修复 feature-toggle 点击需要多次的问题：UI Runtime 对同 fragment identity 的 renderer semantic event 自动 rebase 到当前 active fragment revision，保留 program/input/epoch/sequence/idempotency 校验，避免 host publication 期间的 `fragment_revision_stale` 丢点击。
- 完成交互 P0 修复：GPU ID readback 不得覆盖已 capture 的 hover/capture；窗口和外部 pointer Down 统一使用当前 composed CPU hit，不再用异步 GPU ID 作为生产点击权威。
- 修复窗口点击双击根因：`serve_forwarder` 不再把 async host forward 只放入队列等待下一次 RPC；当前 service tick 立即启动有序 host worker，确保一次点击完成 domain publication 和 fragment submit。
- Checkbox/Radio/Selectable 增加即时 toggle prediction，domain 接受后 authoritative fragment 接管，失败时回滚。
- 快速施工验收：重新编译 `neon-ui-runtime --bins`、`neon-wgpu-runtime --bins`、`neon-dev` 后，真实 `component-gallery-window-input` scenario 通过，action button、drag/drop、slider、drag value、scrollbar、DataGrid tail interaction 全部通过。
- 补齐 `neon-ui-runtime` 测试构造中的 `UiNode.world_scale` 字段，使 package 可以编译。

## 当前状态

进行中，未完成。不能宣称整份 strict layout 计划已完成。

## 测试与验证结果

- `cargo check -p neon-wgpu-runtime --tests`：通过。
- `cargo test -p neon-wgpu-runtime --lib measure_`：8 passed，0 failed。
- `cargo test -p neon-wgpu-runtime --lib`：136 passed，5 failed，1 ignored。
- 后续 focused 验证：text clip 1 passed；parent transition 1 passed；文本测量 8 passed；Column padding/gap 1 passed。
- 滚动 focused tests：5 passed；文本 focused tests：15 passed。
- 新增 `scroll_composition_moves_whole_panel_without_collapsing_child_tracks`：通过。
- text/scroll 架构修复后：scroll focused 5 passed，text focused 15 passed，完整 WGPU renderer 138 passed，4 failed，1 ignored。
- 完整测试在 scroll 修复后：138 passed，4 failed，1 ignored。
- 完整 `neon-wgpu-runtime`：137 passed，4 failed，1 ignored。
- `cargo check -p neon-ui-runtime --tests`：通过。
- `cargo test -p neon-ui-runtime`：99 passed，6 failed；失败集中在既有 demo/host route 场景。
- 显式重编译实际子进程 binaries 后运行 `cargo run -p neon-dev -- scenario component-gallery-interactions`：通过，9/9 controls accepted。
- 交互命中 focused tests：component controls 1 passed；unified overlapping hit 1 passed；topmost focus index 1 passed。
- 新增施工文档 `docs/neon3-ui-component-style-animation-work-plan.md`，覆盖组件缺失、样式缺失、动画缺失、window/headless 统一交互、O(1) 索引、revision queue、scenario/probe 和完工报告模板。
- 开始按施工文档 M1 落地 `UiComponentMetrics`、`UiComponentCapabilities`、`UiComponentSpec`，统一 renderer 组件默认 metrics/capability 入口，并新增全 `UiNodeKind` coverage test。
- 继续推进 M3：新增 `UiStateFlags`、`UiStylePatch`、`resolve_component_style()`，body 样式开始统一处理 hover/pressed/focus/disabled/selected/checked/open 状态。
- component chrome 已改用统一 style resolver，新增 disabled 优先级、selected/focus 不改 layout metrics 的回归测试；真实 window scenario 仍通过。
- 开始 M6：新增 `UiAnimationStatus`、`UiAnimationSpec`、`UiAnimationInstance`，现有 `ActiveTransition` 已能报告 running/completed lifecycle；新增 animation lifecycle focused test，真实 window scenario 通过。
- M6 继续：确认 `ActiveTransition` 必须继续作为唯一运行时动画存储，避免为 diagnostics 再复制一套执行器；animation lifecycle focused test 和 window scenario 保持通过。
- M6 补充：animation history 现在记录 completed/cancelled，新增 cancel lifecycle focused test；真实 window scenario 仍通过。
- M6 retarget：保留纯 `sample()` 供既有测试复用，新增实例包装记录 superseded history；新 target 从旧 transition 当前 sampled visual 开始，retarget focused test 与 window scenario 均通过。
- 开始性能验收：新增 `UiLayoutCounters`（layout/text/world transform），在 layout refresh、text layout build、WorldUi sampled transform 路径计数，并提供 diagnostics 输出。
- 新增 `world_transform_update_does_not_relayout_static_text` 性能回归测试：WorldUi visual transform 不增加 layout/text layout count，测试通过。
- layout counters 已接入 headless/window diagnostics；WorldUi transform、动画 lifecycle focused tests 和真实 window scenario 均通过。
- 新增 `neon-dev probe-window-metrics <endpoint> [samples] [interval-ms]`，输出 bounded JSONL metrics sample/summary；`cargo check -p neon-dev` 通过。
- window debug snapshot 已暴露 `layout_counters.window_ui`，probe 可读取 layout/text/world-transform 计数；性能 focused test 通过。
- 统计并写入施工文档当前真实 Style/Animation 能力矩阵：明确 `UiStyle` 五个字段已生效，`UiStylePatch` 状态字段部分仅 contract，当前动画实际支持 bounds/size、opacity、背景/边框颜色、border width、radius、numeric value；font/shadow/text color 尚未接入最终像素。
- 新增 schema contract：`UiVisualState`、`UiStylePatch`、`UiStyleStateSet`、`UiAnimationProperty`，并通过 serde round-trip 测试；明确 text_color 当前仍未接入 renderer final text pixels。
- 开始实现 CSS-like style：NUI Flow 新增/接通 `opacity`、`radius`、`border_width`，`fill`/`line` 已有并补测试；字体 atlas sampler 改为 Nearest，保持 coverage threshold 不变以增强边缘硬度而避免 CJK 丢笔画。
- CSS-like parser、schema style contract、字体 glyph pixel focused tests 均通过；`text_color/ink` 仍未接入最终 `UiTextInstance.color`，明确保留为下一步。
- 在施工文档中补充硬性约束：窗口版和无窗口版禁止分叉组件/layout/style/animation/hit/revision 实现，必须通过同一共享核心和 headless/window parity scenario 验收。
- 根据复核补充组件成熟度基线 L0-L6，明确已有基础组件不重写，施工目标是从现有静态/部分交互实现推进到完整 contract、状态、动画和 parity 验收。
- `cargo run -p neon-dev -- scenario component-gallery-window-input`：通过；action button、slider、drag value、scrollbar、DataGrid tail interaction 均通过，composition revision 连续递增。
- `cargo test -p neon-ui-runtime --lib demo_domain::tests::component_gallery_headless_scenario_accepts_events_and_publishes_visible_status`：通过。
- toggle prediction / local input lifecycle focused tests：均通过。
- `git diff --check`：无 diff 错误。

## 未完成事项与下一步

- 修复并验证剩余 gallery、drop、WorldUi ID 失败测试。
- 补齐可执行 JSONL probe，并启动真实 WGPU/UI/host 服务验证。
- 完成 2x backing、WorldUi transform cache、border/depth、完整 gallery 回归。
- 运行相关 package tests 和 workspace tests。
