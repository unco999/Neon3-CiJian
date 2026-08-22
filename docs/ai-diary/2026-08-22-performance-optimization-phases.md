---
date: 2026-08-22
topic: 实现性能优化计划 Phase 1/3/4/5/10 — UiPerfCounters, 持久 ID 帧, debug.unified_id.inspect, world_ui_perf_probe
type: implementation
crates:
  - crates/neon-wgpu-runtime/src/lib.rs
  - crates/neon-wgpu-runtime/src/ui_renderer.rs
  - crates/neon-wgpu-runtime/src/bin/world_ui_perf_probe.rs
plan: plan/性能优化2026822.md
---

## 完成的工作

### Phase 1: UiPerfCounters + 60-frame ui_perf_window JSONL

- 新增 `UiPerfCounters` struct (lib.rs) 包含 18 个字段: render_frames, rendered_frames, dropped_frames, skipped_static_frames, skipped_throttled_frames, camera_frames_received, anchor_batches_received, pointer_down_received, pointer_up_received, semantic_clicks, unified_id_passes, unified_id_instances, unified_id_readbacks, transition_begins, transition_ends
- `HeadlessExternalGpu` 新增 `perf: UiPerfCounters` 字段
- `WgpuRuntime` 新增 `camera_frames_received`, `anchor_batches_received`, `pointer_down_received`, `pointer_up_received`, `semantic_clicks` 计数器字段
- 渲染循环中替换原有的 `eprintln!` 文本诊断，改为结构化 JSONL `{"event": "ui_perf_window", ...}` 每 60 帧输出
- 计数器在以下位置递增:
  - 渲染循环: render_frames, rendered_frames, dropped_frames, skipped_static_frames, skipped_throttled_frames
  - RPC 处理器: camera_frames_received, anchor_batches_received
  - pointer_event: pointer_down_received, pointer_up_received, semantic_clicks
  - render() 函数: unified_id_passes, unified_id_instances
  - read_pointer_hit_id: unified_id_readbacks (已移动到 read_completed_id_frame)

### Phase 3/4: 持久 unified ID 帧 + pointer 只读已完成帧

- 渲染循环的 `render()` 函数现在在绘制外部 ID ring buffer 后，额外将统一 ID pass 绘制到持久本地 `pointer_hit_target` 纹理
- 新增 `HeadlessExternalGpu` 字段: `id_frame_sequence`, `id_frame_bindings`, `id_frame_ready`
- 旧的 `read_pointer_hit_id` 函数被重写为 `read_completed_id_frame` — 不再清除/重绘整个 ID 图像，只从 `pointer_hit_target` 复制一个像素 + 回读
- pointer_event 中的绑定解析使用 `id_frame_bindings`（与同一帧配对的绑定映射），而不是热刷新的 `ui.hit_binding`（满足 §1.3 frame pairing）
- 新增 `hit_bindings_snapshot()` 方法到 `UiWgpuRenderer`

### Phase 5: debug.unified_id.inspect RPC

- 新增 `HeadlessExternalGpu::unified_id_inspect()` 方法，返回 `{frame_sequence, ready, binding_count, id_map: [{numeric_id, node_path, intent, interaction_key}]}`
- 在 headless 服务器的 RPC handler 中注册 `debug.unified_id.inspect` 方法
- 符合 §5.3 要求: 不返回 bounds/clip，不暴露 renderer 拓扑

### Phase 10: world_ui_perf_probe.rs 可执行探针

- 新增 `crates/neon-wgpu-runtime/src/bin/world_ui_perf_probe.rs`
- 实现 13 步场景:
  1. service.health
  2. service.describe
  3. 打开 ScreenUi surface
  4. 提交双面板 fragment
  5. 等待 unified ID frame 就绪
  6. 点击 m0 (panel p0)
  7. 释放 pointer
  8. 验证 debug.unified_id.inspect ID 映射
  9. 点击 m1 (panel p1)
  10. 释放 m1
  11. 三次快速点击 m0
  12. 验证最终 ID frame
  13. 输出 JSONL 摘要
- 每步输出 JSONL `{"step": "...", "pass": true/false, ...}`
- 退出码: 0 = 全部通过, 1 = 任意失败, 2 = 服务启动失败, 3 = 协议错误, 4 = 超时
- 通过 `cargo check -p neon-wgpu-runtime --bin world_ui_perf_probe` 编译

## 测试结果

- `cargo test -p neon-wgpu-runtime --lib`: 123 passed, 0 failed, 1 ignored (exit crash 0xc0000005 是预存的 DX12 测试清理问题，改前已存在)
- `cargo test -p neon-world-bridge --lib`: 6 passed, 0 failed
- `cargo check -p neon-wgpu-runtime --bin world_ui_perf_probe`: passed
- `cargo test -p neon-ui-runtime --lib`: 99 passed, 1 failed (预存失败，改前已存在)

## 未完成事项

- world_ui_perf_probe 尚未在真实运行的头戴服务器上测试（需要启动 neon-wgpu-runtime 的 headless server）
- Phase 8 (静态文本/拓扑/GPU buffer 缓存) — 未开始
- Phase 9 (删除无引用诊断代码) — 未开始，需先确认无生产路径引用
- Phase 2 (bevy-nui-host 的 latest-value coalescing) — 在外部仓库 D:\bevy-nui-host，未修改
- Phase 6 (显式 paint order) — 当前 ID 覆盖顺序已通过 unified_hit_image 测试
- 完整的 24 步 world UI 场景需外部 bevy-nui-host 提交 world fragment 和 anchor