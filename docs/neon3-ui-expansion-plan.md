# Neon3 UI 横向扩展设计稿与施工计划

> 本文用于直接交给 DeepSeek 施工。必须严格按阶段执行，不允许跳过协议、测试和真实运行验证。

## 1. 总目标

在现有 ScreenUi、WorldUi、统一 ID 图、2x backing resolution、声明式 NUI Flow 和本地 frame-time 动画基础上，扩展通用动画样式、布局能力和组件系统。

必须保证：

- ScreenUi 与 WorldUi 使用同一套 semantic hit binding 规则。
- WorldUi 的位置由 camera + anchor 决定。
- WorldUi 距离变化只改变整个 subtree 的 uniform scale。
- 距离变化不能重新计算文字、padding、gap、子节点布局。
- pointer feedback 不等待 RPC 才开始。
- 所有 mutation 和 semantic event 使用正式 Neon3 IPC/protocol。
- renderer numeric ID 永远不进入业务协议。
- 不允许用固定坐标、固定点击区域、重复点击或 sleep 掩盖问题。

## 2. 进程职责

### `neon-ui-runtime`

- 解析并校验 NUI Flow。
- 编译 `UiProgram`。
- 管理 Flow state machine。
- 生成 `UiFragment` 和 `UiEffect`。
- 接收 renderer semantic event。
- 向 host 发布 typed semantic intent 和 input publication。

禁止：创建 window、wgpu、GPU buffer、最终像素。

### `neon-wgpu-runtime`

- 唯一 window/GPU owner。
- UI flatten/layout。
- ScreenUi/WorldUi color pass。
- WorldUi depth pass。
- unified `R32Uint` ID pass。
- pointer ID readback。
- frame-time transition sampling。
- 最终 composition。

禁止：推断领域规则、修改项目状态、使用 UI 局部状态代替 host 权威状态。

### Bevy/external host

- 拥有 ECS、Physics、Camera、World anchor。
- 提交 `CameraFrame`、`WorldUiAnchorBatch`、`UiInputFrame`。
- 接收 semantic intent 并转换为领域动作。
- 不创建 UI GPU 资源。
- 不根据屏幕坐标猜测业务目标。

## 3. 必须保持的协议原则

```text
numeric renderer ID 只存在 neon-wgpu-runtime
stable node path 用于诊断，不作为业务 ID
stable object ID / anchor ID 由 host 拥有
WorldUi anchor 只通过 WorldUiAnchorBatch 进入 renderer
semantic intent 必须经过 UiProgramSemanticEvent
camera/anchor 属于 latest-value 数据
click/commit/cancel 属于可靠数据
```

## 4. 统一交互模型

### 4.1 Unified ID frame

ScreenUi 和 WorldUi 必须使用同一张 unified ID 图：

```text
combined fragment snapshot
    -> World anchor projection
    -> stable flatten plan
    -> hit binding allocation
    -> one R32Uint ID pass
    -> one binding_by_id map
```

pointer down 只能读取已经完成的 ID frame，不得在 pointer down 中重新绘制整张 ID 图。

每个 ID frame 必须同时保存：

```rust
frame_sequence
producer_epoch
fragment_revision
world_frame_sequence
binding_by_id
```

numeric ID 与 binding map 必须来自同一次 `refresh_hit_bindings()`。

### 4.2 绘制顺序

必须固定 paint order，禁止依赖 HashMap iteration order：

1. ScreenUi 普通内容。
2. WorldUi far-to-near 内容。
3. modal/dialog/top layer。
4. 后绘制内容覆盖前绘制内容。

如果产品需要 WorldUi 覆盖 ScreenUi，必须同步修改 color pass 和 ID pass 顺序。

### 4.3 不冒泡

默认规则：

```text
一个 pixel -> 一个 numeric ID -> 一个 UiHitBinding -> 一个 semantic intent
```

父 panel 与 child 同时声明 semantic binding 时，child ID 覆盖父 ID，但不能产生两个事件。

## 5. WorldUi 投影模型

### 5.1 两种状态必须分离

```rust
struct WorldProjection {
    screen_x: f32,
    screen_y: f32,
    world_depth: Option<f32>,
    uniform_scale: f32,
    frame_sequence: u64,
}

struct PresentationVisual {
    width: f32,
    height: f32,
    background_color: [f32; 4],
    border_color: [f32; 4],
    border_width: f32,
    opacity: f32,
    numeric_value: Option<f32>,
}
```

### 5.2 投影规则

相机移动时只更新：

```text
screen_x
screen_y
world_depth
uniform_scale
```

禁止修改：

```text
child.bounds
padding
flex_basis
intrinsic text size
node topology
```

渲染流程必须是：

```text
logical layout once
    -> root projection position
    -> root uniform scale
    -> final visual transform
```

### 5.3 2x backing resolution

逻辑 UI 使用：

```text
1280 x 720
```

external color/depth/ID backing 使用：

```text
2560 x 1440
```

renderer 必须明确区分：

```rust
physical_viewport = [2560, 1440]
logical_viewport = [1280.0, 720.0]
```

pointer pixel 必须转换到 physical backing 坐标后读取 R32Uint。

## 6. 动画系统

### 6.1 支持的动画类型

第一阶段支持：

```text
fade
scale
slide
background_color
border_color
border_width
corner_radius
opacity
numeric
```

第二阶段支持：

```text
spring
pulse
shake
stagger
```

### 6.2 动画不允许改变布局拓扑

动画只能改变 presentation，不能插入/删除节点。

WorldUi projection 不属于 presentation animation。

### 6.3 latest-wins

新 transition 到达时：

1. 读取当前 sampled presentation。
2. 取消旧 transition。
3. `revision += 1`。
4. 新 transition.from 使用当前 sampled 状态。
5. 新 transition.target 使用新目标状态。
6. 立即使用本地 `frame_time` 开始采样。

旧 RPC response 如果 revision 小于当前 revision，必须丢弃，不得回滚视觉状态。

### 6.4 生命周期日志

只在生命周期边界输出 JSONL：

```json
{"event":"world_ui_transition_begin","node_path":"surface.combined-ui/p11","revision":4,"motion_key":"status-hit","start_monotonic_ns":1000,"duration_ms":500}
{"event":"world_ui_transition_end","node_path":"surface.combined-ui/p11","revision":4,"end_monotonic_ns":1500,"result":"completed"}
{"event":"world_ui_transition_cancelled","node_path":"surface.combined-ui/p11","old_revision":4,"new_revision":5,"reason":"latest_wins"}
```

禁止每帧打印 progress。

## 7. 布局系统设计

### 7.1 容器默认行为

```text
branch: transparent column structural container
panel: default absolute unless explicit row/column/overlay
modal/dialog: top layer and pointer blocking
world panel: stable logical subtree, root transform only
```

### 7.2 Auto-size

```text
h=0: automatic height
w=0: automatic width
```

Column：

```text
height = padding_top + padding_bottom + sum(child heights) + sum(gaps)
width = max(child widths) + padding_left + padding_right
```

Row：

```text
width = padding_left + padding_right + sum(child widths) + sum(gaps)
height = max(child heights) + padding_top + padding_bottom
```

显式 `w/h` 作为最小声明尺寸；内容超过时必须扩展，不能直接裁剪。`min_size/max_size` 作为约束。

### 7.3 文本测量

文本测量必须与实际 glyph layout 使用同一套 advance 规则：

```text
逐字符累计 glyph advance
遇到显式换行 -> 新行
超过可用宽度 -> 新行
height = line_count * line_height
```

禁止使用“总宽度 / 可用宽度”的粗略估算作为唯一高度来源。

字体大小不能由 `bounds.height` 反向决定，否则会形成：

```text
height -> font scale -> wrapping -> height
```

WorldUi 字体只使用 root uniform scale。

### 7.4 Clip

默认 Label 不应因为父 panel 固定高度而裁剪最后一行。只有以下情况允许 clip：

```text
clip bounds
clip rounded
clip scroll
explicit overflow policy
```

所有 text、border、progress、chrome 必须使用同一经过 transform 的 clip。

## 8. GPU 批处理

### 8.1 ID pass

必须满足：

```text
one R32Uint render pass
one queue.write_buffer
one vertex buffer
one draw call
```

禁止每个 panel 一个 render pass。

### 8.2 WorldUi color/depth/border 一致性

panel 本体、border、slider chrome、progress chrome、text paint group 必须共享：

```text
world_depth
paint_group_id
uniform scale
clip transform
```

Depth pass 必须使用与 color pass 相同的 panel instances，不能让 border 使用 `depth=0` 或 `paint_group_id=0`。

## 9. Host 数据流优化

### 9.1 Camera/anchor latest-value

camera frame 和 anchor batch 只能各自最多一个 in-flight。

如果新值到达：

```text
覆盖 pending latest
不排队旧请求
response 到达后只发送最新 pending
```

### 9.2 Pointer lane

pointer/click 必须使用独立 interaction lane，不能等待 camera/anchor FIFO。

## 10. 推荐施工顺序

### Phase 1：契约与测试

- 增加 `UiWorldProjection`/presentation 单元测试。
- 增加 ID numeric -> binding 映射测试。
- 增加 WorldUi border occlusion GPU 测试。
- 增加 text auto-size 测试。

### Phase 2：布局

- 完成 branch 默认 Column。
- 完成 auto-size Row/Column。
- 完成 glyph advance wrapping。
- 删除 bounds 驱动字体缩放。

### Phase 3：WorldUi transform

- layout 只计算 logical subtree。
- root 设置 `world_scale`。
- flatten 最终阶段只变换 visual bounds/clip/text。
- 禁止 filter 阶段修改 child padding/gap/bounds。

### Phase 4：2x backing

- external color/depth/ID backing 改为 2x。
- logical viewport 维持 1280x720。
- pointer readback 使用 physical pixel。
- 增加 resize 测试。

### Phase 5：动画

- presentation transition 与 projection 分离。
- latest-wins revision。
- transition begin/end/cancel JSONL。

### Phase 6：组件

新增默认组件：

```text
Button
Slider
ProgressBar
Checkbox
RadioButton
Tabs
Dropdown
TextInput
```

所有组件必须有：

```text
稳定默认尺寸
intrinsic/content size
keyboard/pointer semantic behavior
disabled/hover/pressed state
ID hit coverage
```

## 11. Physics WorldUi 建议

Physics 标签只保留：

```text
object name
object id
height progress/value
compact border/header
```

不要在每个物体标签里塞长介绍文本。

高度通过：

```text
Transform.translation.y
-> UiInputChange height{id}
-> progress_bar $height{id}
```

踢击通过：

```text
phys.kick{id}
-> UiProgramSemanticEvent
-> Bevy SemanticIntentEvent
-> PhysicsObject lookup
-> Velocity mutation
```

## 12. 验收场景

必须新增可执行 probe：

```text
neon-wgpu-runtime
neon-ui-runtime
bevy-nui-host / physics_playground
```

场景至少包含：

```text
2 个 WorldUi panel
2 个 anchor
2 个不同位置
分别点击并验证 node path
验证 numeric ID 不为 RENDER_HIT_NONE
验证 border 与 body 同时被遮挡
验证 camera 拉近/拉远只改变 uniform scale
验证 text layout revision 不因 camera distance 改变
验证 2x backing resolution
验证 physics kick 后 height value 变化
```

JSONL 最少字段：

```json
{
  "scenario":"world-ui-expansion.v1",
  "monster_id":"phys.obj.12",
  "anchor_id":"phys.obj.12",
  "surface_id":"case.bevy.world.ui",
  "logical_pointer":[640,360],
  "physical_pointer":[1280,720],
  "id_frame_sequence":42,
  "id_numeric":24,
  "node_path":"surface.physics/phys-tag-12",
  "world_scale":1.8,
  "layout_revision":7,
  "text_layout_revision":3,
  "depth_group":12,
  "pass":true
}
```

## 13. 性能验收阈值

在 1280x720 logical、2560x1440 backing、50 个 WorldUi panel 下：

```text
steady render avg <= 1ms
steady render p95 <= 2ms
dropped frames during idle = 0
dropped frames during camera drag = 0
camera/anchor stale backlog <= 1
pointer down -> ID result p95 <= 5ms
```

必须报告：

```text
snapshot_ms
layout_ms
text_layout_ms
color_pass_ms
depth_pass_ms
id_pass_ms
lock_wait_ms
dropped_frames
```

## 14. 禁止事项

```text
禁止固定点击区域
禁止 CPU bounds 代替 GPU ID
禁止按 camera distance 改 padding/gap/child bounds
禁止每帧重新 parse/compile Flow
禁止每帧打印所有 node
禁止使用 sleep 等待渲染完成
禁止旧 revision response 覆盖新动画
禁止让业务进程持有 renderer numeric ID
```

## 15. 最终交付清单

DeepSeek 完成后必须提供：

1. 修改文件列表。
2. 协议/布局/renderer 分层说明。
3. unified ID frame 证据。
4. WorldUi border depth 测试结果。
5. 2x backing resolution 测试结果。
6. camera distance 不触发 text relayout 的测试结果。
7. Physics kick/height UI 的 JSONL 输出。
8. `cargo check` 实际结果。
9. focused tests 实际结果。
10. release runtime 实际 frame/drop 指标。
11. warnings 和 failures 分开列出。
