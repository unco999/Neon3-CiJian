# DeepSeek 施工任务书：UI-first 多面板并行动画与 WGSL 自动插值

> **施工对象**：Neon3 工作区与外部宿主 `D:\bevy-nui-host`
>
> **施工者**：DeepSeek implementation agent
>
> **优先级**：P0
>
> **目标**：修复 UI Runtime 等待宿主、多个 World UI 面板不能同时动画、CPU 每帧采样/上传动画数据、动画中断与回退跳变等问题。
>
> **本任务不修改 ID 图方案**：现有 persistent ID frame / readback / binding pairing 保持不变。ID 图只作为语义命中确认，不作为本任务的视觉动画驱动路径。

---

## 1. 必须遵守的两个硬约束

### 1.1 UI Runtime 必须先处理状态

原始错误链路是：

```text
Bevy pointer
  -> Bevy 等待/转发
  -> 宿主业务处理
  -> UI Runtime 才开始 state machine
  -> WGPU 才收到 presentation motion
```

这条链路禁止继续存在。

正确链路必须是：

```text
Bevy pointer
  -> UI Runtime ui.host.inbound
  -> UI Runtime 调用 WGPU 做 renderer hit resolution
  -> UI Runtime 立即 dispatch NUI state machine
  -> UI Runtime 立即提交 presentation motion 到 WGPU
  -> UI Runtime 返回 interaction accepted / semantic feedback
  -> 宿主业务异步接收 semantic event
```

要求：

1. UI Runtime 在开始 presentation state machine 和 motion 提交前，不得等待 Bevy 或 domain host 的 response。
2. Bevy/domain host 不得决定动画开始、动画目标、动画 duration 或回退方向。
3. host response 可以晚到、拒绝、超时或断线；这些情况只能触发 UI Runtime 的 reconciliation/rollback，不能阻塞第一帧视觉反馈。
4. semantic event 的最终业务转发仍要保留，但必须是 presentation 已经开始之后的异步或非阻塞步骤。
5. UI Runtime 仍是 UI presentation state machine 的唯一 owner。

### 1.2 动画必须由 WGSL 按时间自动插值

禁止以下实现：

```text
每帧 CPU sample_transition()
  -> 改写所有 UiInstance
  -> queue.write_buffer(instance_buffer, full_frame)
```

正确模型：

```text
transition begin / retarget / confirm / rollback
  -> CPU 计算一次 from/target/track metadata
  -> 只更新受影响的 GPU instance/track record

每个 render frame
  -> CPU 只更新一个全局 time uniform
  -> WGSL 根据 time_seconds、start_seconds、duration、easing
     自动计算 progress
  -> WGSL mix(from, target, progress)
```

这里的“WGSL 自动插值”不要求强行使用 compute shader。当前 UI 是 instanced render pipeline，vertex shader 在绘制每个 instance 时完成插值已经满足要求，且更容易保持 color/depth/text 的一致性。只有在确实需要 GPU-side compaction、indirect draw 或大量 track 状态更新时才增加 compute pass。

---

## 2. 当前真实问题与根因

### 2.1 当前进程/模块职责

```text
bevy-nui-host
  - ECS entity / camera / anchor / gameplay state
  - pointer input source
  - world latest-value producer

neon-ui-runtime
  - NUI Flow parser/compiler
  - UiHostAdapter
  - semantic event validation
  - NUI state machine
  - presentation motion declaration

neon-wgpu-runtime
  - sole GPU owner
  - external surface renderer
  - unified ID frame producer
  - current UiWgpuRenderer
  - final visual composition
```

### 2.2 当前 pointer/semantic 问题

当前相关路径位于：

- `D:\bevy-nui-host\src\lib.rs`
- `D:\Neon3\crates\neon-ui-runtime\src\lib.rs`
- `D:\Neon3\crates\neon-wgpu-runtime\src\lib.rs`

必须确认并修正：

1. Bevy pointer 应提交 `ui.host.inbound` 到 UI Runtime，而不是让 Bevy收到 WGPU `semantic_event` 后自行解释动画。
2. UI Runtime `forward_host_request()` 当前 pointer 分支会调用 WGPU，再 dispatch state machine；这部分必须保留，但 host forwarding 不能阻塞 presentation response。
3. WGPU 的 `LocalInputState`、`captured_binding`、`pending_control_value` 当前存在全局单槽位。必须改为按 `pointer_id` 保存 capture；不同 pointer/panel 不能互相覆盖。
4. UI Runtime 的 pending motion 必须是 batch/map，而不是 `Option<PendingStateMotion>`；同一批中多个 panel 的 transition 必须同时保留。

### 2.3 当前动画问题

当前 `UiWgpuRenderer` 已有：

```rust
active: HashMap<String, ActiveTransition>
```

这说明 renderer 逻辑上已经有 per-node active map，但当前流程仍存在这些问题：

1. `compose_sampled_visuals()` 会 CPU 调用 `sample_transition()`。
2. `draw()` 每帧重建 `instances`，并且原路径每帧写完整 `instance_buffer`。
3. UI Runtime 的 `pending_motion` 曾经是单值，后续即使改成 Vec，也必须按 panel scope 去重和合并。
4. parent transition 与 child visual 的传播未完成，导致子节点可能跳到最终位置。
5. 新点击、旧点击 response、host reject、epoch reset 的先后关系没有统一 generation 规则。
6. rollback 如果从旧 target 直接开始，会出现视觉跳变；必须从当前 GPU track 的解析值重新建 track。

---

## 3. 目标架构

### 3.1 UI-first 交互时序

```text
Bevy
  -> ui.host.inbound { kind: pointer_event }
       [Interaction lane, independent RPC connection]

UI Runtime
  1. validate pointer envelope
  2. call WGPU ui.host.pointer_event
  3. receive semantic_event or no-hit
  4. dispatch all matching NUI state machines
  5. create PresentationDeltaBatch
  6. send one UI fragment/presentation update to WGPU
  7. return accepted response immediately
  8. async forward semantic/domain event to host

WGPU Runtime
  - hit resolution remains renderer-owned
  - local presentation track starts before host response
  - GPU render pipeline samples tracks every frame
```

UI Runtime 不能因为 host 的 `ui.host.inbound` response 未返回而停止接受下一次 pointer。实现可以采用以下任一方式：

- 在 `UiRuntime` 内增加 host-forward queue 与 dedicated worker；
- 将 host forwarding 改为 fire-and-observe，不在 pointer request handler 中等待；
- 用现有 project IPC/RPC 类型建立受控的异步 response queue。

禁止发明新传输协议，禁止绕过 `RpcRequest`/`RpcResponse`。

### 3.2 多面板 presentation track

每个可动画 panel/node 必须有独立 track：

```rust
struct PresentationTrack {
    scope_key: String,
    generation: u64,
    from: GpuVisualState,
    target: GpuVisualState,
    start_seconds: f32,
    duration_seconds: f32,
    easing: GpuEasing,
    status: TrackStatus,
    source_request_id: String,
}

enum TrackStatus {
    Predicted,
    Confirmed,
    RollingBack,
    Cancelled,
}
```

`scope_key` 要求：

- 优先使用 stable panel/node key，不使用 numeric ID。
- 不使用临时 renderer path 作为跨进程业务 identity。
- World UI 应使用 anchor/panel stable key，例如 `monster.m14` 或声明的 panel key。
- 同一 scope 的新 transition 替换旧 transition。
- 不同 scope 的 transition 永远并存。

### 3.3 PresentationDeltaBatch

UI Runtime 对一次 semantic event 或一次输入批次产生：

```rust
struct PresentationDeltaBatch {
    batch_sequence: u64,
    program_revision: Revision,
    items: Vec<PresentationDelta>,
}

struct PresentationDelta {
    scope_key: String,
    generation: u64,
    from: PresentationVisual,
    target: PresentationVisual,
    transition: UiTransition,
    source_request_id: String,
}
```

要求：

1. 一次 batch 可以包含 1 个或多个 panel。
2. `items` 按 stable scope key 排序，保证 deterministic JSON/RPC 和测试。
3. 相同 scope 在 batch 中只能出现最后一个有效 delta。
4. 不同 scope 必须全部发送，不能用 `Option` 覆盖。
5. WGPU 接收 batch 后只更新受影响 track，不重新创建整棵 UI program。

---

## 4. 父子节点传播设计

### 4.1 当前扁平 instance 架构下的正确方案

当前 renderer 将 UI tree flatten 成 `PlannedNode` 和 `UiInstance`。因此 parent transform 的传播应采用：

```text
transition begin / retarget
  -> CPU flatten 当前 authoritative target tree 一次
  -> 计算每个 descendant 的绝对 target rect
  -> 计算每个 descendant 的 from rect
  -> 将 from/target 一次写入 GPU instance record
  -> WGSL 每帧插值
```

### 4.2 只平移的父节点

父节点：

```text
parent.from.x = 0
parent.target.x = 20
delta.x = 20
```

子节点 target 为 `24` 时：

```text
child.from.x = child.target.x - delta.x = 4
child.target.x = 24
```

GPU：

```wgsl
child_x = mix(child_from_x, child_target_x, progress);
```

### 4.3 多级父节点

不能只传播直接 parent。必须从 child 向 root 累加所有 active ancestor：

```text
child_from = child_target
for ancestor in parent_chain:
    child_from += ancestor_from - ancestor_target
```

如果同一 ancestor 有 size/layout transition，CPU 必须基于完整 old/new flattened tree 计算 child 的 from/target，而不是只使用 x/y delta。

### 4.4 layout/flex/reflow

当 parent 改变 width/height 导致 flex/column/row 子节点重新排布时：

1. CPU 在 transition begin/retarget 时计算 old layout tree 和 new layout tree。
2. 每个可绘制 instance 写入自己的 `from_rect` 与 `target_rect`。
3. 子节点不再依赖每帧 CPU 重新布局。
4. GPU 只对最终的 per-instance rect/color/opacity 做插值。

这不是每帧 CPU 动画；CPU 只在结构/target 变化时做一次布局与 descriptor 生成。

### 4.5 更高级的 GPU hierarchy 方案

本阶段不要求改成 GPU parent graph。若未来需要大量嵌套动画，可增加：

```text
GpuNodeRecord { local_rect, parent_index, track_index, ... }
GpuTrackRecord { from_transform, target_transform, start, duration, easing }
```

然后 WGSL 按 bounded parent depth 累加 transform。但这是后续优化，不能阻塞本阶段的扁平 descriptor 方案。

---

## 5. WGSL/GPU buffer 设计

### 5.1 UiView uniform

保持 16-byte 对齐：

```rust
#[repr(C)]
struct UiView {
    viewport: [f32; 2],
    color_mode: u32,
    time_seconds: f32,
}
```

`time_seconds` 使用 renderer 自己的 monotonic clock，不能使用 host wall clock。

### 5.2 UiInstance 扩展

至少包含：

```rust
#[repr(C)]
struct UiInstance {
    target_rect: [f32; 4],
    target_fill: [f32; 4],
    target_border: [f32; 4],
    target_params: [f32; 4],
    clip: [f32; 4],
    depth: f32,
    paint_group_id: u32,
    from_rect: [f32; 4],
    from_fill: [f32; 4],
    from_border: [f32; 4],
    from_params: [f32; 4],
    animation: [f32; 4],
}
```

`animation`：

```text
x = start_seconds
y = duration_seconds
z = easing code
w = enabled flag
```

必须用 `bytemuck::Pod + Zeroable` 和 layout test 固定 ABI。

### 5.3 WGSL progress

```wgsl
fn animation_progress(animation: vec4<f32>) -> f32 {
    if (animation.w == 0.0 || animation.y <= 0.0) {
        return 1.0;
    }
    let linear = clamp(
        (view.time_seconds - animation.x) / animation.y,
        0.0,
        1.0,
    );
    return ease(linear, animation.z);
}
```

color pass、depth pass、必要时 image/text owner transform 必须使用相同 progress。不能 color pass 插值而 depth pass 仍使用旧 rect。

### 5.4 CPU 上传规则

允许上传：

- transition begin
- transition retarget
- transition confirm/rollback
- fragment structure/layout revision 变化
- viewport/resize
- 一次性的 `UiView.time_seconds` 更新

禁止：

- 每个 frame 根据 progress 重写所有 `UiInstance`。
- 每个 frame 调用 CPU `sample_transition()` 后把结果写回 GPU。
- 一个 panel 动画导致所有 panel buffer 重新创建。

推荐维护：

```rust
uploaded_instances: Vec<UiInstance>
```

只有 descriptor 内容变化时调用 `queue.write_buffer(instance_buffer, ...)`。如果 GPU buffer 需要局部更新，按 changed range 合并后写入。

---

## 6. 中断、回退、确认与 stale response

### 6.1 同一 panel 新点击

```text
old track: from=A, target=B, start=t0
new event at t1

current_visual = analytic_sample(old track, t1)  // 只做一次
new track.from = current_visual
new track.target = C
new track.start = t1
```

不能使用旧 `from` 重新开始，也不能等待旧动画结束。

### 6.2 不同 panel 新点击

```text
panel A track 保留
panel B 新建 track
```

一个 fragment revision 可以包含多个 panel 的 transition descriptor。

### 6.3 host/domain 拒绝

1. WGPU/ UI Runtime 已经先启动 predicted presentation。
2. 收到 rejection 后检查 request generation。
3. 如果 response 仍对应当前 generation：
   - current GPU track 作为 rollback from；
   - authoritative presentation 作为 rollback target；
   - 写入新的 rollback track。
4. 如果 response generation 小于当前 generation：丢弃，不得覆盖新状态。

### 6.4 service epoch 变化

1. 所有旧 track 标记 cancelled 或 stale。
2. 清空 pending host response。
3. UI Runtime 从 snapshot 恢复 authoritative presentation。
4. WGPU 重新创建 tracks，不能继续使用旧 epoch 的 start/request。

### 6.5 动画完成

1. WGSL 进度达到 1 时输出 target。
2. CPU 在下一次受控 descriptor maintenance 阶段移除 completed track 或把 `from=target`、`animation.enabled=0` 固化。
3. 不得重新触发同一个仍存在于 fragment 的 enter transition。
4. 诊断输出一次 `transition_end`，不得每帧重复输出。

---

## 7. 必须修改的代码位置

### 7.1 UI Runtime

目标文件：

- `D:\Neon3\crates\neon-ui-runtime\src\lib.rs`
- `D:\Neon3\crates\neon-ui-runtime\src\nui_state_machine.rs`

施工点：

1. `UiRuntime::forward_host_request()`：pointer 先 renderer hit，再 state machine，再 presentation submit，再异步 host forward。
2. 将 `pending_motion` 替换为按 scope 聚合的 `pending_motions`/`PresentationDeltaBatch`。
3. 不同 state machine transition 全部收集，禁止 `transitions.into_iter().next()` 丢弃后续 panel。
4. 同 scope 新 transition 覆盖旧 scope，其他 scope 保留。
5. 增加 request/generation/revision 关联。
6. 保留 UI Runtime 作为唯一 presentation state owner。

### 7.2 WGPU Runtime

目标文件：

- `D:\Neon3\crates\neon-wgpu-runtime\src\ui_renderer.rs`
- `D:\Neon3\crates\neon-wgpu-runtime\src\lib.rs`

施工点：

1. `UiInstance` 增加 from/target/track metadata。
2. color/depth WGSL 同步读取 animation metadata。
3. `UiView` 增加 monotonic `time_seconds`。
4. `active: HashMap<String, ActiveTransition>` 保留并明确为 per-node/per-panel track registry。
5. retarget/rollback 时 CPU analytic sample 一次，普通 frame 不 sample、不 upload full instances。
6. parent chain 传播 from/target descriptor。
7. 完成时做一次 track cleanup。
8. `LocalInputState` / capture 改为按 `pointer_id` 管理，至少不能用全局单一 `captured_binding` 阻塞其他 pointer。
9. 不修改现有 ID frame pairing 协议；本任务不新增 external ID ring 依赖。

### 7.3 Bevy Host

目标文件：

- `D:\bevy-nui-host\src\lib.rs`
- `D:\bevy-nui-host\src\main.rs`

施工点：

1. pointer 走 UI Runtime `ui.host.inbound` interaction lane。
2. Bevy 不生成 `open_status`/`close_status` 动画意图。
3. Bevy 不维护 monster panel animation state。
4. world camera/anchor latest-value coalescing 与本任务独立保留。
5. UI Runtime 先处理 presentation，Bevy 只接收之后的 semantic/business result。

---

## 8. 测试与探针要求

### 8.1 UI Runtime 单元测试

新增或补充：

1. `four_panel_transitions_are_collected_in_one_batch`
   - 输入四个不同 panel semantic events。
   - 验证四个 scope 都存在。
   - 验证没有 `Option` 覆盖。
2. `same_panel_retarget_replaces_only_same_scope`
   - A1、B1、A2。
   - 最终保留 A2 + B1。
3. `presentation_starts_before_host_response`
   - mock host 延迟 500ms。
   - WGPU/mock renderer 必须在 host response 前收到 motion submit。
4. `stale_host_response_does_not_rollback_new_generation`
5. `rejection_rolls_back_from_current_visual`

### 8.2 WGPU renderer 单元测试

1. `gpu_track_descriptor_is_uploaded_once_for_static_animation`
2. `four_panel_tracks_are_independent`
3. `retarget_samples_old_track_once`
4. `rollback_uses_current_track_value`
5. `parent_child_from_target_offsets_are_propagated`
6. `nested_parent_chain_accumulates_offsets`
7. `color_and_depth_use_same_track_progress`
8. `completed_track_is_cleaned_without_restart`
9. `UiInstance` `size_of`/offset ABI test。

### 8.3 可执行 JSONL probe

新增项目原生 probe，例如：

```text
D:\Neon3\crates\neon-wgpu-runtime\src\bin\multi_panel_animation_probe.rs
```

必须：

- 启动或连接所需 local services。
- 使用现有 `RpcClient`、`RpcRequest`、`RpcResponse`。
- 固定 4 个 panel、固定 pointer sequence、固定 timeout。
- 输出 JSONL，每一行包含：

```json
{
  "scenario": "multi-panel-gpu-animation.v1",
  "step": "motion_batch_submitted",
  "input": {
    "panel_keys": ["monster.m0", "monster.m1", "monster.m2", "monster.m3"]
  },
  "sequence": 12,
  "tracks": [
    {
      "scope_key": "monster.m0",
      "generation": 1,
      "from": {},
      "target": {},
      "start_seconds": 1.25,
      "duration_seconds": 0.32,
      "status": "predicted"
    }
  ],
  "gpu_descriptor_uploads": 1,
  "full_instance_uploads": 0,
  "pass": true
}
```

probe 必须验证：

1. UI Runtime motion submit timestamp 小于 host response timestamp。
2. 四个 panel track 同时存在。
3. 不同 panel generation 不互相覆盖。
4. full instance upload 在动画 steady state 为 0。
5. rollback 后 from 是 rejection 时的当前 track value，而不是初始 from。
6. pointer lane 不等待 world latest lane。

退出码：

- `0` 全部通过
- `1` 行为失败
- `2` service 启动失败
- `3` schema/RPC 错误
- `4` timeout

---

## 9. 验收标准

### P0 必须全部通过

1. UI Runtime 在 host response 返回前已经开始 presentation motion。
2. 连续点击 4 个不同 panel，4 个 panel 同时进入 active track。
3. 第一个 panel 的动画不会阻塞第二、第三、第四个 panel。
4. 同一 panel 中途再次点击，动画从当前视觉位置平滑 retarget。
5. host reject 时平滑 rollback，不跳回初始位置。
6. stale response 不覆盖新 generation。
7. parent/child panel 在开始、中断、回退时保持同一进度和相对布局。
8. 普通动画 frame 不调用完整 `queue.write_buffer(instance_buffer, full_instances)`。
9. WGSL color/depth 使用同一个 `time_seconds` 和同一 track progress。
10. ID 图逻辑保持现有 frame pairing，不因本任务新增 external ID ring 阻塞。
11. Bevy 不再维护或生成 UI 动画状态/动画 intent。
12. `cargo test` host 通过。
13. WGPU renderer focused tests 通过。
14. UI Runtime focused tests 通过。
15. JSONL probe 退出码为 0，且输出实际 track/upload 数据。

### 允许保留的 warning

已有 dead-code、DX12 cleanup、asset TEXCOORD 等 warning 可以单独报告，但不能把 warning 当作行为验收通过。

### 不允许接受的结果

- 只编译通过但没有跑 probe。
- 只看到 `semantic_clicks` 增加就宣布动画完成。
- 只验证一个 panel。
- 用 Bevy ECS 状态模拟 UI 动画。
- 用 host response 的到达时间作为动画开始时间。
- 每帧 CPU 采样并上传所有 instance。
- 修改 ID 图协议来掩盖动画问题。

---

## 10. 施工顺序

1. 先创建/运行 UI-first delayed-host test，证明状态先于 host response。
2. 改 UI Runtime transition batch 和异步 host forward。
3. 增加四 panel batch unit test。
4. 改 WGPU `UiInstance` ABI、View time、WGSL color/depth pipeline。
5. 改 parent chain from/target propagation。
6. 增加 retarget/rollback/generation tests。
7. 改 pointer capture 为 per-pointer，避免输入单槽位。
8. 确认 Bevy 只发 `ui.host.inbound`，删除 host-side animation logic。
9. 启动服务并运行 JSONL probe。
10. 运行 focused tests，再运行相关完整 test suite。
11. 输出实际 JSONL artifact、request IDs、track generations、GPU upload counters 和 warnings。

---

## 11. 施工完成报告必须包含

DeepSeek 完工时必须报告：

```text
修改文件清单
UI-first 时序证据
四 panel 并行动画 JSONL
每帧 full instance upload 次数
transition descriptor upload 次数
rollback/generation 测试结果
parent/child propagation 测试结果
host cargo test 结果
UI Runtime focused/full test 结果
WGPU focused/full test 结果
warning 与 failure 分离说明
未完成项与残余风险
```

只有在四 panel probe 实际输出 `pass: true`，且 UI Runtime motion submit 明确早于 host response 后，才可以宣称本任务完成。
