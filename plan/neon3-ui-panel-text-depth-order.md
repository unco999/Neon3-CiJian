# Neon3 Panel 与文字统一深度排序施工方案

> 状态：施工计划，尚未实施。
>
> 目标：修复 World UI、普通 Panel、子节点和文字之间的绘制顺序，使文字与所属父 Panel
> 使用同一套外部遮挡深度策略，同时保留 UI 树内部正确的 painter order。

## 1. 问题定义

当前 UI renderer 将 Panel/Rect、Image、Surface、Text 分别收集到不同数组，再在 draw 阶段
分批绘制。这样做会丢失 UI 树中的真实绘制顺序：

```text
UI 树真实顺序：
  Parent Panel -> Parent Text -> Child Panel -> Child Text

分离数组后的顺序：
  所有 Panel -> 所有 Image -> 所有 Surface -> 所有 Text
```

当 World UI 引入 `world_depth` 后，如果只对 Panel 数组和 Text 数组分别排序或归并，还会产生
新的错误：

- 两个数组不一定按相同 depth 排序，归并结果不可靠。
- 父 Panel 可能在文字之后绘制，导致文字被覆盖。
- 文字可能使用自己的 depth 顺序，脱离所属 Panel。
- 普通 UI 的 2D painter order 被错误地当作 World UI 的场景遮挡深度。
- 文字写入外部 depth target 后，可能产生不合理的 glyph 形状遮挡。

## 2. 正确的深度概念

必须把两个概念分开：

### 2.1 UI painter order

表示同一张 UI 画布内部的绘制顺序：

```text
父节点背景 -> 父节点内容 -> 子节点背景 -> 子节点内容
```

它来自 UI 树的遍历顺序、top-layer、popup/modal 和声明式布局，不来自 camera depth。

### 2.2 World occlusion depth

表示 World UI 与宿主 3D 场景之间的遮挡关系，以及多个 World UI 之间的远近关系：

```text
远 World UI -> 近 World UI -> Screen UI
```

它来自 world anchor 的 camera-space depth。这个值用于对完整的 UI 绘制组排序，不能单独
重排文字，也不能破坏组内 painter order。

## 3. 目标绘制模型

所有可见绘制对象先统一转换成一个 `UiDrawItem` 序列。Panel、Image、RenderSurface 和 Text
不能再由四个互不关联的数组决定最终顺序。

```rust
struct UiDrawItem {
    kind: UiDrawKind,
    bounds: UiBounds,
    clip: UiBounds,
    occlusion_depth: f32,
    paint_group: PaintGroupId,
    paint_order: u32,
    layer: UiLayer,
}

enum UiDrawKind {
    Panel(UiInstance),
    Image(UiImageInstance),
    Surface(UiImageInstance),
    Text(UiTextInstance),
}
```

文字的 `occlusion_depth`、`paint_group` 和所属 Panel 一致，但文字本身仍保留自己的
`paint_order`，用于确保它在所属 Panel 背景之后绘制。

## 4. 绘制组定义

一个绘制组表示一个可独立进行 World UI 遮挡排序的 UI subtree。通常一个 World UI root
对应一个组；普通 Screen UI 也可以作为独立组，但不使用 camera occlusion depth。

```rust
struct UiPaintGroup {
    id: PaintGroupId,
    layer: UiLayer,
    occlusion_depth: Option<f32>,
    items: Vec<UiDrawItem>,
}

enum UiLayer {
    WorldDepthTested,
    WorldAlwaysVisible,
    Screen,
    Popup,
    Modal,
    Tooltip,
}
```

组内顺序必须由 flatten/layout 阶段直接生成：

```text
Parent Panel
Parent Image
Parent Text
Child Panel
Child Image
Child Text
```

不能在 draw 阶段重新根据对象类型拼出顺序。

## 5. 颜色 pass 的准确排序

颜色 pass 的排序规则是“先组、后组内”：

### 5.1 World UI

`WorldDepthTested` 组按 camera depth 从远到近绘制：

```text
occlusion_depth 大 -> 小
```

当前约定是 `0.0` 表示 near/always-on-top，`1.0` 表示 far，因此排序方向必须是：

```text
far group -> near group
```

这样近组后绘制，可以覆盖远组的 Panel 和文字。

### 5.2 AlwaysVisible 和 Screen UI

`WorldAlwaysVisible`、`Screen`、`Popup`、`Modal`、`Tooltip` 不应与普通 World depth 混排。
它们按固定层级在 World UI 之后绘制：

```text
1. WorldDepthTested：远到近
2. WorldAlwaysVisible
3. Screen
4. Popup
5. Modal
6. Tooltip
```

如果项目定义了 popup/modal 的具体优先级，应把它编码为稳定 layer order，而不是依赖 HashMap
遍历顺序。

### 5.3 组内 painter order

每组内部不按 `occlusion_depth` 重新排序，严格使用 flatten 产生的顺序：

```text
Panel background
Image / Surface
Text
Child subtree
```

文字在其所属 Panel 组内的相对顺序必须保持。这样可以保证：

- Panel 自己的文字显示在自己的背景之上。
- 近 World UI 的 Panel 和文字整体覆盖远 World UI。
- 子 Panel 可以按树结构覆盖父 Panel 的局部区域。
- 一个不相关的父节点不会因为类型批处理而重新覆盖文字。

## 6. 统一 draw item 的生成

应在 `flatten_node` 或紧邻的 presentation 阶段生成 draw item，而不是先生成 `instances`
和 `texts`，之后再猜测它们的关联关系。

推荐过程：

```text
flatten root
  -> 计算 bounds、clip、world_depth、layer
  -> 为当前 subtree 创建 PaintGroupId
  -> 追加 Panel draw item
  -> 追加 Image/Surface draw item
  -> 追加 Text draw item
  -> 递归 child，继续追加 child draw item
```

如果当前 renderer 仍需要按 pipeline 批处理，应使用“连续区间批处理”，但不能改变统一序列
中的顺序：

```text
UiDrawItem[0..3] = Panel, Panel, Text, Text
允许一次 Panel batch + 一次 Text batch，前提是二者不跨越会影响视觉的 item。
```

最安全的第一版实现是按连续 item 发 draw；性能优化应在视觉正确性验收后进行。

## 7. 文字与父 Panel 的深度关系

文字不拥有独立的 World UI 遮挡层级。它继承所属 World UI group 的：

```text
occlusion_depth
layer
paint_group
```

但它拥有独立的组内 `paint_order`。

示例：

```text
World Panel A depth=0.8
  A Panel
  A Text

World Panel B depth=0.3
  B Panel
  B Text
```

正确颜色顺序：

```text
A Panel
A Text
B Panel
B Text
```

结果：B 的 Panel 覆盖 A 的 Panel 和 A 的 Text，B 的 Text 显示在 B 自己的 Panel 上方。

错误顺序包括：

```text
A Panel
B Panel
A Text
B Text
```

或者：

```text
A Panel
A Text
B Text
B Panel
```

前者会让远组文字覆盖近组 Panel，后者会让 Panel 覆盖自己的文字。

## 8. 外部 depth target 的职责

颜色 pass 的 UI painter order 和导出的外部 depth target 不是同一个排序问题。

第一阶段建议：

```text
颜色 target：
  Panel、Image、Surface、Text 按统一 UiDrawItem/group 顺序绘制。

外部 depth target：
  只表达 UI group 或 Panel surface 对宿主 3D 场景的遮挡区域。
  文字不单独写入 depth target。
```

原因：文字 glyph 是透明 coverage。如果让 glyph 独立写外部 depth，宿主场景可能只在字形
像素位置被遮挡，形成不符合 Panel 语义的细碎遮挡孔洞。Panel 的 depth 应表示整个 UI surface
或 UI group 的场景遮挡边界。

`draw_depth()` 应使用与颜色 pass 相同的 Panel/group 可见性和 layer 选择，但不需要复现文字
的颜色绘制顺序。

## 9. ID 图也必须使用同一组顺序

ID target 不能继续单独按“所有交互节点收集顺序”绘制，否则视觉上最上面的文字/Panel 可能
与实际命中对象不一致。

ID pass 应复用统一 draw item 的：

```text
clip
visible/enabled
layer
paint_group
paint_order
```

只有具备交互语义的 item 才输出非空 hit ID；不可交互的 Panel 不应覆盖下面可交互 node 的
ID，除非产品明确规定 Panel 会拦截输入。

## 10. 当前代码对应的修复方向

当前 `UiWgpuRenderer::draw()` 的风险来自：

```text
self.instances
texts
images
surfaces
popup_instances
popup_texts
```

这些集合无法表达完整 UI 树 painter order。施工时应：

1. 保留现有 `UiVisual`、layout、clip 和 `world_depth` 计算。
2. 在 flatten/presentation 阶段给每个可绘制 node 生成统一 draw item。
3. 让文字从 `UiVisual.world_depth` 获得所属 group 的 depth，但不单独参与 group 排序。
4. 按 group far-to-near 排序，再按 item 原始 painter order 绘制。
5. 普通 Screen UI 和 popup/modal 使用固定 layer order，不参与 World depth 浮点排序。
6. `draw_depth()` 只写 Panel/group depth，不把文字 glyph 作为独立场景遮挡面。
7. ID pass 复用相同 group/item 顺序，确保视觉和点击一致。

在统一 draw item 迁移完成前，不应继续添加“文字 depth merge”或“文字独立 depth pass”补丁。
那类补丁只能修一个局部排序案例，无法解决父子节点和跨 Panel 的完整顺序问题。

## 11. 施工阶段

### 阶段 1：建立排序模型

- 定义 `UiLayer`、`PaintGroupId`、`UiDrawItem`、`UiPaintGroup`。
- 为 `UiVisual` 补充稳定的 group/painter metadata，或在 presentation 层维护旁路表。
- 增加纯 CPU 排序单元测试，不依赖 GPU。

### 阶段 2：统一颜色绘制

- flatten 时生成 Panel/Image/Surface/Text 的统一 item 序列。
- 按 group far-to-near 排序。
- 组内保留 UI 树 painter order。
- popup/modal/tooltip 固定在正确 top layer。

### 阶段 3：深度 target

- 让 Panel/group depth 继续输出到外部 depth target。
- 删除或禁止文字独立写外部 depth 的实验路径。
- 验收 World UI 与 Bevy 场景互相遮挡不出现 glyph 孔洞。

### 阶段 4：统一 ID pass

- 复用颜色 pass 的 group/layer/clip/paint order。
- 验收视觉最上层的 UI 同时是命中最上层的 UI。

### 阶段 5：性能优化

- 只对连续同 pipeline item 做 batch。
- 使用 group 内连续区间合并 draw call。
- 不允许以 batch 为理由跨越不同 painter group。

## 12. 验收用例

1. 一个 Panel 内：背景、Image、Text、Child Panel、Child Text 的显示顺序正确。
2. 远 World UI 的文字不会覆盖近 World UI 的 Panel。
3. 近 World UI 的文字显示在近 Panel 之上。
4. Screen UI 文字覆盖所有 World UI。
5. Popup/Modal/Tooltip 的 Panel 和文字都保持自身内部顺序，并位于声明的 top layer。
6. Panel 被场景遮挡时，文字不会通过独立 depth 继续显示。
7. 文字 glyph 不会单独制造宿主场景 depth 孔洞。
8. 视觉最上面的可交互控件与 ID 图命中结果一致。
9. 同一个 UI tree 改变 world anchor depth 后，只改变 group 之间的顺序，不改变组内文字顺序。
10. 普通 2D UI 没有 world depth 时，绘制结果与改动前一致。
