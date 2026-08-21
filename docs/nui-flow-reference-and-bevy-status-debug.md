# NUI Flow Reference And Bevy Status Debug

调查范围：

- `D:\Neon3\plan\neon3-nui-flow.md`
- `D:\Neon3\docs\nui-flow-ai-authoring.md`
- `D:\Neon3\crates\neon-ui-runtime\src\nui_flow.rs`
- `D:\Neon3\crates\neon-ui-schema\src\lib.rs`
- `D:\Neon3\tests\fixtures\ui\*.nui`
- `D:\bevy-nui-host\src\main.rs`
- `D:\bevy-nui-host\src\lib.rs`

## 结论

1. 当前 screen UI 点击事件已经进入 Bevy：

   ```text
   [bevy-ui] screen intent=character.status.toggle source=Some("status-action")
   ```

2. UI frame 曾经在第一次提交后成功，后续 frame 因 host 没有读取正确的 accepted revision 而持续 stale。`UiHostPublicationResult` 的 revision 在以下路径：

   ```text
   result.snapshot.scalar_inputs.input_revision
   ```

   host 原先只读取 `result.input_revision` 和 `result.accepted_input_revision`，没有推进本地 revision。已在 `D:\bevy-nui-host\src\lib.rs` 增加嵌套路径读取。

3. `D:\bevy-nui-host\assets\ui\ordinary-status.nui` 当前没有被 main flow 使用。实际启动时 `main.rs::monster_field_flow()` 动态生成并提交 `combined-ui` source。调试时必须以这个动态 source 为准。

4. 当前 NUI source 只声明了 `progress_bar` 数值绑定和 semantic event。它没有声明状态机、branch 或任何动画 transition，因此不能从 NUI 本身产生 health tween。

5. NUI Flow V1 没有通用的 `animation`、`transition`、`duration`、`ease` 或脚本回调语法。已有 schema 的 `UiTransition` 是 IR/runtime 层能力，不等于当前 Flow 文本支持这些语法。

## 当前实际 Source

实际提交源在 `D:\bevy-nui-host\src\main.rs` 的 `monster_field_flow()`：

```text
flow combined-ui
budget nodes=512 bindings=512 instances=512 text=512 glyphs=32768 events=512 clips=512
input health f32:0..100 default 82
input mana f32:0..100 default 64
input level u32:0..100 default 12
surface combined-ui
  panel status-root x 32 y 32 w 320 h 140 column pad 8 gap 6 fill #244A62 line #8ED6F0 event character.open_status
    panel status-header row w 304 h 28 gap 8 fill #183445
      text avatar value "A"
      text name value "Astra"
      text level value "Lv 12"
    panel bars row w 304 h 48 gap 8 fill #183445
      progress_bar health_bar numeric $health
      progress_bar mana_bar numeric $mana
    text status-effect value "Screen UI baseline"
    button status-action h 24 value "Toggle status" event character.status.toggle
```

后面还会追加每个 monster 的 `healthN`, `manaN`, `levelN` input 和 `world panel`。因此 screen 和 world UI 共用一个 program 和一个 scalar input frame。

`ordinary-status.nui` 的语法本身基本合法，但它使用 `surface ordinary-ui`，只声明 `health` 和 `mana`，没有 `flow`，也没有 world inputs。它不是当前运行实例的 source。`character-status.nui` 同样不是 main 当前提交的 source。

## NUI 文档结构

### 顶层声明

常用形式：

```text
version 1
surface surface.editor.example revision 1
budget nodes=128 bindings=64 instances=64 text=64 glyphs=4096 events=32 clips=24
flow example-flow
```

约束：

- 每行一个 declaration。
- 层级缩进必须是两个空格，不能使用 tab 或奇数缩进。
- 字符串使用双引号。
- key 使用 ASCII 字母、数字、`.`、`_`、`-`。
- 不支持 JavaScript、Rust、表达式、回调、URL、文件路径、GPU handle 或 pointer 坐标。
- `version`、`revision`、`budget` 在 fixtures 中常用；当前 host 动态 source 没有 version/revision，但 parser 接受这个最小形式。

### Typed inputs

```text
input enabled bool default true
input health f32:0..100 default 82
input level u32:0..100 default 12
input mode enum:alpha|beta|gamma default beta
input title text default text:empty
input asset_window grid default grid:empty
```

支持：

- `bool`
- `i32`
- `u32`
- `f32`
- `text`
- `enum:a|b|c`
- `grid`

数值 input 可以有 inclusive range，例如 `f32:0..1`。

`emitevent` 是 input declaration 的尾部标记：

```text
flow terrain-workbench
input brush_size i32:0..24 default 4 emitevent
```

它把变量变化作为观察事件发布到 `neon-eventd`，不是业务 command，也不能替代 semantic `event`。

### Components

V1 closed vocabulary：

```text
surface panel text button input checkbox radio_button slider drag_value
combo dropdown tabs selectable list_box scrollbar progress_bar image render
scroll overlay branch repeat template data_grid dialog tooltip
```

仓库 fixtures 实际覆盖的主要能力：

- `surface`、`panel`：层级和 layout 容器。
- `text`：静态字符串或 text input binding。
- `button`、`input`：交互控件和 semantic event。
- `checkbox`、`radio_button`、`selectable`：bool 状态。
- `slider`、`drag_value`、`scrollbar`、`progress_bar`：numeric 状态；`progress_bar` 是 display-only，不接收 pointer input。
- `combo`、`dropdown`、`tabs`、`list_box`：enum state。
- `image`：绑定 `resource <key> image`。
- `render`：绑定 `resource <key> render_surface`。
- `scroll`、`overlay`、`dialog`、`tooltip`：布局和局部 presentation。
- `repeat`、`template`：有容量上限的 bounded rows。
- `data_grid`：有 row height、overscan、columns schema 的虚拟表格。

### Layout and visual attributes

```text
row column overlay
w h minw maxw grow shrink basis
pad gap align justify clip
fill #RRGGBB
line #RRGGBB
ink token:<name>
token token:<name>
```

可用 align：`start`、`center`、`end`、`stretch`。

可用 justify：`start`、`center`、`end`、`between`、`around`、`evenly`。

clip：`none`、`bounds`、`rounded`、`scroll`。

### Direct bindings

```text
text title value $title
button publish enabled $can_publish event asset.review.publish
checkbox feature checked $feature_enabled event gallery.checkbox.toggle
radio_button mode selected $radio_selected event gallery.radio.select
slider exposure numeric $slider_value event gallery.slider.commit
combo mode state $combo_choice event gallery.combo.select
progress_bar hp numeric $health
panel inspector visible $inspector_visible
```

类型必须匹配：

- `checked`、`selected`：bool。
- `numeric`、`scroll`：`i32`、`u32`、`f32`。
- `state`：enum。
- `$input_key` 必须引用已声明 input。

### Semantic events

```text
button publish value "Publish" event asset.review.publish
panel status-root event character.open_status
```

`event` 只声明 intent，不声明 handler。渲染器负责 hit testing、focus、pointer capture 和 control-local preview；边界上传递的是声明过的 semantic intent，不传 pointer 坐标或 renderer hit ID。

正确的数据流是：

```text
NUI event -> runtime semantic gate -> Bevy/domain intent handler
Bevy/domain accepted state -> typed UiInputFrame -> runtime -> renderer
```

### Branch and statechart

状态机是有限 presentation state，不是脚本引擎：

```text
input workspace_state enum:loading|ready|error default loading
input can_publish bool default false

machine asset_review initial loading
state asset_review ready
state asset_review publishing
state asset_review error
sync asset_review when $workspace_state=ready -> ready
sync asset_review when $workspace_state=error -> error
on asset_review asset.review.publish when $can_publish -> publishing emit asset.review.publish

branch ready-view in asset_review.ready
  text ready-label value "Ready"
```

也支持 input predicate branch：

```text
branch loading-view when $workspace_state=loading
  text loading-label value "Loading"
```

只支持直接 bool、取反 bool 或 enum equality predicate。没有任意表达式。

### Drag and drop

```text
drag inspector-drag source inspector axis both snap 8 threshold 3 within parent
```

WGPU 本地处理 capture、preview、snap、clamp 和 target resolution。业务持久化必须等待 domain 返回 accepted/rejected revision；Flow 不直接修改 topology。

### World panels

```text
surface editor column
  world panel mission-marker camera 3d:editor-camera anchor mission.main w 240 h 48
    text mission-title value "Mission"
```

合法 camera 形式只有 `2d:<stable-id>` 和 `3d:<stable-id>`。camera frame 缺失时整个 subtree 不参与 layout、hit testing 和 draw。camera matrix、world position、shader 参数不属于 NUI。

### Repeat, template and data grid

```text
repeat activity-rows h 72 capacity 6 key row_key overflow_summary
  text activity-row value "Activity entry"

template asset-row h 40 capacity 12 key row_key overflow_summary
  panel asset-row-panel row h 32
    text asset-row-label value "Asset row"

data_grid virtual-list source $asset_window capacity 24 row_height 24 overscan 6 columns "id:100:text,name:220:edit:64:virtual_list.name.commit"
```

这些结构必须有 bounded capacity 和 stable key；不能通过运行时 Flow 代码动态创建无限节点。

## 动画能力边界

### NUI Flow V1 没有的语法

以下写法不是当前 V1 grammar：

```text
animate health from 82 to 100 duration 0.4 ease out
transition status-action scale 1.1 duration 200ms
on click callback update_health()
```

搜索 parser、schema、fixtures 和 WGPU runtime 后，未发现可由 `.nui` source 直接声明 tween 的稳定 V1 语法。

### 当前已有的 presentation 能力

- control 的局部 pressed/hover/focus/capture 状态由 renderer 管理。
- statechart 只切换有限 presentation state 和 branch，不执行数值插值。
- drag preview、snap、clamp 是 renderer-local interaction feedback。
- `UiTransition`、`enter_transition` 和 `animation_active` 存在于 schema/renderer 内部，但当前 fixtures 没有对应的 NUI authoring declaration；不能仅凭 `.nui` 文本假设它会产生 health bar tween。
- 数值 health 动画若属于项目自有动画系统，应由现有 Bevy/domain animation system 逐帧产生 input snapshot，NUI 只负责绑定 `numeric $health`。

## 当前 status UI 的问题清单

### 1. 实际 source 与编辑文件分离

当前启动流程：

```text
main.rs::monster_field_flow()
 -> ui_sources = vec![monster_field_flow()]
 -> ui.flow.submit
 -> parser/compiler/runtime
```

因此修改 `assets/ui/ordinary-status.nui` 不会影响当前运行窗口。应该把 source 抽成一个真正加载的 `.nui` 文件，或者明确继续维护 Rust builder，但不能同时维护两份看起来相同的 UI 定义。

### 2. screen status event 没有 NUI statechart

当前 status button 只有：

```text
button status-action h 24 value "Toggle status" event character.status.toggle
```

它会发 intent，但没有 `machine`、`state`、`on` 或 `branch`。因此点击后的状态变化完全依赖 Bevy 的 `react_to_semantic_intents` 和后续 input frame。

### 3. `progress_bar` 是绑定显示，不是动画声明

```text
progress_bar health_bar numeric $health
```

这个 declaration 只把当前 `$health` 映射成进度显示。它不会自动从旧值插值到新值；旧值到新值的动画应由已有项目动画系统或 renderer 已明确支持的 transition path 负责。

### 4. stale revision 修复点

accepted input response 必须从以下位置读取 revision：

```text
result.snapshot.scalar_inputs.input_revision
```

如果 host 不推进该值，第一次 frame 可能成功，第二次开始就会出现：

```text
ui_program_stale_input_revision
```

这正好解释了之前日志从 `nui-flow:combined-ui:2` 开始连续 rejected 的现象。

## 已有案例索引

| Fixture | 展示能力 |
|---|---|
| `asset-review-workbench.nui` | typed text/bool/enum inputs、statechart、branch、repeat、template、render、semantic events、drag |
| `imgui-component-gallery.nui` | checkbox、radio、slider、drag_value、combo、dropdown、tabs、selectable、list_box、scrollbar、image、dialog、data_grid、drag/drop |
| `terrain-workbench.nui` | input commit、enum tool state、loading/ready/error branches、render、repeat |
| `kanban-reparent-workbench.nui` | statechart、drag/drop、accepted/rejected presentation branch、revisioned reparent workflow |
| `scroll-view-demo.nui` | scroll container、clip/layout overflow |
| `virtual-list-demo.nui` | data grid、bounded capacity、row height、overscan、column schema |

## 推荐的 status 写法

如果项目动画系统负责 health 插值，NUI 保持简单：

```text
version 1
surface combined-ui revision 1
budget nodes=32 bindings=8 instances=0 text=8 glyphs=128 events=4 clips=4
input health f32:0..100 default 82
input mana f32:0..100 default 64
surface status-root column w 320 h 140 pad 8 gap 6 fill #244A62 line #8ED6F0
  panel bars row w 304 h 48 gap 8
    progress_bar health-bar numeric $health
    progress_bar mana-bar numeric $mana
  button status-action h 24 value "Toggle status" event character.status.toggle
```

Bevy/domain 流程：

1. semantic event accepted。
2. 现有动画系统把 health 从当前值推进到 target。
3. 每个 animation tick 产生合法的 `UiInputFrame`，使用当前 program revision 和 input revision。
4. runtime accepted response 返回新的 nested input revision。
5. host 推进 revision，下一 tick 继续提交。

不要把 tween、时间、easing 或业务状态写进 NUI；V1 没有这些语义，且会破坏 domain/renderer ownership boundary。

## 验证命令

```powershell
cd D:\Neon3
cargo test -p neon-ui-runtime nui_flow::tests::
cargo test -p neon-ui-runtime nui_state_machine::tests::

cd D:\bevy-nui-host
cargo check
cargo run --release
```

运行时验证重点：第一次 accepted 后，后续 `nui-flow:combined-ui:*` 不应再连续出现 `ui_program_stale_input_revision`；应能看到 accepted frame 的 nested `snapshot.scalar_inputs.input_revision` 单调递增。
