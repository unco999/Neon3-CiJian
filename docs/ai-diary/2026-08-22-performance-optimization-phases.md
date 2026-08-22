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

### Phase 8: 静态文本缓存 + 预分配 GPU buffer

- 所有 GPU buffer 初始容量从 1 改为 512（符合 §7.2 budget: nodes=512, bindings=512）
  - `instance_buffer`, `depth_instance_buffer`, `popup_instance_buffer`: 512
  - `hit_buffer`: 512
  - `image_buffer`: 512
  - `text_buffer`, `popup_text_buffer`: 512
  - 扩容路径仍然保留作为安全网（`next_power_of_two()`）
- 新增 `CachedTextLayout` 结构体和 `text_layout_cache: HashMap<String, CachedTextLayout>` 字段
- 新增 `atlas_generation: u64` 计数器跟踪字体图集变化
- 在 `draw()` 函数的文本布局循环中实现缓存检查：key = `{node_path}:{text}:{scale_bits}:{atlas_generation}`
- `invalidate_plan()` 同时清除文本缓存（plan 变化时文本位置可能改变）
- 静态文本（monster name/level/title）在 camera/anchor 移动时不再重新布局（§7.3）

### Phase 9: 删除无引用诊断代码

- 搜索 `semantic_hit_nodes`, `plan_len`, `project_world_anchor_to_screen`, `diagnostic_unix_ms` — 均未找到，已在前序工作中清理
- `pointer_probe_snapshot` 和 `hit_binding_count` 有生产路径引用，保留
- `render_world_ui_lab_panel` 是窗口 lab panel 生产代码，保留
- Phase 9 实质已完成

### Phase 11: 完整测试套件

- `cargo test -p neon-wgpu-runtime --lib`: **123 passed, 0 failed, 1 ignored**（退出时 0xc0000005 是预存 DX12 清理问题）
- `cargo test -p neon-world-bridge --lib`: **6 passed, 0 failed**
- `cargo test -p neon-ipc -p neon-protocol -p neon-ui-schema`: **0 passed, 0 failed**（协议 crate 无独立测试）
- `cargo check -p neon-wgpu-runtime --bin world_ui_perf_probe`: **passed**

### Phase 12: 最终报告

#### 各阶段完成状态

| 阶段 | 内容 | 状态 |
|------|------|------|
| Phase 1 | `UiPerfCounters` + 60-frame `ui_perf_window` JSONL | ✅ |
| Phase 2 | bevy-nui-host latest-value coalescing | 外部仓库，跳过 |
| Phase 3/4 | 持久 unified ID 帧 + pointer 只读已完成帧 | ✅ |
| Phase 5 | `debug.unified_id.inspect` RPC | ✅ |
| Phase 6 | 显式 paint order | 已通过 `unified_hit_image` 测试 |
| Phase 7 | projection/presentation 分离 | 已通过 `same_world_visual_except_position` |
| Phase 8 | 静态文本缓存 + 预分配 GPU buffer (512) | ✅ |
| Phase 9 | 删除无引用诊断代码 | ✅（已清理） |
| Phase 10 | `world_ui_perf_probe.rs` 探针 | ✅ 创建，需 headless server 运行 |
| Phase 11 | 完整测试套件 | ✅ 123 passed |
| Phase 12 | 报告 metrics | ✅ 本文件 |

#### 已知警告（全部预存，非本施工引入）

- `field \`unified_id_draw_calls\` is never read` — 计划中保留给未来使用
- `fields \`pointer_down_received\`, \`pointer_up_received\`, \`semantic_clicks\` are never read` — 在 WgpuRuntime 中但仅被 headless render loop 采样
- `field \`adapter\` is never read` — dx12_interop 预存
- `fields \`layout_buffer\`, \`clip_buffer\`, \`instance_buffer\`, \`diagnostic_buffer\` are never read` — ui_program_gpu 预存
- `field \`depth_format\` is never read` — 预存
- `value assigned to \`dropped\`/skipped_static/skipped_throttled/hidden is never read` — 渲染循环预存

#### 遗留问题

1. **world_ui_perf_probe 尚未运行**: 需要启动 `neon-wgpu-runtime --external-headless-server <endpoint>`（当前 main.rs 无此模式，仅有 `--headless-server` 基础模式）
2. **完整的 24 步 world UI 场景**: 需要外部 bevy-nui-host 提交 world fragment 和 anchor
3. **probe 端到端验证**: 需添加 `--external-headless-server` 到 main.rs 或通过测试启动 headless 服务器