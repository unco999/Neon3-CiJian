# NUI Flow 代码编辑器组件（code_editor）初期设计

> 施工主文档已迁移到 [`plan/neon3-nui-flow-code-editor.md`](../plan/neon3-nui-flow-code-editor.md)。
> 本文保留为背景设计稿；实施状态、协议字段、里程碑和验收标准以施工主文档为准。

状态：设计讨论稿（v0.2，开放问题已确认）。本文档描述把 `code_editor` 作为
与 `data_grid`、`scroll` 同级别的声明式组件加入 NUI Flow 的方案。范围是
编辑 NUI Flow 文档本身的代码编辑器：语法高亮 + 代码智能提示。

主目标（v0.2 确认）：**做一个能实时热更新显示所编辑 NUI 的训练场软件**。
`code_editor`、注释语法、全局文字特效都服务于这个产品形态。

已确认决策：

1. 编辑器同时支持两种宿主：文件型（编辑 `.nui` 源）与 live 型
   （编辑正在运行的 UI 的 Flow 声明，走 `nui flow patch` 链路）。协议上无区别。
2. NUI Flow 引入 `#` 行注释语法（本文 §3）。
3. 字体使用现有默认字体；文字特效做成**全局 text shader 能力**（本文 §4），
   编辑器与所有 text 节点共享。

## 1. 目标与定位

- `code_editor` 是一个普通 NUI 组件：Flow 文档里只声明属性即可使用，
  不写任何代码、不注册回调。
- 编辑对象第一优先是 NUI Flow 语句（`.nui` 文本）；语法定义为可插拔的
  `language` 描述，V2 可扩展到 shader / JSON 等其他语法。
- 核心能力：
  - 语法高亮（token 着色 + 诊断波浪线）。
  - 代码智能提示（上下文感知补全、触发式弹出、键盘导航）。
  - 常规编辑：光标、选区、输入、删除、换行自动缩进、undo/redo、IME（中文输入必需）。
- 不做（V1 明确排除）：多光标、代码折叠、minimap、搜索替换 UI、LSP、
  任意语言通用引擎、执行/求值任何文档内容。

## 2. 分层所有权（遵循 AGENTS.md 既有边界）

```text
neon-editor-core (新 crate, 无窗口, 无 wgpu, 无 IO)
  纯 Rust 编辑器内核：
    rope 文本缓冲、行索引、UTF-8 图素索引
    NUI Flow tokenizer + 增量高亮（按行状态缓存）
    补全引擎（上下文模型 + 候选排序）
    本地 undo/redo 栈、变更集(diff)生成
  确定性、可单测、可 fuzz；不认识窗口、GPU、协议。

neon-ui-runtime (无窗口, 无 wgpu)
  Flow 语法扩展：解析 code_editor 声明、校验属性、lower 成
  UiCodeEditorDeclaration IR。
  文档帧 / 变更集帧的 schema 校验（同 data_grid 的输入帧模式）。
  补全结果帧校验后作为输入发布。

neon-wgpu-runtime (唯一窗口 + wgpu)
  Layer 1 本地实时交互：光标、选区、按键、拖选、滚动、补全弹层键盘导航、
  IME 组合串(preedit)。打字反馈绝不等待跨进程往返。
  渲染：按 span 着色的 text run、选区矩形、光标、行号槽、当前行高亮、
  诊断波浪线、补全弹层。滚动/裁剪复用 scroll 既有实现。
  文字特效：text 绘制管线的 fx 层（§4），编辑器与全局 text 共享。

宿主 / 领域服务
  拥有文档真相（revisioned document frame）。
  接收变更集，accept -> 新 revision；reject -> 返回 accepted 状态令 UI 回滚。
  训练场场景下，宿主就是外部控制端（见 §5）。
```

依赖方向单向：`neon-editor-core` 不依赖任何 Neon3 运行时 crate；
`neon-wgpu-runtime` 与 `neon-ui-runtime` 都可以链接它。业务 runtime 之间
不互相依赖的既有规则不变。

### 为什么内核是独立 crate

- 打字、光标、高亮必须在 WGPU 进程内以帧率响应（AGENTS.md §26 Layer 1），
  所以内核要链接进 `neon-wgpu-runtime`。
- 声明校验、文档帧校验在 `neon-ui-runtime`，它也需要读同样的
  token/文档模型做校验，所以内核同时链接进 UI runtime。
- 内核无 IO、无 GPU、无窗口，可以完全脱离窗口做 headless 验收
  （符合 §17/§20：AI 用 scenario 而不是手点窗口验证）。
- CLI 可复用同一内核做 headless 高亮/校验输出。

## 3. 注释语法（已确认引入）

V1 只加行注释，保持词法确定性：

```text
# 整行注释
surface demo column w 400 h 300   # 行尾注释
```

词法规则：

- `#` 开始到行尾为注释；`#` 必须处于"token 起始位"（前随空白或行首），
  `abc#def` 不是注释，避免歧义。
- 字符串与数值 literal 内部的 `#`（如 `"a#b"`、`#101820` 色值）不开启注释。
- 无块注释；无嵌套。formatter 保留注释原样（不重排文字，只保证缩进归位）。
- 解析器：注释在 tokenize 阶段剥离，但记录 token class `comment`
  供编辑器高亮；注释不出现在 IR，也不进入 patch 匹配。
- 稳定性：已接受文档中的注释在 patch 应用后尽量原位保留；
  V1 允许"被修改行之外的注释保证保留"这一较弱承诺，写入文档说明。

## 4. 全局文字特效（text fx / text shader 能力）

目标：高审美要求的文字视觉效果，作为 **全局 text 管线的 shader 能力**，
编辑器的代码文本与任何 `text` 节点共享同一套机制，而不是编辑器私有功能。

### 4.1 声明模型：全局命名 text style + 每节点引用

```text
text_style neon-title
  fill #6EF3C5
  fx glow intensity 1.2 radius 8 color #6EF3C5
  fx sweep color #FFFFFF width 24 period 3.5
  fx soft_shadow offset 0 2 blur 6 color #00000080

surface demo column
  text title style neon-title value "Assets"
  button publish value "Publish" text_style neon-title event asset.review.publish
```

- `text_style <key> ...` 是新的顶层声明，定义一次，全局引用；
  未引用的 style 不产生任何渲染成本。
- `text` 节点与带文字的控件（button 等）通过 `text_style <key>` 引用；
  节点上的直接样式属性仍可用，显式 `style` 覆盖默认。
- style 只承载 presentation：颜色、fx 参数；不承载布局、内容、事件。

### 4.2 内置 fx 预设集

首批内置（WGPU text 管线实现，Flow 里只有命名 key + 有界参数）：

| fx key | 效果 | 主要参数 |
| --- | --- | --- |
| `glow` | 外发光/霓虹 | color, radius, intensity, animated(呼吸) |
| `outline` | 描边 | color, width |
| `soft_shadow` | 软阴影 | offset, blur, color |
| `gradient` | 渐变填充 | from, to, angle |
| `sweep` | 流光扫过 | color, width, period |
| `wave` | 波动/漂浮 | amplitude, frequency, speed |

每个 fx 参数有 schema 校验（类型与有界范围），超界直接 reject，
与 material shader 的 bounded parameters 同纪律。

### 4.3 自定义 text shader 包

复用既有 `wgpu.shader.register` + `UiShaderPackage` 体系，扩展一个
`text` 绘制目标：

```text
shader package 注册（Node SDK / CLI / 宿主，走公开 RPC）
  package_id: "chroma-aberration-text"
  draw_target: text            # 新增；既有包默认 panel/material
  entry_point: text_material
  parameters: [ amount: f32:0..1 ]

Flow 引用：
  text title style glitch value "Error"
  text_style glitch fx shader chroma-aberration-text amount 0.5
```

约束（与 NUI_CUSTOM_SHADER_AND_GEOMETRY 同一契约）：

- fragment shader 收到 glyph alpha mask、UV、local time、有界参数；
  输出 premultiplied color。**不得**改变布局、hit test、事件路由。
- 注册方提交 WGSL 源 + digest；Flow 文档里永远只有稳定 key 和参数，
  严禁把 WGSL、URL、文件路径写进 Flow（AI Boundaries 不变）。
- 编译失败进入 fallback（标准文字绘制），并通过 diagnostics 上报，
  绝不黑屏。
- 时间 uniform 由渲染器统一提供，驱动 sweep/wave/呼吸等动画；
  不需要 Flow 侧任何状态。

### 4.4 与编辑器的关系

编辑器代码文本默认**不套用装饰性 fx**（可读性优先）；但共享同一管线，
允许训练场主题通过 text_style 给行号、括号匹配、当前行等编辑器元素
配置轻量 fx（如当前行 glow）。编辑器 token 配色体系本身接入
text_style，使训练场可以整体换肤。

## 5. 训练场（NUI Playground）应用形态

产品形态：一个窗口，左侧 `code_editor` 编辑 NUI 源，右侧实时预览，
保存即热更新。这就是 live 宿主模式的第一落地。

```text
+--------------------------------------------------+
| 训练场 (一个 wgpu 窗口, 一个根 Flow 文档)         |
|  +----------------+  +------------------------+  |
|  | code_editor    |  | 预览 surface            |  |
|  | (flow 源)      |  | (被编辑文档的渲染结果)  |  |
|  +----------------+  +------------------------+  |
+--------------------------------------------------+
```

热更新闭环：

```text
编辑器 ChangeSet (commit 或防抖)
  -> 训练场宿主 (外部控制端, 同 neon-cli 同等协议 client)
  -> parse_nui_flow / parse_nui_flow_patch 校验
     失败: 诊断帧回填编辑器(diagnostics), 预览保持上一个有效 revision
  -> compile_nui_flow_program -> 新 UiProgram revision
  -> wgpu.ui.submit_fragment
  -> 预览区更新;  wgpu.render.diagnostics 可查
```

规则：

- 校验失败的文档**永不**提交渲染；预览始终停留在最后一个有效 revision，
  编辑器内用诊断线 + 状态条提示。符合"错误必须可精确追踪"（§18）。
- 热更新防抖独立于编辑提交防抖（例如编辑 300ms / 重编译 150ms），
  且预览重编译走有界队列，后到的新文档替换旧任务，不排队堆积。
- 预览区本身是一个普通 surface：训练场根文档声明
  `render surface preview-src ...` 类节点（复用既有 render surface 机制），
  绑定被编辑文档的编译产物；两个 Flow 文档边界清晰，
  被编辑文档不感知训练场的存在。
- 文字特效在这条闭环里可即时所见：改 text_style -> 热更新 -> 预览即变，
  这是训练场对 fx 能力的直接验收路径。

## 6. code_editor 声明语法（V1 草案）

```text
input script doc default doc:empty
input locked bool default false
input extra_completions completion_set default completions:empty

surface flow-editor column w 900 h 640 gap 8 pad 12 fill #101820
  code_editor flow-script source $script language nui_flow
    line_numbers true
    wrap none
    font_size 14
    tab_size 4
    read_only $locked
    event script.edit.preview
    event script.edit.commit
    completions $extra_completions
```

属性表（与现有组件风格一致：空格分隔的 key value 对）：

| 属性 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `source` | `doc` 输入引用 | 必填 | 绑定 revisioned 文档帧，同 `data_grid source` 模式 |
| `language` | 枚举 | `nui_flow` | V1 只有 `nui_flow`；语法表可插拔 |
| `line_numbers` | bool | `true` | 行号槽 |
| `wrap` | `none` \| `char` | `none` | V1 建议只做 `none`，软换行推 V2 |
| `font_size` | 数值 | 14 | 逻辑像素 |
| `tab_size` | 整数 1..8 | 4 | Tab 展开宽度 |
| `read_only` | bool 输入引用 | `false` | 只读时仍可滚动、选择、复制 |
| `event` | 点分 intent | 可多条 | `preview`（输入防抖）/`commit`（失焦或显式提交）复用既有 event 机制 |
| `completions` | `completion_set` 输入引用 | 可选 | 宿主提供的动态补全帧（资产名、input 名等） |
| `gutter_diagnostics` | bool | `true` | 诊断行标记 |

`doc` 与 `completion_set` 是新的输入种类，校验规则同 `grid`：
节点 key 不是数据身份，`source` 指向的输入 key 才是稳定身份。

## 7. 数据与所有权模型（镜像 data_grid 的窗口帧模式）

大段动态文本不走普通 `text` 输入（那类输入只承载稳定 text handle）。
采用与 DataGrid 一致的"宿主拥有真相 + 有界帧"模型：

```text
UiEditorDocument (宿主发布, revisioned)
  revision: u64
  text_handle: 文本驻留句柄（全文正文，走 text 驻留资源路径）
  line_count, byte_len
  diagnostics: [ { line, col, len, severity, code, message } ]

UiEditorChangeSet (UI -> 宿主, revisioned + idempotency_key)
  base_revision: u64
  ops: [ Insert{line,col,text_handle片段} | Delete{line,col,len} ]   // 有界条数
  kind: preview | commit
```

规则：

1. 初始加载：宿主发布完整 document frame；UI runtime 校验后挂载。
2. 编辑期间：WGPU 本地立即改自己的展示缓冲并逐帧渲染（Layer 1）；
   变更按防抖策略（如 500ms 空闲或 N 条 op）打成 ChangeSet，经
   `UiHostInbound` 走宿主适配器（Layer 2 可靠 RPC）。
3. 宿主 accept：返回新 revision 的 document 基线（可只回增量对齐结果）；
   reject（如只读、revision conflict）：UI 回滚到 accept 基线并保留未提交输入，
   同时给出稳定的 `editor.revision_conflict` 诊断，绝不 silent no-op。
4. undo/redo 是 UI 本地 presentation 状态（内核维护），语义层的撤销由
   宿主通过发布旧 revision 基线完成——两层 undo 不混用。
5. 宿主（训练场/CLI）把 `parse_nui_flow` 诊断随 document frame 下发，
   编辑器只画波浪线，不自己判断"文档是否合法"。
6. live 模式与文件模式在协议层无区别：宿主可以是训练场（内存中编译），
   也可以是文件监视服务（写盘 + 重读）。`kind: preview` 供训练场
   做更激进的即时编译，`kind: commit` 表示用户显式保存点。

## 8. 语法高亮设计

### Token 分类（nui_flow）

| class | 示例 |
| --- | --- |
| keyword | `input` `surface` `machine` `state` `sync` `on` `emit` `branch` `template` `repeat` `drag` `drop` `scroll` `text_style` |
| node_kind | `panel` `text` `button` `slider` `data_grid` `code_editor` ... |
| attribute | `w` `h` `fill` `source` `capacity` ... |
| input_ref | `$can_publish` |
| color_literal | `#101820` |
| numeric_literal | `14` `0.5` `i32:0..24`（含 typed range 形式） |
| string | `"Publish"` |
| intent | `asset.review.publish`（点分语义 intent） |
| enum_literal | `alpha`（`enum:a|b` 声明处的选项） |
| comment | `# ...`（§3 新语法） |
| fx_key | `glow` `sweep` ...（fx 参数位置可识别） |
| invalid | 无法归类的 token（配合诊断） |

### 实现要点

- tokenizer 与 `nui_flow.rs` 解析器共用同一份**静态语法表**
  （组件名、每个组件的属性名/属性类型、keyword 表）。现状是属性匹配
  散在 parser 的 match 分支里；本设计要求先把语法表提炼为
  `neon-ui-schema` 中的单一数据源，parser、formatter、completion 三方共用。
  这是本设计中唯一建议的既有代码重构。
- 增量高亮：每行缓存 (token 序列, 行尾词法状态)，编辑只重扫受影响行；
  跨行结构（字符串）由行尾状态传递。
- token class 到最终 RGBA 的映射走 text_style/皮肤体系（§4.4），
  训练场可整体换肤，内核只产 class。

## 9. 代码智能提示设计

### 上下文模型

补全引擎按光标处的语法上下文产出候选，规则完全来自语法表 + 当前文档符号：

| 光标上下文 | 候选来源 |
| --- | --- |
| 行首 / 新语句 | 顶层关键字（`input` `surface` `machine` `state` `sync` `on` `branch` `text_style` ...） |
| 节点名位置 | 已声明 node_kind 表，附带一行说明 |
| 节点属性位置 | 该组件的属性表（含类型、默认值说明），已出现的属性可过滤 |
| `$` 触发 | 当前文档已声明的 input 名（来自文档符号索引） |
| 属性值需颜色 | 补全十六进制色 + 当前文档已用色 |
| `event` 值 | 已声明 machine 的 on/emittable intent + 文档已出现的 intent |
| fx 参数位置 | fx key + 该 fx 的参数名/范围 |
| `enum:` 声明 | 无自动候选，仅格式提示 |

文档符号索引（input 名、节点 key、machine/state、text_style、已用 intent）
由内核在解析诊断时增量维护，随编辑本地更新。

### 宿主动态补全

`completions $extra_completions` 允许宿主发布 revisioned
`completion_set` 帧（例如项目 asset 名、可用 intent 清单）。帧有
`capacity` 上限与 `revision`，与 data_grid 的列定义帧同构。UI 只展示，
不做过滤逻辑之外的处理。

### 交互

- 触发：输入中自动触发（属性位置、`$` 后）+ `Ctrl+Space` 手动触发。
- WGPU 本地渲染弹层，方向键/回车/Esc 导航，全部 Layer 1。
- 选中候选项产生 `preview` 语义事件（可声明），插入动作本身是本地编辑，
  随后续 ChangeSet 提交。

## 10. 输入与 IME

- WGPU 拥有键盘焦点与 pointer capture（既有所有权），编辑器获得焦点时
  申请平台 IME 关联；组合串(preedit)在光标处本地渲染，commit 后转为
  Insert op。中文输入是 V1 必需验收项。
- 拖选、双击选词、滚轮滚动全部 Layer 1 本地完成，不产生跨进程请求。

## 11. 性能预算与虚拟化

- 只实例化可见行 + overscan 的 text run（复用 data_grid 的窗口化思路）。
- 预算：1 万行文档，单次编辑增量高亮 < 1ms；补全候选 < 200 条，
  弹出 < 16ms；打字路径 p95 帧时间不劣化（用既有 perf 基准对比）。
- ChangeSet 防抖参数可配置，避免逐键跨进程。
- 文字特效预算：同一可见区叠加 fx 层数有上限（建议 2），
  超出时按声明顺序取前 N 个并记 diagnostics；fx 不进 ChangeSet/
  热路径，style 变更与普通 style 更新同路径。

## 12. 验收（对应 §20/§21 分层）

| 层级 | 证明方式 |
| --- | --- |
| contract-ready | tokenizer/补全引擎/ChangeSet 的单元与 fuzz 测试（neon-editor-core 内）；注释语法 lexer 测试 |
| service-ready | headless scenario：挂载文档帧 -> 模拟 op 序列 -> 断言高亮 span JSON 与补全候选 JSON |
| gpu-ready | `code_editor` 声明通过校验、IR 含 declaration |
| wgpu-rendered | frame capture 断言：行号、彩色 token、光标、弹层可见，无黑屏/遮挡 |
| interactive-accepted | 人工验证 IME、拖选、滚动手感 |

scenario 示例（neon-testkit）：

```yaml
id: ui.code-editor.highlight-and-complete.v1
steps:
  - target: ui-runtime
    method: ui.editor.attach_document
    params: { doc_fixture: fixtures/asset-review-workbench.nui }
  - apply_ops: [ { insert: { line: 3, col: 2, text: "slide" } } ]
    expect:
      completions_prefix_match: "slider"
      highlight_line_3_contains: [attribute, node_kind]
  - commit_changeset: true
    expect_job: editor.commit.accepted
```

训练场热更新闭环 scenario：

```yaml
id: playground.live-edit-loop.v1
steps:
  - attach: { editor_doc: fixtures/playground-sample.nui, preview: true }
  - apply_ops: [ { insert: { line: 6, col: 0, text: "text hello value \"hi\"\n" } } ]
  - commit_changeset: true
  - await:
      preview_program_revision: "> 1"
  - expect:
      diagnostics_empty: true
      preview_contains_node: "hello"
  - apply_ops: [ { insert: { line: 2, col: 0, text: "surfaec broken\n" } } ]
  - commit_changeset: true
  - expect:
      diagnostics_contains: { code: "nui_flow_unknown_keyword" }
      preview_program_revision_unchanged: true
```

## 13. 实施切片

1. **注释语法**：lexer/parser/formatter 支持 `#` 行注释（小步先行，
   独立可测，编辑器依赖它）。
2. **内核**：`neon-editor-core`——rope、tokenizer、行状态缓存、符号索引、
   ChangeSet。纯库 + 单测。
3. **语法表提炼**：把 nui_flow 组件/属性表提炼进 `neon-ui-schema`，
   parser 改为查表（行为不变，现有测试守护）。
4. **声明接入**：`nui_flow.rs` 增加 `code_editor`/`text_style` 解析、
   `doc`/`completion_set` 输入种类。
5. **渲染**：`ui_renderer` 增加编辑器 paint（先只读模式：高亮 + 行号 +
   诊断线），虚拟化滚动复用。
6. **交互**：光标/选区/键入/undo + ChangeSet 防抖提交 + 宿主 accept/reject 回滚。
7. **补全**：上下文引擎 + 弹层渲染 + `Ctrl+Space` + 宿主动态补全帧。
8. **IME** 与编辑器收尾验收。
9. **文字特效**：内置 fx 预设（glow/outline/shadow 先行）-> text_style
   全局声明 -> 自定义 text shader 包（复用 UiShaderPackage，`draw_target: text`）
   -> 编辑器元素换肤。
10. **训练场应用**：热更新闭环（校验门 + 防抖重编译 + 诊断回填）+
    预览 render surface + 分栏布局，验收 `playground.live-edit-loop.v1`。

文字特效与训练场排在编辑器主体之后，但 text_style 体系（切片 9 前半）
可以提前到切片 4 之后并行，因为它独立成立且编辑器换肤依赖它。
