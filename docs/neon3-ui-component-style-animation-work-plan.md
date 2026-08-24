# Neon3 UI 组件、样式、动画完整功能推进表

> 施工对象：DeepSeek
>
> 目标：补齐 Neon3 现有 UI Flow 中的组件能力、视觉样式能力和状态动画能力，确保无窗口链路与窗口链路使用同一套 UI 语义、同一套 layout/composition/render contract。
>
> 施工原则：先统一 contract 和 owner，再补组件；先补确定性状态，再补动画；先让 headless scenario 通过，再做真实 WGPU/window 验收。

## 当前施工进度

截至当前工作会话，已实际落地：

- `UiComponentMetrics`、`UiComponentCapabilities`、`UiComponentSpec` renderer-local component spec。
- 默认文字 inset、交互能力判断开始统一从 component spec 读取。
- component spec 覆盖全部 `UiNodeKind` 的 focused test。
- 已开始接入统一 state style resolver：`UiStateFlags`、`UiStylePatch`、`resolve_component_style()`，body 样式统一经过状态解析入口。
- component chrome 已接入同一 style resolver，避免 body 与 glyph/track/thumb chrome 的 disabled/hover/pressed/focus/selected 状态不一致。
- 已新增 `UiAnimationStatus`、`UiAnimationSpec`、`UiAnimationInstance`，并将现有 `ActiveTransition` 映射到 running/completed lifecycle diagnostics；目前仍需继续补 supersede/cancel 的显式存储与回归。
- 动画生命周期 focused test 已通过；当前阶段先保留现有 `ActiveTransition` 作为唯一运行时存储，避免并行维护第二套动画执行器。
- animation history 已记录 completed/cancelled；下一步仅需把 retarget 分支的 superseded 事件接入同一 history，不得另建执行器。
- retarget 分支已记录 `Superseded`，新 transition 的 from 使用旧 transition 当前 sampled visual；focused test 与真实 window scenario 已通过。
- 已新增 `UiLayoutCounters`：`layout_count`、`text_layout_count`、`world_transform_update_count`，并提供 renderer diagnostics 输出入口，为 camera drag 性能验收准备。
- 已新增 WorldUi 性能回归：视觉 transform 更新不增加 `layout_count` 或 `text_layout_count`。
- counters 已接入 headless/window diagnostics：可读取 screen/world/unified renderer 的 layout/text/world-transform 计数。
- `neon-dev probe-window-metrics <endpoint> [samples] [interval-ms]` 已加入，输出 bounded JSONL metrics samples/summary。
- probe 已能读取 window debug snapshot 中的 `layout_counters.window_ui`，包括 layout/text/world-transform 计数。
- toggle prediction、O(1) plan/hit index、窗口/headless pointer/revision 修复已落地。
- `component-gallery-window-input` 已通过真实 WGPU/window scenario。

仍未完成：

- 完整 state style set（disabled/hover/pressed/focus/open）。
- 所有组件逐项 L6 验收。
- 完整 animation spec/store/retarget contract。
- workspace 全量测试和完整 JSONL component probe。

---

## 0. 先读的文件

开始施工前必须阅读：

```text
D:\Neon3\AGENTS.md
D:\Neon3\docs\neon3-ui-layout-strict-update.md
D:\Neon3\plan\neon3-nui-flow.md
D:\Neon3\crates\neon-ui-runtime\tests\fixtures\ui\imgui-component-gallery.nui
```

核心代码：

```text
D:\Neon3\crates\neon-ui-schema\src\lib.rs
D:\Neon3\crates\neon-ui-runtime\src\lib.rs
D:\Neon3\crates\neon-ui-runtime\src\nui_flow.rs
D:\Neon3\crates\neon-ui-runtime\src\nui_state_machine.rs
D:\Neon3\crates\neon-ui-runtime\src\demo_domain.rs
D:\Neon3\crates\neon-wgpu-runtime\src\ui_renderer.rs
D:\Neon3\crates\neon-wgpu-runtime\src\lib.rs
D:\Neon3\crates\neon-dev\src\main.rs
```

当前完整组件 fixture：

```text
D:\Neon3\crates\neon-ui-runtime\tests\fixtures\ui\imgui-component-gallery.nui
```

---

## 1. 施工目标

最终必须满足：

1. fixture 中声明的每种组件都有完整的 layout、paint、text、hit、disabled、focus、hover、pressed、selected、popup 或 scroll 行为。
2. 组件的业务值由 domain/runtime 提供，renderer 不推断业务事实。
3. 窗口渲染和无窗口渲染使用同一套 `UiFragment -> composed visual -> semantic event` 链路。
4. 窗口只增加 OS window、surface、scale factor 和 pointer source；不能复制一套业务交互逻辑。
5. 样式变化不改变 layout 尺寸，除非声明的是 layout 属性变化。
6. hover/pressed/checked/selected/disabled 只改变视觉状态，不改变 track、bounds 或 sibling 位置。
7. 动画不会改变业务 revision，不会制造每帧 IPC request，不会触发每帧 Flow parse/compile。
8. transition 的 from/to、request、fragment revision、input revision、interaction sequence 能够结构化追踪。
9. 所有 popup 属于 top layer，不改变父容器 track。
10. 所有 scroll child 只应用一次 scroll transform。
11. 所有文本在 logical layout 阶段测量，在 visual transform 阶段移动；滚动时不复用旧屏幕坐标。
12. 所有 hit 结果与当前 composed visual 同帧对应，不允许使用旧 GPU ID 覆盖当前 capture。
13. 组件在 1x、2x backing、resize、font generation、WorldUi scale、scroll 下保持一致。
14. 每项功能都有 focused test、scenario 或 GPU probe。

---

## 2. 当前模型与缺口

### 2.0.3 当前真实可实现的 Style / Animation 字段

以下矩阵按“代码当前是否真的能影响最终像素”统计，不按枚举是否存在统计。

#### Style 字段

| 字段 | schema `UiStyle` | Flow/UiNode 当前可声明 | renderer body | chrome/track | state patch | 备注 |
|---|---:|---:|---:|---:|---:|---|
| `background_color` | 是 | 是 | 是 | 部分 | 内部 resolver | body 与部分 chrome 生效 |
| `border_color` | 是 | 是 | 是 | 是 | 内部 resolver | body/chrome/depth 需继续统一 |
| `border_width` | 是 | 是 | 是 | 部分 | 内部 resolver | 不应改变 logical layout |
| `corner_radius` | 是 | 是 | 是 | 是 | 内部 resolver | clip rounded 仍需完整验收 |
| `opacity` | 是 | 是 | 是 | 是 | 内部 resolver | 会影响 hit/paint policy，需固定规则 |
| `text_color` | patch contract | 否 | 否 | 否 | 只定义未接入 | 需要加入 text visual/style resolve |
| `shadow_color` | 否 | 否 | 否 | 否 | 否 | 尚未实现 |
| `shadow_offset` | 否 | 否 | 否 | 否 | 否 | 尚未实现 |
| `shadow_blur` | 否 | 否 | 否 | 否 | 否 | 尚未实现 |
| `fill_color` | 否 | 否 | 部分 | 部分 | 否 | 当前由组件 chrome 内部颜色生成 |
| `track_color` | 否 | 否 | 部分 | 部分 | 否 | Slider/Scrollbar/ProgressBar 仍是内部常量 |
| `thumb_color` | 否 | 否 | 部分 | 部分 | 否 | Slider/Scrollbar 仍是内部常量 |
| `selection_color` | 否 | 否 | 部分 | 部分 | 否 | TextInput/ListBox 尚未统一公开 |
| `font_size` | 否 | 否 | 否 | 否 | 否 | 当前固定 raster size，不能误称支持 |
| `font_weight` | 否 | 否 | 否 | 否 | 否 | 尚未实现 |

当前可以直接通过 `UiNode.style` 控制的字段只有：

```text
background_color
border_color
border_width
corner_radius
opacity
```

`UiStylePatch.text_color` 已进入公共 contract，但 renderer 尚未把它接到 `UiTextInstance.color`，因此当前标记为“定义存在、功能未完成”。

#### Style 状态

| 状态 | resolver 已有 | schema `UiStyleStateSet` 已有 | Flow 可声明 | 当前实际效果 |
|---|---:|---:|---:|---|
| normal | 是 | base `UiStyle` | 部分 | 生效 |
| hover | 是 | 是 | 否 | renderer 默认提亮 |
| pressed | 是 | 是 | 否 | renderer 默认压暗 |
| focused | 是 | 是 | 否 | renderer 默认高亮边框 |
| disabled | 是 | 是 | 否 | renderer 默认降 opacity/颜色 |
| selected | 是 | 是 | 否 | presentation 驱动 |
| checked | 是 | 是 | 否 | toggle presentation 驱动 |
| open | 是 | 是 | 否 | popup owner 内部状态 |

结论：当前已经有“状态行为”，但还没有完成“Flow 可配置状态主题”。

#### Animation 字段

| 字段/属性 | schema | renderer interpolation | 当前实际状态 |
|---|---:|---:|---|
| `delay_ms` | 是 | 是 | 生效 |
| `duration_ms` | 是 | 是 | 生效 |
| `easing` | 是 | 是 | Linear/EaseIn/EaseOut/EaseInOut 生效 |
| `motion_key` | 是 | diagnostics/state motion | 生效 |
| `bounds` / position | 是 | 是 | 生效，GPU rect interpolation |
| size | 是（包含在 bounds） | 是 | 生效 |
| `background_color` | 是 | 是 | 生效 |
| `border_color` | 是 | 是 | 生效 |
| `border_width` | 是 | 是 | 生效 |
| `corner_radius` | 是 | 是 | 生效 |
| `opacity` | 是 | 是 | 生效 |
| `numeric_value` | 是 | 部分 | Slider/Progress/DragValue 需逐项验收 |
| text color | 否 | 否 | 未实现 |
| font size | 否 | 否 | 禁止从 bounds.height 推导 |
| font weight | 否 | 否 | 未实现 |
| shadow | 否 | 否 | 未实现 |
| clip shape | 否 | 否 | 需要显式设计，不能动画中回写 layout |
| child topology | 否 | 否 | 禁止动画修改 topology |

当前可以实际实现的 animation property：

```text
position
size
opacity
background_color
border_color
border_width
corner_radius
numeric_value
```

当前明确不能实现、也不能由 DeepSeek 偷偷补成 bounds 反馈的字段：

```text
font_size
font_weight
text_wrap_width
child_topology
branch_business_state
project/domain value
```

### 2.0.4 下一批字段施工优先级

必须按以下顺序补，不得先实现阴影装饰：

1. 把 `UiStylePatch.text_color` 接到 `UiTextInstance.color`。
2. 把 `UiStyleStateSet` 挂到 node 的兼容声明入口或 style effect。
3. 将 Slider/ProgressBar/Scrollbar 的 track/fill/thumb 颜色变成统一 style token。
4. 将 TextInput selection/caret color 变成 renderer-local presentation style。
5. 为 animation property 增加明确 property mask，避免 transition 意外插值未声明字段。
6. 之后才考虑 shadow、font weight 等新增 GPU/字体资源能力。

### 当前已落地的 CSS-like Flow 字段

当前 NUI Flow 已可解析并写入 `UiNode.style`：

```text
fill <color>          -> background_color
line <color>          -> border_color
opacity <0..1>        -> opacity
radius <number>       -> corner_radius
border_width <number> -> border_width
```

已实际验证：

```text
css_like_visual_attributes_are_parsed_into_style: passed
```

字体 atlas 已采用 Nearest sampler，目标是强化 authored logical size 下的字形边缘；raster size 和 coverage threshold 保持不变。

`ink` 目前仍要求 `token:<name>`，但还没有把 token registry/text color 接入最终 `UiTextInstance.color`；在 DeepSeek 施工中不得把 `ink` 当前的 token 校验误报为文字颜色已经生效。

字体采样当前使用 Nearest atlas sampling，目标是让 authored logical size 下的字形边缘更硬朗；没有通过提高 coverage threshold 的方式锐化，以避免 CJK/emoji 细笔画被裁掉。

### 2.0 重要说明：本计划不重复创建已有基础组件

当前仓库已经有大量基础组件声明、Flow parser 分支和 gallery fixture。本计划的“补齐”不是重新写一套 Button/Checkbox/Slider，而是按下面的成熟度分级推进：

```text
L0 只有 UiNodeKind 枚举或 parser 关键字
L1 能生成 UiNode，能显示基础 body
L2 有 layout/paint/text/hit 的静态实现
L3 有 disabled/hover/pressed/focus/selected 等状态
L4 有 typed presentation、一次点击语义链和 revision 回写
L5 有 popup/scroll/virtualization/WorldUi 等复杂边界
L6 有 animation、headless/window parity 和 JSONL 验收证据
```

DeepSeek 不得因为组件已经出现在 fixture 中，就把它标记为完成；也不得因为组件已经有 renderer 分支，就重写为第二套组件。施工目标是把每个组件从当前等级推进到 L6。

### 2.0.1 当前组件基线分级

下表是施工开始时的基线方向，具体等级必须由测试确认：

| 组件 | 当前已有内容 | 不能直接视为完成的部分 | 施工目标 |
|---|---|---|---|
| Panel | schema、parser、layout、基础 body | style state、clip/hit/depth 一致性、scroll viewport | L6 |
| Label | schema、文本绘制、fixture | font generation、scroll refresh、WorldUi transform、完整 clip | L6 |
| Button | schema、parser、基础 body/text/hit | 一次点击、revision、pressed/focus、动画、window parity | L6 |
| Image | schema、AssetRef、atlas 绘制 | 2x、clip、WorldUi、missing asset 状态 | L6 |
| RenderSurface | schema、surface target | surface hit、window/headless target parity、resize | L6 |
| TextInput | schema、IME/caret/selection 基础实现 | scroll、commit revision、clip、window parity | L6 |
| Checkbox | schema、checked binding、基础 chrome | optimistic toggle、rollback、一次点击、disabled/focus | L6 |
| RadioButton | schema、selected binding、基础 chrome | selected 语义、group/domain boundary、一次点击 | L6 |
| Slider | schema、numeric presentation、drag 基础实现 | single gesture commit、thumb/track bounds、rollback | L6 |
| DragValue | schema、numeric presentation、drag 基础实现 | integer commit、一次 gesture、revision queue | L6 |
| Combo | schema、choice presentation、基础 body | popup ownership、option hit、top layer、one-click select | L6 |
| Dropdown | schema、choice presentation、基础 body | popup clip、outside click、parent track isolation | L6 |
| Tabs | schema、choice presentation、branch fixture | branch layout、active indicator、state machine motion | L6 |
| Tooltip | schema、top-layer 类型、fixture | hover lifecycle、pointer policy、独立 clip | L6 |
| Modal | schema、top-layer/backdrop 部分实现 | pointer blocking、backdrop depth、close lifecycle | L6 |
| Dialog | schema、top-layer/backdrop 部分实现 | dialog focus、close/revision、animation、clip | L6 |
| Selectable | schema、toggle presentation | selected prediction、hit order、disabled/focus | L6 |
| ListBox | schema、choice presentation、option text | option hit、scroll、fixed row、selection revision | L6 |
| Scrollbar | schema、scroll presentation、thumb 基础实现 | scroll transform 一次应用、drag commit、clip | L6 |
| ProgressBar | schema、numeric presentation、fill 基础实现 | fill 不改 layout、disabled/style/animation | L6 |
| DataGrid | schema、bounded frame、virtual rows、cell hit 基础实现 | sticky header、cell revision、scroll、2x、window parity | L6 |
| Template/Repeat | schema、parser、stable instance 基础实现 | prototype cache、stable row key、resource reuse | L6 |
| Branch | parser、predicate、透明 Column 容器 | branch switching regression、topology/layout cache | L6 |
| WorldUi | world depth/scale、anchor pipeline 部分实现 | root transform、border/depth parity、camera cache | L6 |

### 2.0.2 施工动作的判断规则

对每个组件先做以下判断：

1. 如果 parser/schema 没有能力：补 contract。
2. 如果 schema 有但 renderer 没有：补 renderer implementation。
3. 如果能静态显示但不能一次交互：补 shared pointer/revision pipeline。
4. 如果交互成功但反馈延迟：补 local prediction + authoritative reconciliation。
5. 如果窗口和无窗口结果不同：抽到 shared core，不允许复制修复。
6. 如果已有测试通过：保留并扩展边界测试，不重写已有实现。
7. 如果组件只是 fixture 中的静态案例：不能声称它已经完成。

### 2.1 当前已有 schema

当前 `UiNode` 已有：

```rust
pub struct UiNode {
    pub node_id: UiNodeId,
    pub kind: UiNodeKind,
    pub bounds: UiBounds,
    pub layout: Option<UiLayout>,
    pub visible: bool,
    pub enabled: bool,
    pub text_key: Option<String>,
    pub text: Option<TextRef>,
    pub image: Option<AssetRef>,
    pub surface: Option<RenderSurfaceRef>,
    pub style: UiStyle,
    pub enter_transition: Option<UiTransition>,
    pub world_depth: Option<f32>,
    pub world_scale: Option<f32>,
    pub children: Vec<UiNode>,
}
```

当前 `UiNodeKind` 已声明：

```text
Panel
Label
Button
Image
RenderSurface
TextInput
Checkbox
RadioButton
Slider
DragValue
Combo
Dropdown
Tabs
Tooltip
Modal
Dialog
Selectable
ListBox
Scrollbar
ProgressBar
DataGrid
```

当前 `UiStyle` 只有基础样式：

```rust
pub struct UiStyle {
    pub background_color: [f32; 4],
    pub border_color: [f32; 4],
    pub border_width: f32,
    pub corner_radius: f32,
    pub opacity: f32,
}
```

当前 `UiControlPresentation` 已覆盖：

```rust
Toggle { selected: bool }
Numeric { value: f32, min: f32, max: f32 }
Choice { token: String, options: Vec<String>, selected: bool }
Scroll { position: f32 }
```

当前 `UiTransition` 已覆盖：

```rust
delay_ms
duration_ms
from.bounds
from.background_color
from.border_color
from.border_width
from.corner_radius
from.opacity
from.numeric_value
motion_key
```

### 2.2 当前主要缺口

当前缺口不是 `UiNodeKind` 枚举缺少名字，而是：

1. 组件没有统一的 visual contract。
2. 默认尺寸、最小 padding、文字 inset、track、glyph、popup、hit 区域散落在 renderer 函数中。
3. disabled、hover、pressed、focus、selected 的样式状态没有统一的 state resolver。
4. toggle 只有 domain publication 后才有最终视觉变化，需要 renderer-local optimistic presentation。
5. 普通点击、窗口点击、headless pointer、debug activation 过去有多套 capture 路径。
6. GPU ID readback 不能继续作为窗口生产点击的权威。
7. async host forward 必须有明确的 ordered queue 和 completion wake-up；不能等下一次 RPC 才 drain。
8. fragment revision、input revision、composition revision、interaction sequence 混用时容易造成“点两次才变化”。
9. 组件 chrome、body、text、ID、depth 的 paint group 和 transform 需要一个统一生成入口。
10. animation 目前主要是 `UiTransition` 级别，尚未形成完整的 property/state/motion 生命周期。
11. DataGrid、Template、Popup、WorldUi 的组件级动画和状态样式没有完整验收。

---

## 3. Owner 与边界

### 3.1 `neon-ui-schema`

负责公开、可序列化、跨进程兼容的声明和 presentation contract：

- `UiNodeKind`
- `UiNode`
- `UiLayout`
- `UiStyle`
- `UiControlPresentation`
- `UiTransition`
- `UiEffect`
- typed semantic event / input value

禁止：

- WGPU resource
- glyph atlas handle
- pipeline
- window pointer ID
- GPU hit ID
- 业务规则
- 项目写入

### 3.2 `neon-ui-runtime`

负责：

- Flow parse/compile
- branch visibility
- typed binding
- semantic event validation
- host/domain input revision
- authoritative fragment revision
- state machine motion selection
- publication and fragment submission

禁止：

- 创建窗口
- 创建 WGPU resource
- 计算最终 glyph instance
- 根据像素猜业务状态

### 3.3 `neon-wgpu-runtime`

负责：

- logical layout
- intrinsic text measure
- composed visual
- style resolution
- final transform
- paint bounds / clip bounds / hit bounds
- GPU body/text/image/border/ID/depth instances
- local hover/pressed/focus/toggle prediction
- 当前 composed snapshot 的 O(1) plan/hit 索引
- 统一 pointer capture

禁止：

- 推断 `feature_enabled` 的业务含义
- 直接写 project/domain state
- 通过 UI element ID 跨进程传业务命令

### 3.4 `neon-dev`

负责：

- child process lifecycle
- service health/describe
- scenario runner
- JSON/JSONL artifacts

不负责：

- 修改业务状态
- 绕过公开 RPC
- 自己模拟 renderer 内部 state

### 3.5 硬性规则：窗口版和无窗口版禁止分叉

本计划**不是**让 DeepSeek 写两套组件实现：

```text
错误：
WindowedRuntime::button_hit()
HeadlessExternalGpu::button_hit()

WindowedRuntime::toggle()
HeadlessExternalGpu::toggle()

WindowedRuntime::resolve_style()
HeadlessExternalGpu::resolve_style()
```

这些重复实现禁止合入。

允许存在的差异只有宿主层：

```text
WindowedRuntime
  - winit Window
  - OS surface
  - scale factor
  - WindowEvent -> logical PointerCommand
  - swapchain present
  - IME / keyboard / OS focus

HeadlessExternalGpu
  - external surface/fence
  - RPC pointer input -> logical PointerCommand
  - bounded render loop
  - capture/export target
```

以下内容必须只有一份实现，两个宿主都调用：

```text
logical layout
intrinsic text measurement
component spec/default metrics
style state resolver
UiControlPresentation resolver
WorldUi logical/final transform
paint/hit/clip/depth bounds
plan_index
hit index
pointer capture
local prediction/rollback
semantic event construction
fragment/input/composition revision validation
ordered mutation queue
animation state machine
component chrome/body/text generation
```

最终代码形态必须接近：

```rust
struct UiInteractionCore {
    renderer: UiWgpuRenderer,
    pointer: PointerInteractionState,
    plan_index: UiPlanIndex,
    hit_index: UiHitIndex,
    mutation: UiMutationQueue,
}

impl UiInteractionCore {
    fn dispatch(
        &mut self,
        fragments: &HashMap<UiFragmentId, UiFragment>,
        context: PointerDispatchContext,
        command: PointerCommand,
    ) -> Result<PointerDispatchResult, PointerError>;
}

struct WindowedRuntime {
    core: UiInteractionCore,
    window_host: WindowHostState,
}

struct HeadlessExternalGpu {
    core: UiInteractionCore,
    external_host: ExternalSurfaceHostState,
}
```

如果由于 WGPU resource 生命周期暂时不能把 `UiWgpuRenderer` 直接放进同一个结构体，至少必须抽出无 GPU 依赖的共享核心：

```text
UiComponentSpec
UiStyleResolver
UiLogicalLayoutSnapshot
UiPlanIndex
UiHitIndex
PointerInteractionState
UiMutationQueue
UiAnimationStore
```

两个宿主只能把同一个共享核心的结果交给各自的 target renderer。

### 3.6 parity 验收是硬条件

同一个 fixture、同一组输入、同一组初始 revision，必须分别运行：

```text
headless scenario
window scenario
```

两边必须得到相同的机器结果：

```json
{
  "node_path": "component-gallery/feature-toggle",
  "hit_target": "component-gallery/feature-toggle",
  "event_count": 1,
  "input_revision_delta": 1,
  "fragment_revision_delta": 1,
  "presentation": {"selected": false},
  "status": "accepted"
}
```

允许不同的字段：

```text
physical target size
surface/frame sequence
GPU timing
window/external transport metadata
```

不允许不同的字段：

```text
hit target
event count
semantic intent
requested value
input revision result
fragment revision result
component state
branch visibility
popup ownership
scroll logical bounds
animation status
```

### 3.7 防止 DeepSeek 误施工

DeepSeek 修改任何组件时，必须回答：

```text
1. 这个逻辑属于共享核心还是宿主适配层？
2. headless 和 window 是否调用同一个函数？
3. 是否新增了第二个 hit/capture/revision 路径？
4. 是否有同一 scenario 的 headless/window 对照结果？
5. 失败时是否能用 request_id/interaction_id/sequence/revision 定位？
```

如果答案是“窗口版单独实现”或“无窗口版单独实现”，默认视为架构违规，必须退回重构。

---

## 4. 总施工顺序

必须按以下顺序，不得先堆视觉效果再补 contract：

| 阶段 | 内容 | 结果 |
|---|---|---|
| M0 | 组件缺口盘点与 contract freeze | 每个组件有能力矩阵 |
| M1 | 统一 component spec/defaults/metrics | 默认尺寸和 chrome 不再散落 |
| M2 | 补齐组件 layout/paint/hit | 所有组件先静态正确 |
| M3 | 补齐 state presentation | disabled/hover/pressed/focus/selected 一致 |
| M4 | 合并 window/headless pointer pipeline | 一次点击一条 capture 链 |
| M5 | 补齐 style token/state style | 样式可复用、可验证 |
| M6 | 补齐 animation contract | motion 有 trigger、from、to、生命周期 |
| M7 | 补齐 popup/scroll/DataGrid/Template/WorldUi | 复杂组件完整 |
| M8 | O(1) index 与 frame pairing | 命中不乱序、不串位 |
| M9 | 完整 fixture 回归 | 组件缺失全部暴露 |
| M10 | headless/window/GPU acceptance | 真正完成 |

---

## 5. M0：组件能力矩阵

DeepSeek 必须先建立机器可读矩阵，不能只写人类说明。

建议新增：

```text
crates/neon-wgpu-runtime/tests/fixtures/ui/component_capability_matrix.json
```

结构：

```json
{
  "schema_version": 1,
  "components": [
    {
      "kind": "button",
      "layout": true,
      "paint": true,
      "text": true,
      "hit": true,
      "disabled": true,
      "hover": true,
      "pressed": true,
      "focus": true,
      "selected": false,
      "popup": false,
      "scroll": false,
      "numeric_presentation": false,
      "choice_presentation": false,
      "required_tests": [
        "button_default_size",
        "button_long_text",
        "button_disabled_size_stable",
        "button_pressed_size_stable"
      ]
    }
  ]
}
```

矩阵必须覆盖：

```text
Panel
Label
Button
Image
RenderSurface
TextInput
Checkbox
RadioButton
Slider
DragValue
Combo
Dropdown
Tabs
Tooltip
Modal
Dialog
Selectable
ListBox
Scrollbar
ProgressBar
DataGrid
Template/Repeat
Branch
Scroll container
WorldUi panel
```

M0 完成标准：

- 每个 kind 有一行 capability matrix。
- 每个 true capability 至少有一个测试 ID。
- matrix 中的测试 ID 能映射到 Rust test 或 scenario。
- 没有用“已有代码”代替测试证据。

---

## 6. M1：统一组件视觉 contract

### 6.1 不要继续把默认规则散落在 `match UiNodeKind`

当前 renderer 中存在大量：

```rust
match visual.kind {
    UiNodeKind::Slider => ...,
    UiNodeKind::Checkbox => ...,
    UiNodeKind::Tabs => ...,
}
```

这类 match 不是全部禁止，但必须集中到组件 spec 层。布局、paint、hit、text、chrome 不能各自维护自己的 kind 规则。

### 6.2 建议新增 renderer-local 结构体

这些结构体首先放在 `neon-wgpu-runtime/src/ui_renderer.rs`，不要急于跨进程公开；只有 Flow/IPC 需要声明时才进入 schema。

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
struct UiComponentMetrics {
    min_width: f32,
    min_height: f32,
    horizontal_padding: f32,
    vertical_padding: f32,
    text_inset_left: f32,
    text_inset_right: f32,
    control_glyph_width: f32,
    control_glyph_gap: f32,
    label_track_ratio: f32,
    track_height: f32,
    popup_gap: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiComponentCapabilities {
    has_text: bool,
    is_interactive: bool,
    has_numeric_value: bool,
    has_choice_value: bool,
    has_toggle_value: bool,
    opens_popup: bool,
    scrolls: bool,
    top_layer: bool,
    virtualized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiComponentSpec {
    kind: UiNodeKind,
    metrics: UiComponentMetrics,
    capabilities: UiComponentCapabilities,
}
```

### 6.3 spec 访问入口

```rust
fn component_spec(kind: &UiNodeKind) -> UiComponentSpec;
```

要求：

1. 入口必须是纯函数。
2. 不读 pointer position。
3. 不读 domain state。
4. 不产生 GPU resource。
5. 不改变 bounds。
6. 所有默认尺寸集中在这里。
7. 组件特殊尺寸必须有测试。

### 6.4 不要把业务 presentation 写入 metrics

错误：

```rust
if feature_enabled { height = 40.0; }
```

正确：

```rust
metrics = component_spec(&kind).metrics;
presentation = UiControlPresentation::Toggle { selected };
style = resolve_style(metrics, presentation, local_state);
```

checked/selected/value 只进入 presentation/style，不改变 layout track。

---

## 7. M2：组件静态功能补齐

施工顺序：先让组件在没有 hover/动画时正确显示，再接状态。

### 7.1 基础容器

#### Panel

必须支持：

- background
- border
- border width
- radius
- opacity
- padding
- row/column/overlay/absolute child layout
- clip policy
- scroll viewport
- parent clip inheritance
- paint group/depth inheritance

验收：

- body 与 border 使用同一 bounds。
- child 不因为 panel border 重复偏移。
- `clip none` 不裁 child。
- `clip bounds` 只裁 paint/hit，不改变 logical child bounds。
- `clip scroll` viewport 固定，content extent 独立计算。

#### Label

必须支持：

- ASCII/CJK/emoji
- explicit newline
- automatic wrap
- font fallback
- loaded font relayout
- text safe inset
- inherited WorldUi scale
- current visual clip

禁止：

- 从 bounds.height 反推 font size。
- 把滚动后 text instance 当成静态缓存。
- 用文本视觉坐标作为 logical cache key。

#### Image

必须支持：

- AssetRef
- intrinsic source size
- authored bounds
- tint/opacity
- parent clip
- 2x logical/physical mapping
- WorldUi transform

### 7.2 交互基础组件

#### Button

默认：

```text
height >= 30 logical px
horizontal padding >= 10 logical px
hover/pressed 不改变 layout size
text uses Button text inset
```

必须支持：

- hit bounds
- focus
- hover style
- pressed style
- disabled style
- semantic intent
- optimistic pressed feedback
- authoritative post-click state

#### TextInput

必须支持：

- focus
- caret
- selection
- IME preedit
- committed text
- horizontal scroll
- current clip
- text input binding
- commit semantic event

注意：

- TextInput 的 caret/selection 是 renderer-local presentation state。
- commit 后才进入 UI/domain protocol。
- text input 的横向滚动不能污染普通 scroll container 的 offset。

#### Checkbox

结构：

```text
row bounds
  glyph fixed box
  gap
  label remaining track
```

必须支持：

- checked false/true
- disabled
- hover
- pressed
- focus
- one-click optimistic toggle
- domain rejection rollback
- checked 不改变 row height

#### RadioButton

与 Checkbox 共用 toggle visual pipeline，但 presentation contract 独立。

必须保证：

- radio glyph 与 checkbox glyph metrics 不混用。
- `selected` 不是 `active` 的别名。
- 不由 renderer 猜哪个 radio 属于同一组。
- group/domain 规则由 domain 或 typed input 提供。

#### Selectable

必须支持：

- selected
- hover
- pressed
- focus
- disabled
- label track
- one-click optimistic selection

### 7.3 数值组件

#### ProgressBar

组件模型：

```rust
struct ProgressVisualState {
    normalized_value: f32,
    track_bounds: UiBounds,
    fill_bounds: UiBounds,
    clip: UiBounds,
}
```

规则：

- track bounds 固定。
- fill width = `track_width * normalized_value`。
- value 不能改 parent layout。
- label 文本不能改变 fill track。
- disabled 只改变颜色/opacity。

#### Slider

组件模型：

```rust
struct SliderVisualState {
    label_bounds: UiBounds,
    track_bounds: UiBounds,
    thumb_bounds: UiBounds,
    normalized_value: f32,
    clip: UiBounds,
}
```

规则：

- label 区和 track 区固定比例。
- thumb 不参与 parent track。
- pointer drag 只更新 local numeric preview。
- release 只发送一次 semantic commit。
- domain publication 返回后清除对应 pending preview。

#### DragValue

与 Slider 共用 numeric gesture，但显示格式和步进可以不同：

- `i32` 使用整数 rounding。
- min/max 必须来自 typed presentation。
- drag preview 不发送每个 pointer move 的可靠 RPC。
- release/commit 才发送语义事件。

### 7.4 选择组件

#### Combo / Dropdown

必须拆成两个 visual 层：

```text
control body
popup top layer
```

popup 结构：

```rust
struct UiPopupVisual {
    owner_path: String,
    popup_bounds: UiBounds,
    option_bounds: Vec<UiBounds>,
    clip: UiBounds,
    top_layer: bool,
    opened: bool,
}
```

规则：

- popup 不改变 parent track。
- popup 有独立 clip。
- popup hit 优先于普通 sibling。
- outside click 关闭 popup，不误触下方 control。
- option click 只产生一次 selection event。
- open/close 是 renderer-local state；选中值由 domain authoritative publication 决定。

#### Tabs

必须区分：

```text
tab control presentation
active tab state
tab body branch visibility
```

规则：

- tab chrome 不改变 body layout。
- active tab 的 branch 才参与 layout。
- inactive branch 不进入 intrinsic measurement。
- tab click 的 selected feedback 可以本地立即显示。
- domain/state machine 返回后更新 active branch。

#### ListBox

必须支持：

- option row fixed height
- selected option
- scroll viewport
- option hit bounds
- popup/scroll clip
- text cache per option row

禁止：

- 当前 option 文本随机改变整个 ListBox width。
- 每个 option 产生独立不可追踪的 business command。

### 7.5 Scrollbar

组件模型：

```rust
struct ScrollbarVisualState {
    axis: ScrollAxis,
    track_bounds: UiBounds,
    thumb_bounds: UiBounds,
    normalized_position: f32,
    max_offset: f32,
}
```

规则：

- track 固定。
- thumb 不影响 layout。
- drag 只更新 scroll local state。
- release/settled 状态再发送 domain semantic event（如果该 scrollbar 有绑定）。
- scroll content 和 scrollbar chrome 不互相重复应用 offset。

### 7.6 Top layer

#### Tooltip

- hover/focus 触发 renderer-local visible。
- tooltip 不进入普通 parent track。
- tooltip 使用独立 clip。
- tooltip pointer policy 默认不阻塞 underlying control，除非显式声明。
- tooltip text 使用当前 transform，不复用旧屏幕坐标。

#### Modal / Dialog

- top layer。
- backdrop 由 renderer 生成。
- pointer blocking 明确：modal 外部点击被消费，不能落到 background control。
- dialog body 使用普通 row/column layout。
- dialog popup/transition 不改变普通 parent track。
- close button 仍通过 typed semantic intent。

### 7.7 DataGrid

必须拆成：

```text
grid viewport
sticky header
virtual row window
cell visual
horizontal scroll
vertical scroll
cell hit binding
```

建议结构体：

```rust
struct DataGridLayoutState {
    viewport: UiBounds,
    header: UiBounds,
    body: UiBounds,
    content_extent: [f32; 2],
    row_height: f32,
    column_bounds: Vec<UiBounds>,
    first_row: u64,
    visible_rows: u32,
}

struct DataGridCellVisual {
    identity: DataGridCellIdentity,
    bounds: UiBounds,
    clip: UiBounds,
    kind: UiNodeKind,
    presentation: Option<UiControlPresentation>,
}
```

规则：

- row height 只来自 declaration。
- column width 只来自 declaration，不由当前文本随机扩展。
- virtual rows 只来自 bounded frame。
- sticky header 不随 vertical scroll 移动。
- body cells 的 scroll transform 和 clip 一次完成。
- cell identity 使用 stable row key + column key。
- cell hit binding 使用 O(1) identity map。

### 7.8 Template/Repeat

必须支持：

- prototype 只编译/测量一次。
- stable row key。
- instance path 可逆解析。
- instance 不重复创建 font/layout resource。
- instance 视觉状态不能覆盖另一个 row。
- accepted drop 的 instance 必须与 hidden prototype 区分。

---

## 8. M3：统一状态样式系统

### 8.1 当前问题

当前基础 `UiStyle` 只描述一个最终样式，不能表达：

```text
normal
hover
pressed
focused
disabled
selected
checked
open
invalid
```

不要把这些状态直接写成业务状态。它们属于 renderer-local presentation state 或 domain-provided presentation。

### 8.2 建议 schema 结构

第一版可以先放 renderer-local；当 Flow 需要声明主题时再公开到 `neon-ui-schema`。

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum UiVisualState {
    Normal,
    Hover,
    Pressed,
    Focused,
    Disabled,
    Selected,
    Checked,
    Open,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiResolvedStyle {
    fill: [f32; 4],
    border: [f32; 4],
    text: [f32; 4],
    border_width: f32,
    radius: f32,
    opacity: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct UiStyleStateSet {
    normal: UiStyle,
    hover: Option<UiStylePatch>,
    pressed: Option<UiStylePatch>,
    focused: Option<UiStylePatch>,
    disabled: Option<UiStylePatch>,
    selected: Option<UiStylePatch>,
    checked: Option<UiStylePatch>,
    open: Option<UiStylePatch>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UiStylePatch {
    background_color: Option<[f32; 4]>,
    border_color: Option<[f32; 4]>,
    text_color: Option<[f32; 4]>,
    border_width: Option<f32>,
    corner_radius: Option<f32>,
    opacity: Option<f32>,
}
```

### 8.3 样式 resolve 顺序

必须固定顺序：

```text
base component default
  -> authored UiStyle
  -> disabled patch
  -> selected/checked patch
  -> focus patch
  -> hover patch
  -> pressed patch
  -> local optimistic patch
  -> opacity/inherited opacity
```

优先级必须写成测试，不允许由 HashMap 或调用顺序决定。

建议：disabled 最高优先级，pressed 不能覆盖 disabled。

```rust
fn resolve_component_style(
    spec: UiComponentSpec,
    authored: UiStyle,
    state: UiStateFlags,
    presentation: Option<&UiControlPresentation>,
    styles: &UiStyleStateSet,
) -> UiResolvedStyle;
```

### 8.4 状态来源

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UiStateFlags {
    hovered: bool,
    pressed: bool,
    focused: bool,
    disabled: bool,
    selected: bool,
    checked: bool,
    open: bool,
}
```

来源规则：

- `hovered/pressed/focused`：WGPU renderer local。
- `disabled`：UiNode enabled 的反值，来自 UI/domain snapshot。
- `selected/checked/open`：domain presentation + renderer local prediction 合并。
- renderer 不能直接把 `feature_enabled` 当成 checked，必须使用绑定后的 presentation。

### 8.5 样式不参与 layout

以下变化默认不能让 `resolve_children()` 重新计算 track：

```text
fill
border color
text color
opacity
border width
corner radius
hover
pressed
focused
checked
selected
```

例外：如果明确声明 `border_width` 参与 paint bounds，而不是 layout bounds，也不能改变 layout box；必须扩展 paint bounds/clip policy，而不是重新排版 sibling。

---

## 9. M4：统一 window/headless interaction pipeline

### 9.1 目标结构

窗口与无窗口都必须使用同一个内部结构：

```rust
#[derive(Clone, Debug)]
struct PointerInteractionState {
    input: LocalInputState,
    captured_binding: Option<UiHitBinding>,
    pending_control_value: Option<UiSemanticPayloadValue>,
    next_semantic_sequence: u64,
    active_interaction_id: Option<InteractionId>,
}
```

建议抽到：

```text
crates/neon-wgpu-runtime/src/interaction.rs
```

如果暂时不拆文件，至少在 `lib.rs` 形成独立 impl。

### 9.2 统一入口

```rust
enum PointerSource {
    Window,
    External,
    Debug,
}

struct PointerDispatchContext {
    source: PointerSource,
    epoch: u64,
    generation: u64,
    composition_revision: Revision,
    logical_viewport: [f32; 2],
    physical_viewport: [u32; 2],
}

enum PointerCommand {
    Move { logical: [f32; 2] },
    Down { logical: [f32; 2], button: UiPointerButton },
    Up { logical: [f32; 2], button: UiPointerButton },
    Wheel { logical: [f32; 2], delta: [f32; 2] },
    Cancel,
}
```

统一方法：

```rust
fn dispatch_pointer_command(
    renderer: &mut UiWgpuRenderer,
    interaction: &mut PointerInteractionState,
    fragments: &HashMap<UiFragmentId, UiFragment>,
    context: PointerDispatchContext,
    command: PointerCommand,
) -> Result<PointerDispatchResult, PointerError>;
```

这个函数必须完成：

1. 设置当前 logical pointer。
2. `prepare_interaction()`。
3. 当前 plan/sampled 的 O(1) binding resolve。
4. capture。
5. toggle/numeric/choice local prediction。
6. release 时只生成一次 `UiSemanticEvent`。
7. 统一递增 interaction sequence。
8. 统一校验 epoch/generation/composition revision。

窗口路径只做：

```text
WindowEvent -> logical pointer -> PointerCommand
```

无窗口路径只做：

```text
RPC UiPointerEvent -> PointerCommand
```

禁止窗口和 headless 各自拥有另一套 capture。

### 9.3 Window MouseInput 必须使用最近 logical position

`WindowEvent::MouseInput` 本身没有可靠的坐标字段，必须依赖：

```rust
struct WindowPointerState {
    last_physical: [f64; 2],
    last_logical: [f32; 2],
    last_move_sequence: u64,
}
```

`CursorMoved` 更新它；`MouseInput Down/Up` 使用同一份 `last_logical`，不能重新读取旧 renderer state，也不能把物理像素直接当逻辑坐标。

### 9.4 GPU ID 的权限

生产点击不能依赖异步 GPU readback。

允许：

- debug diagnostics
- ID pass 验收
- hover observation
- GPU coordinate probe

禁止：

- readback completion 修改 capture。
- readback completion 覆盖当前 hovered/captured binding。
- 一个 frame 的 ID 使用另一个 frame 的 binding map。

---

## 10. M5：O(1) 交互索引

### 10.1 Renderer plan index

```rust
struct UiPlanIndex {
    by_path: HashMap<String, usize>,
    by_node_key: HashMap<(String, String), usize>,
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
}
```

维护时机：

- `refresh_plan()` 完成后一次建立。
- fragment revision/topology 改变时重建。
- camera distance、scroll offset、hover、pressed、animation frame 不重建。

### 10.2 Hit index

```rust
struct UiHitIndex {
    by_hit_id: HashMap<u32, UiHitBinding>,
    by_path: HashMap<String, u32>,
    ordered: Vec<u32>,
}
```

规则：

- `hit_id -> binding` O(1)。
- `node_path -> hit_id` O(1)。
- topmost 顺序由 `ordered` 按 plan paint order 固定，不依赖 HashMap iteration。
- `HashMap.values().find(...)` 禁止出现在 focus/hit 生产路径。

### 10.3 复杂空间查询

第一版 UI 节点数量较少时，可以使用有序 plan 从 top 到 bottom 做 O(n) spatial fallback，但必须：

- 只作为没有 GPU ID / debug fallback。
- 生产点击优先使用当前 composed hit binding。
- 不能把 HashMap iteration 当排序依据。

如果组件 gallery 规模继续增长，再增加：

```rust
struct UiSpatialIndex {
    cells: HashMap<(i32, i32), Vec<usize>>,
    cell_size: f32,
}
```

但 spatial index 只能缩小候选，最终仍按 paint order 逆序判断 clip/hit。

---

## 11. M6：动画 contract

### 11.1 动画与业务分离

动画分为三类：

```text
renderer-local interaction animation
  hover / pressed / focus / popup open / scroll thumb

UI presentation transition
  bounds / opacity / color / numeric value / selected style

domain/state-machine motion
  state transition chooses motion_key; UI runtime publishes transition declaration
```

renderer 不决定业务状态；domain 不创建 GPU animation object。

### 11.2 建议结构体

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAnimationProperty {
    Position,
    Size,
    Opacity,
    BackgroundColor,
    BorderColor,
    BorderWidth,
    CornerRadius,
    NumericValue,
}

#[derive(Clone, Debug, PartialEq)]
struct UiAnimationSpec {
    key: String,
    properties: Vec<UiAnimationProperty>,
    delay_ms: u32,
    duration_ms: u32,
    easing: UiEasing,
    from: UiTransitionState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAnimationStatus {
    Pending,
    Running,
    Completed,
    Cancelled,
    Superseded,
    Rejected,
}

#[derive(Clone, Debug, PartialEq)]
struct UiAnimationInstance {
    node_path: String,
    interaction_id: Option<String>,
    fragment_revision: Revision,
    started_at_seconds: f32,
    spec: UiAnimationSpec,
    status: UiAnimationStatus,
}
```

### 11.3 动画生命周期

```text
CommandReceived
  -> CommandValidated
  -> LocalPredictionApplied (optional)
  -> DomainAccepted / DomainRejected
  -> PublicationApplied
  -> FragmentSubmitted
  -> AnimationStarted
  -> AnimationCompleted / Superseded / Cancelled
```

每条动画必须携带：

```text
request_id
interaction_id
node_path
fragment_id
fragment_revision
motion_key
```

### 11.4 retarget 规则

如果同一 node 新 target 在旧动画结束前到达：

1. 从当前 sampled visual 取 from。
2. 不从旧 target 取 from。
3. 旧动画标记 `Superseded`。
4. 新动画只保留一个 active instance。
5. 不重复 parse/compile Flow。
6. 不生成重复 child topology。

### 11.5 GPU 动画与 CPU layout

GPU 可以插值：

- rect
- fill
- border
- params
- numeric fill/thumb

GPU 不能重新决定：

- child track
- text wrapping
- topology
- branch visibility
- popup ownership

如果 size/width 真的改变，先生成新的 logical layout revision，再让 GPU 做 visual interpolation；不能用 GPU animation 的视觉 rect 反向喂给 layout。

---

## 12. M7：组件动画明细

| 组件 | local animation | authoritative animation | 禁止 |
|---|---|---|---|
| Button | hover/pressed/focus | optional enter/exit | pressed 改 layout height |
| TextInput | caret blink/selection | commit feedback | 每个 keypress 跨进程 layout request |
| Checkbox | pressed/checked color | Toggle publication | checked 改 row height |
| RadioButton | pressed/selected color | Selection publication | renderer 决定 group business state |
| Slider | thumb drag/value preview | release numeric commit | 每帧 RPC |
| DragValue | value preview | release commit | numeric text 触发 parent relayout |
| Combo | hover/open | selected option publication | popup 改 parent track |
| Dropdown | open/close/option hover | selected option publication | popup 与 body 共用错误 clip |
| Tabs | active indicator | machine/domain state | inactive branch 占 layout |
| Selectable | hover/pressed/selected | selected publication | HashMap 决定选中节点 |
| ListBox | option hover/scroll | selected publication | 当前 option 宽度扩展 container |
| Scrollbar | thumb drag | optional scroll publication | scroll offset 重复应用 |
| ProgressBar | numeric fill | authoritative numeric publication | fill 改 parent layout |
| DataGrid | row/cell hover/edit | cell commit/window publication | virtual rows全量进入 topology |
| Tooltip | show/hide delay | none | tooltip 阻塞普通 pointer |
| Modal/Dialog | enter/exit/backdrop | visible binding | backdrop 与 body 分离 depth |
| WorldUi | root transform | anchor/depth publication | 每个 child 单独 scale |

---

## 13. M8：revision 与 command queue

### 13.1 revision 分层

不能把不同 revision 混用：

```text
program_revision
  Flow/program schema identity

input_revision
  domain input snapshot

fragment_revision
  declarative UI tree/presentation snapshot

composition_revision
  WGPU renderer accepted composition graph

interaction_sequence
  one pointer interaction ordering

renderer_epoch
  process restart boundary
```

### 13.2 一个点击的正确 command record

```rust
struct UiCommandEnvelope {
    request_id: RequestId,
    idempotency_key: String,
    interaction_id: String,
    sequence: u64,
    renderer_epoch: u64,
    program_revision: Revision,
    input_revision: Revision,
    fragment_revision: Revision,
    composition_revision: Revision,
    source_node_path: String,
    intent: UiIntent,
    requested_value: Option<UiSemanticPayloadValue>,
}
```

### 13.3 ordered queue

每个 UI surface 只允许一个 mutation lane：

```rust
struct UiMutationQueue {
    next_sequence: u64,
    in_flight: Option<UiCommandEnvelope>,
    queued: VecDeque<UiCommandEnvelope>,
    last_input_revision: Revision,
    last_fragment_revision: Revision,
}
```

规则：

- 前一个 domain publication 未完成时，后续 command 不能基于旧 input revision 直接发出。
- 后续命令可以先做 renderer-local prediction，但必须在 queue 里等待 authoritative input revision。
- 不要用“fragment revision rebase”隐藏真正的 input revision 冲突。
- 如果新命令 supersede 同一个 node 的旧预测，必须显式记录 `Superseded`。
- 不能让两个异步 worker 并发写同一个 UI surface。

### 13.4 serve loop

错误模型：

```text
request -> pending queue -> 等下一次 RPC 才启动 worker
```

正确模型：

```text
request -> queue -> 当前 tick 立即启动 ordered worker
worker completion -> wake service loop -> apply publication
```

如果 transport 没有 wake-up 机制，先使用同步 forward；不能用“下一次用户点击”作为 completion pump。

---

## 14. M9：完整 fixture 回归矩阵

必须使用：

```text
imgui-component-gallery.nui
```

每个页面/组件必须做：

```text
initial render
disabled render
hover render
pressed render
selected/checked render
one-click interaction
repeated interaction
resize
1x/2x
scroll ancestor
popup/top-layer
branch switch
font fallback
font loaded
```

### 14.1 每项输出

```json
{
  "scenario": "gallery.feature-toggle.single-click.v1",
  "status": "passed",
  "component": "checkbox",
  "node_path": "component-gallery/feature-toggle",
  "input": {
    "pointer_logical": [220.0, 410.0],
    "pointer_physical": [440.0, 820.0]
  },
  "frames": {
    "down": 101,
    "prediction": 102,
    "publication": 103,
    "final": 104
  },
  "revisions": {
    "input_before": 7,
    "input_after": 8,
    "fragment_before": 12,
    "fragment_after": 13,
    "composition_before": 20,
    "composition_after": 21
  },
  "hit": {
    "source": "composed_cpu",
    "node_path": "component-gallery/feature-toggle",
    "binding_found": true
  },
  "prediction": {
    "before": true,
    "after": false,
    "rolled_back": false
  }
}
```

### 14.2 必须新增 scenario

```text
gallery.button.single-click.v1
gallery.feature-toggle.single-click.v1
gallery.radio.single-click.v1
gallery.slider.single-drag.v1
gallery.drag-value.single-drag.v1
gallery.combo.single-select.v1
gallery.dropdown.single-select.v1
gallery.tabs.single-select.v1
gallery.selectable.single-click.v1
gallery.list-box.single-select.v1
gallery.scrollbar.single-drag.v1
gallery.data-grid.cell-edit.v1
gallery.data-grid.tail-row.v1
gallery.dialog.open-close.v1
gallery.branch.switch.v1
gallery.scroll.panel-text.v1
gallery.world-ui.transform.v1
```

### 14.3 每个 scenario 的硬断言

- 一次 Down + Up 只能产生一个 semantic event。
- 一次 semantic event 只能产生一个 domain mutation。
- 一次 accepted mutation 只能产生一个 fragment revision increment。
- 一次 fragment submission 只能产生一个 composition revision increment。
- source node path 必须稳定。
- A 点击不能改变 B 的 presentation。
- disabled control 不能产生 semantic event。
- popup option 不能同时触发 popup owner。
- scroll 后 sibling 的 logical gap 不变。
- 下半区坐标命中不能被 viewport height 截断。

---

## 15. M10：测试分层

### 15.1 Contract tests

位置：

```text
crates/neon-ui-schema/src/lib.rs tests
crates/neon-ui-runtime/src/lib.rs tests
```

覆盖：

- serde roundtrip
- deny_unknown_fields
- default/compatibility
- UiControlPresentation
- UiTransition
- UiStyleStateSet
- revision/idempotency

### 15.2 Service tests

覆盖：

- one click -> one accepted event
- stale input revision rejected with stable code
- duplicate idempotency returns same response
- queued command order
- publication fragment revision monotonic

### 15.3 Renderer tests

位置：

```text
crates/neon-wgpu-runtime/src/ui_renderer.rs #[cfg(test)]
```

覆盖：

- every component spec exists
- default metrics
- state style priority
- disabled size stable
- pressed size stable
- topmost hit order
- O(1) path/hit maps
- toggle prediction/rollback
- scroll text current rect
- popup independent layout
- branch inactive no track
- DataGrid stable cell identity

### 15.4 GPU tests

覆盖：

- 1x logical/physical
- 2x physical / 1x logical
- ID coordinate mapping
- ID frame/binding pairing
- body/text/border clip same
- depth paint group same
- near/far panel occlusion
- popup top layer

### 15.5 Executable probe

必须增加或扩展：

```text
crates/neon-wgpu-runtime/src/bin/ui_component_style_animation_probe.rs
```

probe 要求：

- 固定 fixture
- 固定 viewport
- 固定 pointer sequence
- explicit timeout
- bounded polling
- JSONL output
- failure exit code != 0
- 输出 request_id、interaction_id、sequence、frame、revision、node_path、input/output

建议 callback：

```jsonl
{"event":"scenario_started","scenario":"gallery.feature-toggle.single-click.v1","sequence":1}
{"event":"pointer_down","logical":[220,410],"physical":[440,820],"frame":100}
{"event":"hit_resolved","source":"composed_cpu","node_path":"component-gallery/feature-toggle","hit_id":17,"frame":100}
{"event":"local_prediction","node_path":"component-gallery/feature-toggle","value":false,"frame":101}
{"event":"semantic_forwarded","request_id":"...","interaction_id":"...","sequence":1}
{"event":"publication_applied","input_revision":8,"fragment_revision":13,"composition_revision":21}
{"event":"scenario_finished","status":"passed"}
```

---

## 16. 组件默认 spec 表

DeepSeek 施工时必须把默认值集中实现，下面是最低要求，不是可选建议：

| 组件 | min height | text inset | 默认 clip | 默认 hit | popup/top layer |
|---|---:|---:|---|---|---|
| Panel | 0 | 0 | bounds | paint | 否 |
| Label | line height | 8 | inherited | paint | 否 |
| Button | 30 | 10 | bounds | paint | 否 |
| Image | 0 | 0 | bounds | paint | 否 |
| RenderSurface | 0 | 0 | bounds | paint | 否 |
| TextInput | 30 | 12 | bounds | paint | 否 |
| Checkbox | 30 | glyph + 8 | bounds | row | 否 |
| RadioButton | 30 | glyph + 8 | bounds | row | 否 |
| Slider | 30 | 8 | bounds | track | 否 |
| DragValue | 30 | 8 | bounds | track | 否 |
| Combo | 32 | 8 | bounds | body | popup |
| Dropdown | 32 | 8 | bounds | body | popup |
| Tabs | 32 | 8 | bounds | segment | 否 |
| Tooltip | auto | 8 | independent | optional | top layer |
| Modal | auto | 8 | independent | blocking | top layer |
| Dialog | auto | 8 | independent | blocking | top layer |
| Selectable | 30 | glyph + 8 | bounds | row | 否 |
| ListBox | 90 | 8 | scroll | row | 否 |
| Scrollbar | 20/20 | 0 | viewport | thumb | 否 |
| ProgressBar | 24 | 8 | bounds | optional | 否 |
| DataGrid | declaration | cell | scroll | cell | 否 |

如果现有 fixture 或视觉设计需要不同值，必须通过明确的 style/layout declaration 覆盖，不能静默修改 default spec。

---

## 17. 常见错误模型，禁止合入

### 17.1 点击错误

```text
GPU readback completion -> overwrite current capture
HashMap.values().find -> focus target
Window MouseInput -> use stale pointer position
async queue -> wait next RPC to drain
same node pending prediction -> silently overwrite older command
fragment revision rebase -> hide input revision conflict
```

### 17.2 组件错误

```text
hover changes bounds
pressed changes track size
checked changes row height
popup participates in parent layout
text width changes DataGrid column width
virtual rows expand full topology
inactive branch enters intrinsic measurement
```

### 17.3 样式错误

```text
style state stored as domain business state
disabled style only changes color but remains hittable
border uses another paint group
text uses another world scale
focus style determined by random map order
opacity animation bypasses clip/depth
```

### 17.4 动画错误

```text
camera distance triggers text layout
animation writes project/domain state
every pointer move becomes RPC
animation target changes child topology
new target samples from old target instead of current visual
fragment revision increment is used as proof that every interaction completed
```

---

## 18. 施工检查清单

### Phase A：基础 contract

- [ ] capability matrix 已建立。
- [ ] component spec 已建立。
- [ ] metrics/defaults 已集中。
- [ ] style state priority 已固定。
- [ ] O(1) plan/hit index 已建立。

### Phase B：组件静态能力

- [ ] Panel/Label/Button/Image/RenderSurface。
- [ ] TextInput。
- [ ] Checkbox/RadioButton/Selectable。
- [ ] Slider/DragValue/ProgressBar/Scrollbar。
- [ ] Combo/Dropdown/Tabs/ListBox。
- [ ] Tooltip/Modal/Dialog。
- [ ] DataGrid/Template/Repeat。
- [ ] Branch/Scroll/WorldUi。

### Phase C：交互一致性

- [ ] window/headless 使用同一 PointerCommand。
- [ ] 一次点击只有一个 capture。
- [ ] 一次点击只有一个 semantic event。
- [ ] GPU readback 不覆盖 capture。
- [ ] 下半区坐标可命中。
- [ ] 2x coordinate mapping 通过。
- [ ] A/B 不串位。
- [ ] feature-toggle 一次点击立即有 prediction。
- [ ] domain rejection 能 rollback。

### Phase D：动画

- [ ] hover/pressed/focus local animation。
- [ ] toggle/selection presentation animation。
- [ ] numeric/scroll preview animation。
- [ ] popup open/close。
- [ ] modal/dialog enter/exit。
- [ ] branch switch。
- [ ] WorldUi root transform。
- [ ] retarget/supersede/cancel。
- [ ] animation trace。

### Phase E：验收

- [ ] focused tests。
- [ ] service scenarios。
- [ ] GPU probes。
- [ ] component gallery headless。
- [ ] component gallery window。
- [ ] 1x。
- [ ] 2x。
- [ ] resize。
- [ ] scroll。
- [ ] lower-half click。
- [ ] package tests。
- [ ] workspace tests。

---

## 19. 推荐命令

先编译实际被 supervisor 启动的 binary：

```powershell
cargo build -p neon-ui-runtime --bins
cargo build -p neon-wgpu-runtime --bins
cargo build -p neon-dev
```

启动真实窗口组件 gallery：

```powershell
cargo run -p neon-dev -- case component-gallery --show-logs
```

headless component gallery scenario：

```powershell
cargo run -p neon-dev -- scenario component-gallery-interactions
```

window component gallery scenario：

```powershell
cargo run -p neon-dev -- scenario component-gallery-window-input
```

focused renderer tests：

```powershell
cargo test -p neon-wgpu-runtime --lib
```

UI runtime tests：

```powershell
cargo test -p neon-ui-runtime
```

workspace tests：

```powershell
cargo test --workspace
```

probe：

```powershell
cargo run -p neon-wgpu-runtime --bin ui-component-style-animation-probe -- --scenario gallery.feature-toggle.single-click.v1
```

---

## 20. 完成报告模板

DeepSeek 完工时必须填写：

```text
修改文件：
- ...

新增结构体：
- ...

组件完成：
- Panel: pass/fail
- Label: pass/fail
- ...

样式状态完成：
- disabled: pass/fail
- hover: pass/fail
- pressed: pass/fail
- focus: pass/fail
- selected/checked: pass/fail

动画完成：
- local interaction: pass/fail
- presentation transition: pass/fail
- state motion: pass/fail
- retarget/supersede: pass/fail

window/headless 一致性：
- one-click feature-toggle: pass/fail
- lower-half pointer: pass/fail
- A/B hit order: pass/fail
- 2x mapping: pass/fail

测试：
- focused tests: command + result
- service scenarios: command + JSON result
- GPU probe: command + JSONL result
- package tests: command + result
- workspace tests: command + result

性能：
- layout_count:
- text_layout_count:
- world_transform_update_count:
- color_pass_ms:
- id_pass_ms:
- dropped_frames:

warnings：
- ...

failures：
- ...

未完成：
- ...
```

不能只写“窗口能打开”或“测试编译通过”。必须证明：

```text
同一份 Flow
同一套组件 contract
同一套 style resolver
同一套 pointer/capture/semantic pipeline
在 headless 和 window 两种启动方式下得到相同的行为结果。
```

---

## 21. 最终完成条件

只有同时满足以下条件，才能宣布本计划完成：

1. component capability matrix 全部通过。
2. fixture 中所有组件都有静态 layout/paint/hit 验收。
3. 所有交互组件一次点击或一次手势完成，不依赖第二次输入。
4. feature-toggle、radio、selectable 的 prediction/rollback/authoritative 状态一致。
5. A/B 重叠控件命中稳定，不能串位。
6. 窗口下半区和 2x backing 坐标都能正确命中。
7. popup、modal、dialog、scroll、DataGrid、Template 不破坏 sibling layout。
8. camera drag 期间 `text_layout_count` 不增加。
9. scroll text 不复用旧屏幕坐标。
10. GPU ID 仅作诊断/验收，不覆盖生产 capture。
11. headless scenario 通过。
12. window scenario 通过。
13. focused/package/workspace tests 通过，warnings 与 failures 分开报告。
14. JSONL probe 输出真实 request/frame/revision/intermediate/final pass/fail 数据。

如果其中任意一项没有真实证据，只能标记为 `in_progress`，不能标记为 `completed`。
