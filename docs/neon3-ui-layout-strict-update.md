# Neon3 UI 排版系统严格更新规范

> 本文只讨论 UI layout、measurement、overflow、clip、文本排版、组件尺寸和 WorldUi transform。
> 不讨论业务规则，不重新设计 semantic intent，不实现新的动画业务。
> 施工对象为 DeepSeek，必须按章节顺序执行。

## 0. 现有组件基线

完整组件案例位于：

```text
D:\Neon3\crates\neon-ui-runtime\tests\fixtures\ui\imgui-component-gallery.nui
```

该案例已经覆盖：

```text
surface
panel
row
column
overlay
scroll
branch
modal
dialog
tooltip
button
input
progress_bar
checkbox
radio_button
slider
drag_value
combo
dropdown
tabs
selectable
list_box
scrollbar
data_grid
template
drag/drop
render surface
```

本次任务不是重复创建组件，而是让现有组件全部使用一致、可预测的排版系统。

## 1. 排版系统目标

最终必须满足：

1. 同一份 Flow 在字体未加载、字体已加载、窗口 resize、2x backing、WorldUi scale 下都不重叠。
2. 文本不会因为固定高度被裁掉最后一行。
3. 显式 `w/h` 是约束，不是允许内容被裁剪的理由。
4. `h=0` 和 `w=0` 表示自动测量。
5. `padding`、`margin`、`gap` 必须参与测量。
6. Row/Column 的 child track 计算必须稳定。
7. Branch 是结构容器，不生成无意义背景，不把子节点叠在同一个原点。
8. Modal/Dialog 是 top layer，具有明确 pointer blocking 行为。
9. WorldUi 先完成 logical layout，再统一 transform；距离不能触发 child relayout。
10. 字体大小不能由 node `bounds.height` 反向决定。
11. 所有组件必须能在 `imgui-component-gallery.nui` 中稳定显示。
12. Layout 结果必须可通过 deterministic test 检查。

## 2. 禁止的错误模型

以下实现全部禁止：

```text
child 没有显式 y，就默认和上一个 child 重叠
parent 固定高度不足时静默裁掉 child
文本总宽度 / 容器宽度 作为唯一换行算法
文本节点高度决定字体大小
WorldUi 距离变化时修改 padding/gap/child bounds
每个 WorldUi child 单独 scale
border 使用独立 depth 或 paint group
HashMap iteration order 决定绘制顺序
字体未加载时和字体加载后使用不同布局规则
```

## 3. 数据模型

### 3.1 Logical layout result

每个 flattened node 必须保留 logical 几何：

```rust
struct LogicalLayoutBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    content_x: f32,
    content_y: f32,
    content_width: f32,
    content_height: f32,
    clip: Option<UiBounds>,
}
```

这个结果只由以下内容决定：

```text
node declaration
parent layout
font metrics
text content
padding
margin
gap
min/max/preferred size
viewport
```

不能包含 camera distance。

### 3.2 Final visual transform

WorldUi 在 logical layout 完成后再处理：

```rust
struct FinalVisualTransform {
    origin: [f32; 2],
    uniform_scale: f32,
    world_depth: Option<f32>,
}
```

ScreenUi 使用：

```text
origin = [0, 0]
uniform_scale = 1
```

WorldUi 使用：

```text
origin = root bottom-center
uniform_scale = distance_scale
```

Transform 只作用于最终 visual box、text instance、border instance、progress instance 和 clip。

## 4. 尺寸语义

### 4.1 显式尺寸

```text
w > 0: 声明宽度的最小保证
h > 0: 声明高度的最小保证
```

显式高度不能导致内容被裁掉。如果 intrinsic content 高度更大：

```text
resolved_height = max(declared_height, intrinsic_height)
```

只有显式 overflow/clip 才允许裁剪。

### 4.2 自动尺寸

```text
w=0: auto width
h=0: auto height
```

Column 容器：

```text
content_width = max(child outer widths)
content_height = sum(child outer heights) + gap * (visible_count - 1)
width = max(declared_width, padding_left + content_width + padding_right)
height = max(declared_height, padding_top + content_height + padding_bottom)
```

Row 容器：

```text
content_width = sum(child outer widths) + gap * (visible_count - 1)
content_height = max(child outer heights)
width = max(declared_width, padding_left + content_width + padding_right)
height = max(declared_height, padding_top + content_height + padding_bottom)
```

Overlay 容器：

```text
width = max(declared_width, max(child right edge) + padding)
height = max(declared_height, max(child bottom edge) + padding)
```

Absolute 容器：

```text
child x/y 使用声明值
child 不参与自动 track 排列
如果 parent auto-size，则计算所有 child 的 right/bottom extent
```

### 4.3 Margin

Margin 不属于 child content box，但属于 parent track：

```text
row main axis = margin start + width + margin end
column main axis = margin top + height + margin bottom
```

不能把 margin 同时加进 bounds width/height，否则会重复计算。

## 5. Intrinsic text measurement

### 5.1 单一测量源

`intrinsic_size()` 和 `layout_text()` 必须调用同一个底层函数：

```rust
measure_text_lines(text, available_width, font_metrics) -> TextMeasure
```

返回：

```rust
struct TextMeasure {
    line_count: u32,
    max_line_width: f32,
    total_height: f32,
    line_height: f32,
}
```

不能分别实现两套近似算法。

### 5.2 换行规则

按字符逐个累计真实 glyph advance：

```text
显式 '\n' -> 立即换行
当前行宽 + glyph advance > available_width -> 换行
空文本 -> 至少一行高度
```

中文、日文、韩文、emoji、ASCII 必须使用实际 font metrics；字体未加载时使用统一 fallback metrics，但字体加载后必须触发布局 revision，重新测量。

### 5.3 Text safe inset

文本可用宽度：

```text
available_width = max(node_width - horizontal_text_inset, 1)
```

默认：

```text
Label/Button: 8 logical px
TextInput: TEXT_INPUT_INSET * 2
```

实际绘制和 intrinsic 测量必须使用同一个 inset。

### 5.4 禁止文字自动字号反馈

禁止：

```rust
text_scale = bounds.height / 28.0
```

正确规则：

```text
logical font scale = 1.0
WorldUi final glyph transform = root uniform_scale
```

文字不能独立于 panel 自动放大或缩小。

## 6. Layout pipeline

必须拆成明确阶段：

```text
Stage 1: parse/compile
Stage 2: resolve branch visibility
Stage 3: measure intrinsic content
Stage 4: resolve parent size
Stage 5: resolve row/column tracks
Stage 6: resolve child logical boxes
Stage 7: resolve logical clip
Stage 8: assign semantic bindings
Stage 9: apply WorldUi final transform
Stage 10: build color/depth/ID instances
```

禁止在 Stage 9 之后回头修改 Stage 4-7 的数据。

## 7. Branch strategy

Branch 是条件结构节点：

```text
visible = predicate(input/state)
style = transparent
background alpha = 0
border alpha = 0
default layout = Column
```

Branch 不应该制造额外可见背景，也不能让 inactive branch 占用 layout。

当前完整组件案例中的 branch：

```text
bakery-view
grocery-view
bathhouse-view
```

必须验证：

```text
只有 active branch 进入 layout
inactive branch 不进入 intrinsic measurement
active branch 子节点不重叠
切换 branch 后父容器重新计算一次 layout
```

## 8. Row/Column track 算法

### 8.1 Track 输入

每个 child track 保存：

```rust
struct Track {
    visible: bool,
    base_size: f32,
    min_size: f32,
    max_size: f32,
    flex_grow: f32,
    flex_shrink: f32,
    margin_start: f32,
    margin_end: f32,
}
```

### 8.2 分配顺序

严格按以下顺序：

1. 移除 invisible child。
2. 计算 intrinsic/base size。
3. clamp 到 min/max。
4. 扣除 margin 和 gap。
5. 计算剩余空间。
6. 按 flex_grow 分配剩余正空间。
7. 按 flex_shrink 分配负空间。
8. 再次 clamp。
9. 对齐 cross axis。
10. 写入 child logical bounds。

不能在分配过程中改变 child 的 intrinsic content。

### 8.3 Overflow

当内容超出 parent：

```text
auto parent: 扩展 parent
fixed parent + clip none: 内容可溢出但不裁
fixed parent + clip bounds: 按 clip 裁剪
scroll: 生成 scroll viewport 和 content extent
```

默认不能静默裁剪。

## 9. 组件尺寸规范

完整组件案例应作为回归基准。

### Button

```text
默认高度: 30 logical px
最小水平 padding: 10
文本超宽: parent auto-expand 或明确 overflow
pressed/hover 不改变 layout size
```

### Slider

```text
默认高度: 30
track 在 content box 内
label 区和 track 区使用固定比例
thumb 不改变布局尺寸
numeric value 只改变 thumb/progress
```

### ProgressBar

```text
背景 track 固定
fill width 随 numeric value 变化
不能改变 parent layout
```

### Checkbox/Radio

```text
控件 glyph 占固定尺寸
label 占剩余 track
checked 不改变 row 高度
```

### Combo/Dropdown/Tabs

```text
control 本体不因 popup 改变 layout
popup 属于 top layer
popup 使用独立 clip
popup 不改变 parent track
```

### DataGrid

```text
header height 固定
row height 使用声明 row_height
column width 不由当前单元格文本随机改变
横向 overflow 使用 scroll
虚拟行不进入整个 layout tree
```

### Template/Repeat

```text
prototype 只测量一次
instance 使用 stable row key
instance 不重复生成全套资源和字体 layout
```

## 10. Clip 和边界

所有 visual 必须区分：

```text
layout bounds
paint bounds
clip bounds
hit bounds
```

默认关系：

```text
paint bounds = layout bounds
hit bounds = paint bounds
clip bounds = parent clip
```

如果组件需要特殊行为，必须显式声明：

```text
clip none
clip bounds
clip rounded
clip scroll
```

WorldUi transform 后必须同时变换：

```text
paint bounds
hit bounds
clip bounds
text instances
border instances
depth instances
```

任何只变换 color、不变换 ID/depth 的实现都禁止合入。

## 11. WorldUi 专用规则

WorldUi 的正确流程：

```text
logical layout at authored size
anchor projection -> root x/y
view distance -> root uniform scale
root bottom-center as transform origin
apply final transform to all descendants
```

禁止：

```text
distance -> modify padding
distance -> modify gap
distance -> change child topology
```

WorldUi border 必须和 body 使用同一个：

```text
world_depth
paint_group_id
uniform_scale
clip
```

## 12. Cache 规则

### Layout cache key

必须包含：

```text
fragment_id
fragment_revision
viewport logical size
font generation
input branch state
content text hash
```

不能包含 camera distance。

### Text cache key

必须包含：

```text
node path
text content
font generation
logical available width
logical line height
```

不要把 WorldUi uniform scale 当成新的 text layout 宽度；最终 scale 在 transform 阶段处理。

### WorldUi transform cache

可以每帧更新：

```text
root screen position
root scale
depth
```

不得让这些变化清空 logical text/layout cache。

## 13. 完整组件案例验收

必须使用：

```text
D:\Neon3\crates\neon-ui-runtime\tests\fixtures\ui\imgui-component-gallery.nui
```

验证以下页面：

```text
gallery-controls
gallery-layout
sky-banner
button
input
progress_bar
image-preview
checkbox
radio_button
slider
drag_value
combo
dropdown
tabs
selectable
list_box
scrollbar
tooltip
dialog
data_grid
template
drag/drop
render surface
```

每个组件必须验证：

```text
不重叠
不超出 parent 边界
文字完整
disabled 状态不改变尺寸
hover/pressed 不改变尺寸
popup 不改变 parent layout
scroll 不破坏 sibling layout
branch 切换后布局稳定
```

## 14. 必须新增的测试

### 14.1 Intrinsic text

```text
ASCII 单行
CJK 单行
CJK 自动换两行
显式换行
超长英文 token
emoji
字体未加载
字体加载后重新布局
```

### 14.2 Container

```text
Column padding + gap
Row padding + gap
auto parent height
explicit minimum height
child height larger than declared parent
margin 不重复计算
invisible branch 不占空间
```

### 14.3 WorldUi

```text
camera distance 变化不修改 logical child bounds
uniform scale 只应用一次
text 与 border 使用同一 scale
ID 与 color 使用同一 transform
depth 与 border 使用同一 paint group
```

### 14.4 GPU

```text
2x backing / 1x logical mapping
R32Uint ID 坐标正确
border 被 scene occluder 遮挡
panel body 和 border 同时可见/同时被遮挡
near panel 覆盖 far panel
```

## 15. 性能要求

稳定状态下：

```text
camera distance 变化不能触发 text layout
camera distance 变化不能重新 parse/compile Flow
camera distance 变化不能生成新 node topology
WorldUi transform update 应只改 instance/transform 数据
```

必须记录：

```text
layout_count
text_layout_count
world_transform_update_count
color_pass_ms
id_pass_ms
```

验收目标：

```text
连续相机拖动期间 text_layout_count 不增加
连续相机拖动期间 topology revision 不增加
steady render dropped_frames = 0
```

## 16. 施工顺序

必须按顺序施工：

1. 给 `UiVisual`、flatten result、text cache 补齐 logical/final transform 语义。
2. 统一 `measure_text_lines()`，让 intrinsic 和实际绘制共用。
3. 完善 Row/Column track 分配。
4. 修复 branch、modal、scroll、popup 的布局边界。
5. 完善 clip/hit/paint/depth 四套 bounds 的关系。
6. 移除所有 bounds.height 驱动字体缩放代码。
7. 完成 WorldUi root uniform transform。
8. 接通 2x physical / 1x logical viewport。
9. 修复 border/chrome depth group。
10. 用完整组件 fixture 做回归。
11. 执行 focused tests。
12. 执行 package tests。
13. 启动真实 WGPU/UI/host，读取 JSONL metrics。

## 17. 施工完成报告必须包含

```text
修改文件列表
layout pipeline 变化
intrinsic measurement 变化
WorldUi transform 变化
2x backing 实际尺寸
完整组件 fixture 测试结果
文本裁剪测试结果
WorldUi border depth 测试结果
camera drag 期间 text_layout_count
camera drag 期间 dropped_frames
warnings 单独列表
failures 单独列表
```

如果不能证明“camera distance 不触发 text relayout”，就不能宣称本次布局更新完成。
