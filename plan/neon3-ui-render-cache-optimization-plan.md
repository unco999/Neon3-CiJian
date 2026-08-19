---
title: Neon3 UI 每帧渲染与绑定缓存优化施工计划
tags:
  - neon3
  - rendering
  - wgpu
  - performance
status: draft
---

# Neon3 UI 每帧渲染与绑定缓存优化施工计划

> 面向低级实现模型的施工文档。
>
> 目标：在不破坏 Panel/Text 深度排序、Bevy 外部合成、ID 图和三缓冲协议的前提下，减少
> 每帧 layout、文字排版、instance buffer 上传、draw command 和资源绑定开销。

## 0. 最重要的结论

不要把所有 UI 当成每帧动态内容处理。必须拆成三个层次：

```text
层 1：UI 内容缓存
  NUI flow、节点树、布局、文字 glyph、图片 UV、Panel/Text group
  低频更新，只在内容 dirty 时重建

层 2：World presentation
  camera/anchor 投影后的 screen rect、scale、depth、visible
  高频更新，但只修改 instance 数据

层 3：GPU 提交
  Pipeline、BindGroup、RenderBundle、buffer capacity
  尽量长期复用
```

最终目标：

```text
静态 UI：不重新 draw
World UI 移动：只更新 transform/depth buffer
UI 内容改变：只重建受影响 group
Pipeline/BindGroup：不因数据变化而重建
```

## 1. 当前问题

当前 external World UI render loop 大致是：

```text
每 16ms
  -> 获取 fragments snapshot
  -> invalidate_plan()
  -> refresh_plan()
  -> flatten/layout
  -> compose_sampled_visuals()
  -> 重新生成 Panel/Text/Image/Surface 数据
  -> queue.write_buffer()
  -> color pass
  -> color depth pass
  -> external R32Float depth pass
  -> ID pass
```

主要浪费：

| 开销 | 当前问题 | 优化方向 |
| --- | --- | --- |
| Flow/layout | camera/anchor 变化可能触发全量重新布局 | 内容布局与 World presentation 分离 |
| 文字 | 每帧重新生成 glyph instance，可能重复字体测量 | 缓存 glyph local rect 与 UV |
| Panel | 每帧重建全部 instance | 缓存 local instance，只更新 transform/depth |
| 分组 | 每帧重新创建 group vector、排序、range | 缓存 group topology 与 draw range |
| Buffer | 每个 group 反复 `queue.write_buffer` | 合并为大 buffer，一帧少量连续上传 |
| BindGroup | 绑定关系已有缓存，但没有明确生命周期 | 明确创建时机，禁止每帧重建 |
| Pipeline | 已缓存，但 render path 可能反复切换 | 固定 pipeline 顺序，必要时使用 RenderBundle |
| Render target | 静态内容也不断重绘 | target dirty / snapshot dirty 缓存 |
| 三缓冲 | producer 60Hz、consumer 较慢时频繁 drop | 静态时不生产新帧，动态时保留最新 dirty 状态 |

## 2. 不可破坏的深度契约

施工期间必须保留以下约定。

### 2.1 两张 depth 的职责不同

```text
Depth32Float color depth attachment
  neon-wgpu-runtime 内部 color pass 使用
  解决 Panel/Text/Image 之间的 UI 遮挡

R32Float external ui_depth target
  导出给 Bevy
  只解决 Neon UI 与 Bevy scene depth 的遮挡
```

不能把 `R32Float` 传给 `DepthStencilState`。

### 2.2 深度范围

```text
0.0 = 近
1.0 = 远
```

内部 color depth 使用：

```text
Depth32Float
depth_compare = LessEqual
depth_write_enabled = true
```

Bevy 外部 shader 使用：

```text
scene_d < ui_depth -> Bevy 场景更近，discard UI
```

### 2.3 Panel/Text 必须共享 presentation context

同一个 World UI group 的 Panel、Text、Image、Surface 必须共享：

```text
paint_group_id
base_depth
world transform
visible
clip context
```

允许不同类型有不同的微小 layer bias，但不能各自独立计算 World depth。

建议：

```text
Panel: base_depth + 2 * epsilon
Image: base_depth + 2 * epsilon
Text:  base_depth + 1 * epsilon
```

因为数值越小越近，所以 Text 可以显示在自己的 Panel 上方；近 group 仍会遮挡远 group。

## 3. 目标数据模型

### 3.1 内容缓存

新增 renderer-local 结构：

```rust
struct UiContentCache {
    key: UiContentCacheKey,
    groups: Vec<UiCachedGroup>,
    panels: Vec<UiCachedPanel>,
    texts: Vec<UiCachedText>,
    images: Vec<UiCachedImage>,
    surfaces: Vec<UiCachedSurface>,
    hit_bindings: Vec<UiHitBinding>,
    draw_ranges: UiDrawRanges,
}
```

内容缓存只保存与 camera/anchor 无关的数据：

```rust
struct UiCachedText {
    panel_id: u32,
    paint_group_id: u32,
    local_rect: [f32; 4],
    color: [f32; 4],
    clip_local: [f32; 4],
    glyph_uv: [f32; 4],
    glyph_alpha_ready: bool,
}
```

### 3.2 World presentation

```rust
struct UiWorldPresentation {
    paint_group_id: u32,
    translation: [f32; 2],
    scale: f32,
    base_depth: f32,
    visible: bool,
    frame_sequence: u64,
}
```

这一层每帧可以变化，但不能反向修改 NUI authoring/layout node 的原始 bounds。

### 3.3 GPU instance

最终上传给 GPU 的 instance 是内容缓存和 presentation 合成后的结果：

```rust
struct UiGpuPanelInstance {
    rect: [f32; 4],
    fill: [f32; 4],
    border: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
    depth: f32,
    panel_id: u32,
}
```

文字、图片和 Surface 也必须携带：

```text
paint_group_id
panel_id
depth
```

## 4. Dirty flag 设计

不要只使用一个 `dirty: bool`。使用分层标记：

```rust
struct UiDirtyFlags {
    content: bool,
    layout: bool,
    text: bool,
    resource: bool,
    world_transform: bool,
    depth_order: bool,
    color_target: bool,
    color_depth_target: bool,
    external_depth_target: bool,
    hit_target: bool,
    render_bundle: bool,
}
```

### 4.1 标记含义

| 标记 | 触发条件 | 必须执行 | 不必执行 |
| --- | --- | --- | --- |
| `content` | fragment/program revision、节点结构改变 | 重新生成内容缓存 | 不一定重建 GPU resource |
| `layout` | bounds、padding、gap、clip、字体尺寸改变 | 重新 layout | 不一定重建 Flow |
| `text` | 文本值、字体、字号改变 | 重新 glyph layout | 不必重新投影 World anchor |
| `resource` | Image/Surface/Font asset 变化 | 更新 atlas/bind group | 不必重建所有 Panel |
| `world_transform` | camera、anchor、scale 改变 | 更新 rect/depth | 不重新测量文字 |
| `depth_order` | group 深度或 layer 改变 | 重排 group range | 不重新解析文字 |
| `color_target` | Panel/Text/Image 内容或 transform 改变 | 重绘 color | 不一定重绘 ID |
| `color_depth_target` | Panel/Text/Image depth 覆盖改变 | 重绘内部 Depth32Float | 不一定更新外部 R32Float |
| `external_depth_target` | UI 与 Bevy 场景遮挡形状改变 | 重绘 R32Float | 不一定重绘 color 内容 |
| `hit_target` | 交互区域、visible、enabled、layer 改变 | 重绘 ID | 不一定重建文字 |
| `render_bundle` | pipeline、buffer range、draw topology 改变 | 重建 bundle | 不因 buffer 内容改变而重建 |

### 4.2 Dirty 传播规则

```text
content
  -> layout + text + resource + hit_target + color_target

layout
  -> text + world_transform + color_target + color_depth_target + hit_target

text
  -> color_target + color_depth_target

world_transform
  -> color_target + color_depth_target + external_depth_target

depth_order
  -> color_target + color_depth_target + external_depth_target + hit_target

resource
  -> color_target
```

不要反向传播：

```text
color_target dirty 不等于 layout dirty
external_depth_target dirty 不等于 text dirty
hit_target dirty 不等于 content dirty
```

## 5. Cache key

### 5.1 内容缓存 key

```rust
struct UiContentCacheKey {
    fragment_revision: Revision,
    program_revision: UiProgramRevision,
    input_revision: Revision,
    font_revision: u64,
    resource_revision: u64,
    viewport_logical_size: [u32; 2],
}
```

World camera position、anchor position 和 camera depth 不得放入内容 key，否则移动会摧毁缓存。

### 5.2 Presentation key

```rust
struct UiPresentationKey {
    renderer_epoch: u64,
    camera_revision: u64,
    anchor_revision: u64,
    viewport_revision: u64,
}
```

只要 presentation key 改变，就更新 World transform；不要重建内容缓存。

### 5.3 External frame key

```text
generation + buffer_index + frame_sequence
```

缓存不能跨 generation 使用。resize/device lost 后必须清空所有 external target cache。

## 6. 施工阶段

### 阶段 0：建立基线与计时

#### 任务

- 在 WGPU runtime 记录结构化计时。
- 不改变行为，只统计每帧各阶段耗时。

#### 必须记录

```text
snapshot_ms
invalidate_plan_ms
refresh_plan_ms
compose_visuals_ms
text_layout_ms
group_sort_ms
buffer_upload_ms
color_pass_ms
color_depth_pass_ms
external_depth_pass_ms
hit_pass_ms
render_bundle_build_ms
```

#### 验收

- 日志或 diagnostics 能按 frame_sequence 查询。
- 能区分“跳过渲染”和“渲染但没有空闲 buffer”。

### 阶段 1：静态 snapshot dirty cache

#### 任务

- 在 `HeadlessExternalGpu` 保存上一次成功渲染的 fragment snapshot。
- 当前 snapshot 与缓存相等且所有 surface 都完成更新时，跳过整帧 render。
- 三缓冲没有空闲 buffer 时不得更新成功缓存。

#### 规则

```text
静态 UI:
  不执行 ui.draw()

动态 UI:
  snapshot 不同，继续渲染

部分 surface 成功:
  保持 dirty，下一帧继续尝试
```

#### 验收

- 静态 UI 连续 5 秒不产生新的 color/depth/ID frame。
- camera/anchor 变化后下一次 render 必须产生新 frame。
- 变量变化后下一次 render 必须产生新 frame。

### 阶段 2：拆分内容缓存与 World presentation

#### 任务

- 禁止 `filter_world_panels()` 把 World projection 永久写回 authoring node bounds。
- 保留原始 layout bounds。
- 新增 renderer-local `UiWorldPresentation`。
- `flatten/layout/text measurement` 只消费内容数据。
- `instance buffer` 阶段应用 translation、scale、depth。

#### 目标链路

```text
NUI flow + vars
  -> content cache

camera + anchor
  -> world presentation

content cache + world presentation
  -> GPU instances
```

#### 验收

- 相机移动不触发字体测量。
- 相机移动不触发 Flow compile。
- 文字 glyph UV 和 advance 不变。
- Panel/Text/Image 只更新 rect/depth。

### 阶段 3：连续大 buffer 与 draw range

#### 任务

为每种 pipeline 保持一个连续 buffer：

```text
panel_buffer
text_buffer
image_buffer
surface_buffer
```

同一帧最多一次或少量连续写入：

```rust
queue.write_buffer(panel_buffer, 0, panel_bytes);
queue.write_buffer(text_buffer, 0, text_bytes);
queue.write_buffer(image_buffer, 0, image_bytes);
```

每个 group 保存 range：

```rust
struct DrawRange {
    paint_group_id: u32,
    start: u32,
    count: u32,
}
```

#### 禁止

```text
每个 group 都 queue.write_buffer(buffer, 0, group_data)
```

#### 验收

- Panel/Text 不因 group 数量线性增加 buffer upload 次数。
- BindGroup 仍绑定原 buffer，不因 range 变化重建。
- 同一帧 GPU buffer 内容和 draw range 一致。

### 阶段 4：增量 World transform

#### 任务

- 缓存每个 group 的 local geometry。
- camera/anchor 变化时只遍历可见 World group。
- 只写发生变化的 instance range。
- 变化范围包含：rect、clip、depth、visible。

#### 伪代码

```text
for group in world_groups:
  presentation = project(anchor, camera)
  if presentation == group.previous_presentation:
      continue
  update_group_instances(group, presentation)
  mark color_target dirty
  mark color_depth_target dirty
  mark external_depth_target dirty
```

#### 验收

- 静止 World group 不重复上传。
- 移动单个 World group 不上传其他 group。
- depth 改变时颜色 depth 和 external R32Float 同步更新。

### 阶段 5：RenderBundle

#### 适用条件

只有以下条件全部满足才使用 RenderBundle：

```text
pipeline 不变
BindGroup 不变
buffer layout 不变
draw range 不变
group topology 不变
```

#### Bundle 内容

```text
Panel pipeline + bind group + draw ranges
Text pipeline + font bind group + draw ranges
Image pipeline + atlas bind group + draw ranges
```

#### 不触发 bundle 重建的变化

```text
instance buffer 内容变化
World translation 变化
depth 值变化
文字颜色变化
```

#### 必须重建 bundle 的变化

```text
UI tree 结构改变
group 数量改变
draw range 改变
pipeline/layout 改变
资源 bind group 改变
```

#### 验收

- transform-only frame 不重建 RenderBundle。
- content topology 改变能重建 bundle。
- RenderBundle 的 color/depth format 与 render pass 完全匹配。

### 阶段 6：按 target dirty 更新

#### Color target

以下任一变化才重绘：

```text
Panel/Text/Image 内容
World transform
group depth/order
颜色或 opacity
```

#### Color Depth32Float target

以下任一变化才重绘：

```text
Panel/Text/Image bounds
depth
clip/alpha coverage
group order
```

#### External R32Float target

以下任一变化才重绘：

```text
World UI depth
World UI 可见性
World UI 覆盖区域
```

#### ID target

以下任一变化才重绘：

```text
PanelId / HitId
visible/enabled
hit policy
clip
group order
```

## 7. BindGroup 规则

### 必须缓存

```text
view_bind_group
font_bind_group
image_atlas_bind_group
render_surface_bind_group
pipeline layout
sampler
```

### 可以更新 buffer 内容

```text
viewport uniform buffer
panel instance buffer
text instance buffer
image instance buffer
```

### 只有以下情况重建 BindGroup

```text
底层 buffer 对象更换
atlas texture 更换
font texture 更换
RenderSurface target 更换
generation/resize/device lost
```

改变 buffer 中的数据不需要重建 BindGroup。

## 8. Buffer 容量与上传策略

### 容量

```text
当前 count <= capacity:
  复用 buffer

当前 count > capacity:
  next_power_of_two 扩容
  重建 buffer
  重建依赖该 buffer 的 BindGroup
  标记 render_bundle dirty
```

### 上传

```text
静态且无 dirty:
  不上传

局部 dirty:
  queue.write_buffer(buffer, range_offset, range_bytes)

大范围 dirty:
  一次连续上传
```

不要把 `queue.write_buffer` 放在每个 group 的 draw 循环中。

## 9. 三缓冲与缓存交互

每个 external buffer 都有独立生命周期：

```text
buffer 0:
  color
  Depth32Float internal
  R32Float external
  ID
```

缓存必须记录：

```text
generation
buffer_index
frame_sequence
content_cache_key
presentation_key
```

规则：

1. 不允许 color 使用 frame N、ID 使用 frame N-1。
2. 一个 buffer 只有在 consumer release 后才能重写。
3. 静态 UI 不生成新 frame，consumer 继续读取最后完整 frame。
4. 动态 UI 没有空闲 buffer 时丢弃当前更新，但保留 dirty 状态。
5. resize/device lost 后清空所有 target cache、bundle cache 和 presentation cache。

## 10. PanelId/HitId 缓存要求

`PanelId` 在同一个 frame key 内稳定。缓存中必须保存：

```text
PanelId
paint_group_id
UiHitBinding
visible/enabled
hit policy
ID draw range
```

以下变化会使 ID target dirty：

```text
节点结构变化
交互状态变化
clip 变化
group order 变化
```

只有 World translation/depth 变化时，不应重新分配 PanelId；只更新 ID pass 的几何位置和深度。

## 11. 施工顺序总表

| 阶段 | 工作 | 主要文件 | 依赖 | 完成标准 |
| --- | --- | --- | --- | --- |
| 0 | 计时与诊断 | `neon-wgpu-runtime/src/lib.rs` | 无 | 能看到每阶段耗时 |
| 1 | snapshot dirty cache | `lib.rs` | 阶段 0 | 静态 UI 不再提交新帧 |
| 2 | 内容/presentation 分离 | `lib.rs`, `ui_renderer.rs` | 阶段 1 | camera 移动不重新排版文字 |
| 3 | 连续大 buffer | `ui_renderer.rs` | 阶段 2 | 每类 buffer 每帧最多少量上传 |
| 4 | group 增量 transform | `ui_renderer.rs` | 阶段 3 | 单 group 移动不更新其他 group |
| 5 | RenderBundle | `ui_renderer.rs` | 阶段 3 | topology 不变时 bundle 复用 |
| 6 | target dirty | `lib.rs`, `ui_renderer.rs` | 阶段 4 | color/depth/ID 分离更新 |
| 7 | 三缓冲优化 | `lib.rs`, Bevy host | 阶段 6 | 无空闲 buffer 不破坏 dirty 状态 |
| 8 | 性能验收 | `tests/`, scenarios | 阶段 7 | p95/p99 达到预算 |

## 12. 验收场景

### 场景 A：静态 Screen UI

```text
保持 5 秒不动
期望：
  0 次 layout rebuild
  0 次 text upload
  0 次新 external frame
```

### 场景 B：单个 World anchor 移动

```text
只移动一个 anchor
期望：
  不重新 compile Flow
  不重新测量字体
  只更新对应 group instance range
  color/depth/external depth 同 frame 更新
```

### 场景 C：一个变量变化

```text
只修改 health
期望：
  只重建所属 UI group 内容
  其他 group 的 Panel/Text buffer range 不变
```

### 场景 D：文字内容变化

```text
修改一个 Label 文本
期望：
  只重建该 group 的 text range
  font atlas 未变时不重建 font BindGroup
  不重建其他 group 的文字
```

### 场景 E：只改变 depth

```text
World UI A depth 0.8 -> 0.3
期望：
  不重新布局文字
  更新 color Depth32Float
  更新 external R32Float
  Panel/Text 使用相同新的 base depth
```

### 场景 F：三缓冲背压

```text
consumer 故意落后
期望：
  producer 保留 dirty
  不把未完成帧记为缓存成功
  consumer 继续显示最后完整帧
```

### 场景 G：深度正确性回归

```text
近 Panel 覆盖远 Panel Text
同 Panel Text 显示在 Panel 上方
Bevy 场景更近时同时遮挡 Panel 与 Text
```

## 13. 性能目标

第一阶段目标：

```text
静态 UI：不产生新的 external render frame
静态 UI CPU draw preparation：接近 0
camera-only 更新：不发生 text layout
单 group 变化：不上传无关 group
```

第二阶段目标：

```text
静态内容的 BindGroup/Pipeline/RenderBundle 全部复用
动态 transform 只产生 instance buffer 局部更新
```

必须记录：

```text
CPU frame preparation p50/p95/p99
GPU color pass p50/p95/p99
GPU depth pass p50/p95/p99
buffer upload bytes/frame
draw calls/frame
BindGroup rebuilds/frame
RenderBundle rebuilds/frame
dropped external frames
```

## 14. 禁止事项

```text
禁止每帧创建 RenderPipeline
禁止每帧创建 BindGroup
禁止每个 group 从 offset 0 重写同一个 buffer 后再 draw
禁止 camera 移动时重新测量所有文字
禁止把 World projection 写回 authoring/layout node
禁止混用 Depth32Float 与外部 R32Float
禁止只更新 color 而不更新对应 depth/ID target
禁止三缓冲部分成功后清除 dirty 状态
禁止用固定 sleep 判断 GPU ready
禁止用旧 generation 的缓存继续采样
```

## 15. 最低实现版本

如果施工模型无法一次完成全部优化，必须按以下最低顺序交付：

```text
1. snapshot dirty cache
2. 内容缓存与 World presentation 分离
3. 单个大 Panel/Text buffer + draw ranges
4. BindGroup 复用
5. depth/ID 与 color dirty 同步
```

RenderBundle 和真正的 group-level offscreen cache 可以后置，但不能跳过前四项。
