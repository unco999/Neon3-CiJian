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
- `cargo run -p neon-dev -- scenario component-gallery-window-input`：通过；action button、slider、drag value、scrollbar、DataGrid tail interaction 均通过，composition revision 连续递增。
- `cargo test -p neon-ui-runtime --lib demo_domain::tests::component_gallery_headless_scenario_accepts_events_and_publishes_visible_status`：通过。
- toggle prediction / local input lifecycle focused tests：均通过。
- `git diff --check`：无 diff 错误。

## 未完成事项与下一步

- 修复并验证剩余 gallery、drop、WorldUi ID 失败测试。
- 补齐可执行 JSONL probe，并启动真实 WGPU/UI/host 服务验证。
- 完成 2x backing、WorldUi transform cache、border/depth、完整 gallery 回归。
- 运行相关 package tests 和 workspace tests。
