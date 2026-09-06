# NUI Component Skin System 设计稿

## 目标

让任何标准 NUI 控件保留统一行为，但允许应用用颜色、image 或 nine-slice 完全替换其视觉构成。

```text
行为层：命中、hover、pressed、focus、键盘、拖拽、value commit、semantic intent
皮肤层：背景、边框、轨道、填充、滑块、图标、焦点环、禁用态
```

皮肤不能直接执行业务逻辑，也不能创建 GPU resource。图片仍通过 `ui.image.upload` 或资源绑定进入 renderer-owned atlas。

## 基本原则

1. 控件类型不变：`button` 仍是 button，`slider` 仍是 slider。
2. 图片只改变绘制，不改变 hit test、键盘焦点和 semantic event。
3. 每个视觉元素是一个有限 slot，不允许任意 shader 或自由节点树。
4. pointer/keyboard 状态由 WGPU Runtime 本地维护；domain 不发布 hover/pressed。
5. 皮肤使用稳定 key 和资源 key，不发送 texture handle、UV、路径或 GPU 对象。
6. 所有皮肤都必须有可访问 fallback：默认 renderer skin 始终可用。

## 控件与视觉 Slots

| 控件 | 必需 slots | 可选 slots |
| --- | --- | --- |
| button | `body`、`label` | `icon`、`focus_ring`、`badge` |
| slider | `track`、`fill`、`thumb` | `focus_ring`、`ticks` |
| checkbox | `box`、`mark` | `focus_ring`、`label` |
| radio | `ring`、`dot` | `focus_ring`、`label` |
| input | `body`、`caret`、`selection` | `focus_ring`、`placeholder` |
| tabs | `tab_body`、`active_indicator` | `divider`、`focus_ring` |
| progress_bar | `track`、`fill` | `peak_marker` |
| scrollbar | `track`、`thumb` | `hover_thumb` |
| window_button | `body`、`icon` | `focus_ring` |

每个 slot 可以选择：

```text
solid color
image stretch
image cover
image contain
nine-slice image
renderer default
```

## 状态模型

通用状态：

```text
idle
hover
pressed
focused
disabled
selected
active
```

不是所有状态都需要每张贴图。解析器使用确定的 fallback 链：

```text
pressed -> hover -> idle
focused -> idle
disabled -> idle
selected -> active -> idle
```

例如 slider 的 thumb 只声明 `idle` 与 `hover`，按下时自然回退到 `hover`；不会发生资源缺失或空白控件。

## Flow 草案

资源先声明，皮肤在顶层声明，控件用稳定 skin key 引用：

```text
resource pulse-button-idle image
resource pulse-button-hover image
resource pulse-slider-track image
resource pulse-slider-fill image
resource pulse-slider-thumb image

skin pulse-primary button
  slot body idle resource pulse-button-idle nine_slice 16 16 16 16 border 8 8 8 8
  slot body hover resource pulse-button-hover nine_slice 16 16 16 16 border 8 8 8 8
  slot label color token:ink-primary padding 12 6
  slot focus_ring line token:focus width 2 radius 6

skin pulse-volume slider
  slot track idle resource pulse-slider-track nine_slice 8 8 8 8 border 4 4 4 4
  slot fill active resource pulse-slider-fill nine_slice 8 8 8 8 border 4 4 4 4
  slot thumb idle resource pulse-slider-thumb fit contain size 18 18
  slot thumb hover resource pulse-slider-thumb fit contain size 22 22

surface player overlay w 1280 h 800
  button play skin pulse-primary value "Play" event player.transport.play_pause
  slider volume skin pulse-volume numeric $volume event player.volume.commit
```

`skin` 只允许静态 key；不允许 `skin $expression`。状态由 renderer 的控件状态机选择，不通过 Flow 动态换肤。

## Schema 设计

新增公共 schema：

```text
UiControlSkin
  key
  component_kind
  slots: Vec<UiSkinSlot>

UiSkinSlot
  slot_kind
  state
  presentation: Solid | Image | NineSlice | Default
  resource_key?
  fit: Stretch | Cover | Contain
  source_insets?
  target_insets?
  content_padding?
  fixed_size?
  color_token?
```

`UiProgram` 新增 `skins` 表，控件节点新增可选 `skin_key`。Program 编译时验证：

- skin 与控件类型匹配。
- 资源存在且类型为 image。
- nine-slice source insets 对应图片尺寸合法。
- `thumb` 的最小 hit size 不小于平台定义阈值。
- 所有 slot 都能按 fallback 链解析。
- 总贴图引用、实例和 clip 数量在 resource budget 内。

## Renderer 设计

WGPU Runtime 接收已验证的 skin recipe：

1. 控件先计算自身标准逻辑矩形，例如 slider 的 track/fill/thumb。
2. 从本地状态机得到 `hover`、`pressed`、`focused`、`disabled`、`selected`。
3. 对每个 slot 按 fallback 查 skin presentation。
4. 使用 image atlas 或 nine-slice shader 生成 instance。
5. hit test 始终用控件逻辑矩形，绝不使用图片 alpha 或 atlas UV。

这保证贴图不规则、透明、带洞时，交互行为仍然稳定。

## Window Chrome

无边框窗口使用同一系统：

```text
skin pulse-window-control window_button
  slot body idle resource chrome-idle nine_slice 6 6 6 6 border 4 4 4 4
  slot body hover resource chrome-hover nine_slice 6 6 6 6 border 4 4 4 4
  slot icon color token:ink-primary

surface player overlay window_drag background
  panel titlebar row h 48 window_drag true
    window_button minimize skin pulse-window-control action minimize
    window_button maximize skin pulse-window-control action maximize
    window_button close skin pulse-window-control action close
```

`window_drag` 和 `window_button` 是 WGPU-local 行为：不进入 domain host，不产生业务 intent。

## SDK API

窗口启动标准化为：

```ts
NeonApp.start({
  mode: "windowed",
  window: {
    chrome: "borderless", // decorated | borderless
    initialSize: [1280, 800],
    minSize: [1040, 640],
    resizable: true,
  },
});
```

SDK 把配置转为 runtime 启动参数；不通过 environment variable 作为正式 API。环境变量仅保留开发调试用途。

## 验收

每个新 skin capability 必须提供：

1. parser/formatter round-trip test。
2. schema validation test：错误资源、错误 component、非法 nine-slice、缺失 fallback。
3. renderer test：每个控件 state 生成期望 slots 与 UV。
4. hit test test：替换皮肤后按钮/slider 命中范围不变。
5. JSONL window probe：input、state、selected skin slots、frame sequence、pass/fail。
6. final composition PNG capture：默认 skin 与 custom skin 都非空、无遮挡、无溢出。

## 实施顺序

1. `image fit`：已完成，作为 skin image presentation 的基础。
2. `UiControlSkin` schema + `button` body/label/focus ring。
3. `slider` track/fill/thumb。
4. `window_drag`、`window_button`、borderless resize hit zones。
5. checkbox/radio/tabs/input/progress/scrollbar。
6. theme token registry、皮肤继承和 package-level theme assets。
