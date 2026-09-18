# IDE / Agents NUI Flow 与组件缺口报告

日期：2026-09-18  
范围：Neon3 当前工作区中的 NUI Flow、UI schema、UI runtime、WGPU renderer、editor runtime，以及面向 IDE / agents 模块的组件适配性。  
结论状态：审计完成；建议先冻结 contract，再进入 IDE agents UI 施工。

## 结论摘要

当前 Neon3 已经具备构建 IDE 外壳的基础组件：`Panel`、`scroll` 容器、`Splitter`、`TreeView`、`ContextMenu`、`Tabs`、`DataGrid`、`Dialog/Modal`、`TextInput` 和一部分 `code_editor` 渲染链路。NUI Flow 的解析、lowering、revision、语义事件和 WGPU 最终绘制边界也已经存在。

但是当前不能把它描述为“IDE / agents UI 组件已经完成”。主要原因不是缺少几个按钮，而是以下四个 contract 仍未闭合：

1. NUI Flow 文档、parser、schema、editor grammar 的组件词汇已经分叉。
2. `code_editor` 当前有 renderer 和本地编辑雏形，但 `source $document` 尚未接入专用 document frame；现有 demo 依赖节点 literal `value` 承载源码。
3. `TreeView`、`ContextMenu`、`Switch`、`Accordion`、`MenuBar`、`Popup` 等新增组件有不同程度的 renderer 代码，但数据模型、内置交互、键盘行为、revision 回写和 JSONL 验收没有统一完成。
4. agents 模块需要对话流、工具调用、审批、diff、终端输出和运行状态等有界数据视图，这些不是普通 `Panel + text` 能稳定替代的组件。

建议的最低可用目标是：先完成一个可恢复、可审计的 IDE shell，再接 agents 业务视图；不要先增加大量视觉组件。

## 1. 审计依据

| 层级 | 依据 |
| --- | --- |
| NUI Flow 边界 | [`docs/nui-flow-ai-authoring.md`](nui-flow-ai-authoring.md)、[`plan/neon3-nui-flow.md`](../plan/neon3-nui-flow.md) |
| 组件盘点 | [`docs/ui-component-audit.md`](ui-component-audit.md)、[`docs/neon3-ui-component-style-animation-work-plan.md`](neon3-ui-component-style-animation-work-plan.md) |
| DataGrid / 滚动 | [`docs/nui-flow-data-grid.md`](nui-flow-data-grid.md)、[`tests/fixtures/ui/scroll-view-demo.nui`](../tests/fixtures/ui/scroll-view-demo.nui) |
| Code Editor | [`plan/neon3-nui-flow-code-editor.md`](../plan/neon3-nui-flow-code-editor.md)、[`crates/neon-ui-runtime/src/editor_component.rs`](../crates/neon-ui-runtime/src/editor_component.rs)、[`crates/neon-wgpu-runtime/src/ui_renderer/editor_renderer.rs`](../crates/neon-wgpu-runtime/src/ui_renderer/editor_renderer.rs) |
| 公共 schema | [`crates/neon-ui-schema/src/lib.rs`](../crates/neon-ui-schema/src/lib.rs) |
| Flow parser / lowering | [`crates/neon-ui-runtime/src/nui_flow.rs`](../crates/neon-ui-runtime/src/nui_flow.rs) |
| WGPU renderer | [`crates/neon-wgpu-runtime/src/ui_renderer.rs`](../crates/neon-wgpu-runtime/src/ui_renderer.rs)、[`crates/neon-wgpu-runtime/src/lib.rs`](../crates/neon-wgpu-runtime/src/lib.rs) |
| editor control-plane | [`crates/neon-editor-runtime/src/lib.rs`](../crates/neon-editor-runtime/src/lib.rs)、[`crates/neon-editor-runtime/src/bin/editor_protocol_probe.rs`](../crates/neon-editor-runtime/src/bin/editor_protocol_probe.rs) |
| 当前变更记录 | [`CHANGELOG.md`](../CHANGELOG.md) |

## 2. 面向 IDE 的现有组件状态

这里的“可用”只表示已有可复用代码，不等于达到 `composition-ready` 或 `wgpu-rendered`。完整完成仍需要对应的 headless scenario、window probe 和 revision 证据。

| 组件 / 能力 | 当前实际情况 | IDE / agents 适用性 | 判定 |
| --- | --- | --- | --- |
| `Panel` / `surface` | layout、row/column/overlay、gap、padding、clip 和基础样式已有 | IDE shell、工具栏、侧栏、Inspector 的基础容器 | 可复用，但需继续补齐状态样式、clip/hit/depth parity |
| `scroll` | 当前是 `Panel` + `UiClipPolicy::Scroll`，renderer 已有滚轮、thumb、middle-button pan 和 metrics | Inspector、文件树、消息列表、日志列表 | 基础可用；不要重复创建第二套 `ScrollView`，应把 `scroll` 作为 canonical 名称 |
| `Splitter` | schema/parser/renderer 已有；renderer 有实时拖拽和位置保留逻辑 | IDE 左右栏、编辑器与预览区、agents 面板 | 基础实现存在；缺最小尺寸/方向/比例 contract、可靠 commit、headless/window 对照 probe |
| `TreeView` | schema 有 `UiTreeNode`；当前 renderer 主要按 TreeView 下的扁平 Label 顺序和 x 缩进做展开隐藏 | 文件树、符号树、agent task tree | 不足以作为项目文件树权威视图；缺 revisioned tree frame、lazy loading、stable node payload、键盘导航、选择/rename/loading/error 状态 |
| `ContextMenu` | parser/schema/effect 和右键锚定 popup 已存在；outside click 关闭路径已存在 | 文件树、编辑器、diff hunk、agent tool call | 基础 popup 可用；缺统一 menu item 数据、disabled/checked、键盘导航、快捷键、viewport collision、命令状态 revision |
| `Popup` / `MenuBar` | `Popup` 已有 top-layer body；MenuBar 相关 demo 由外部状态手工改变可见性、位置和文字 | command palette、菜单栏、补全/操作菜单 | 不能按“通用交互组件已完成”处理；需要 popup owner、focus trap、escape/outside click、item selection 的统一 contract |
| `Accordion` | schema/renderer 样式入口存在；现有 showcase 由外部代码手工切换 content visibility 和箭头文字 | agents run detail、工具调用详情、Inspector section | 仅有展示/案例级实现；缺内建展开状态、键盘行为、single/multiple policy 和 semantic event |
| `Tabs` | choice presentation 和 branch 场景已有 | 文档标签、Agent/Terminal/Diff 面板切换 | 基础可用；缺 stable tab frame、关闭、dirty、拖动排序、overflow、active tab 与 branch 的统一回写 |
| `TextInput` | 基础输入、focus/caret/IME 路径存在 | 搜索框、文件名、agent prompt 的单行输入 | 可复用，但剪贴板、焦点环、提交/取消和 revision 证据仍需补全 |
| `code_editor` | parser/schema declaration、`EditorComponent`、WGPU token/caret/selection/completion 绘制和 IME forwarding 已存在 | NUI Flow 编辑器、代码/配置编辑 | 逻辑和 renderer 均为半成品；不能作为完整 IDE editor contract |
| `DataGrid` | bounded frame、stable row key、虚拟窗口、cell text/select/dropdown/edit、window request 已存在 | agent runs 表、工具调用表、资源列表、diagnostics 表 | 是当前最成熟的有界数据视图；仍缺 sticky header、列 resize、完整 keyboard selection、统一 2x/window parity 验收 |
| `Dialog` / `Modal` | top-layer/backdrop 的基础路径存在 | 权限审批、危险操作确认、冲突处理 | 可作为组合基础；缺统一 result/close lifecycle、focus trap、backdrop blocking 和 request/session/job 关联 |
| `Tooltip` | top-layer 类型和基本显示路径存在 | 命令说明、状态解释 | 缺明确 hover delay、pointer policy、keyboard focus、collision 和稳定内容来源 |
| `Toast` | schema kind、默认样式和 top-layer 分类已有 | agent job 完成/失败、保存结果 | 缺 notification queue、TTL/dismiss、去重、严重级别和可追踪 job 关联 |
| `Spinner` / `ProgressBar` | spinner 有 render-loop rotation；progress bar 有 numeric fill | loading、agent job progress | 视觉基础可用；缺统一 job state、cancel/retry、indeterminate/determinate 切换和状态语义 |
| `Switch` | schema/style/chrome 已有；renderer 内有 track/thumb 绘制 | settings、agent capability toggle | 目前主要是视觉和外部状态案例；内建 toggle path 与 Checkbox/Radio/Selectable 不一致 |
| `Divider` | renderer 基础线条和默认尺寸已有 | shell 分区、工具组 | 可作为静态 presentation；不需要优先新增功能 |
| `Image` / `RenderSurface` / `Canvas` | 外部图片上传、render surface、数据驱动 canvas contract 已存在 | agent artifact、preview、分析标注 | 可复用；agents 目前更缺数据视图而不是图片组件 |

## 3. 已发现的问题

### 3.1 P0：NUI Flow 的规范与实现不再是单一 contract

`plan/neon3-nui-flow.md` 的 closed vocabulary 仍停留在早期列表，未覆盖当前代码中已经存在的 `data_grid`、`canvas`、`tooltip`、`dialog`、`splitter`、`context_menu`、`tree_view`、`switch`、`toast`、`menu_bar`、`accordion`、`spinner`、`divider`、`popup` 等组件。实际 parser 在 [`nui_flow.rs:3580`](../crates/neon-ui-runtime/src/nui_flow.rs:3580) 已接受更多组件，schema 的 `UiNodeKind` 在 [`lib.rs:996`](../crates/neon-ui-schema/src/lib.rs:996) 也已经扩展。

影响：

- AI 根据计划文档生成 Flow 时会误判合法组件为非法。
- editor completion 不能完整提示当前可用组件。
- formatter、parser、schema、component gallery 和文档会继续分叉。
- 新增组件无法判断是正式 V1、兼容 slice，还是仅 demo 能力。

处理建议：

1. 在 `neon-ui-schema` 或独立无 GPU grammar crate 建立唯一的 component/attribute/input-kind 表。
2. parser、formatter、editor highlighter、completion、capability report 全部从同一份表生成或读取。
3. 重新发布 NUI Flow V1 component matrix，给每个组件标记 `contract-ready`、`service-ready`、`composition-ready`、`wgpu-rendered`、`interactive-accepted`。
4. 旧文档保留为背景稿时必须明确 `historical`，不能继续作为 AI authoring 入口。

### 3.2 P0：`code_editor source $document` 没有真正连接 document frame

规范设计要求 `source` 绑定稳定 document handle，正文走 editor control plane，不进入普通 GPU input slot。当前实现仍有几个断点：

- parser 在 [`nui_flow.rs:4058`](../crates/neon-ui-runtime/src/nui_flow.rs:4058) 只把 `$input` 写入 `UiCodeEditorDeclaration`，没有生成专用 `document` binding 或 document frame effect。
- `UiCodeEditorDeclaration` 仍把 `source_input_key` 定义为普通 key；schema 文档明确说当前是 compatibility Panel，但没有完成 dedicated input kind。
- `EditorBridge::sync_fragments` 在 [`editor_component.rs:1281`](../crates/neon-ui-runtime/src/editor_component.rs:1281) 通过 `collect_node_literal_text` 读取节点 literal text，而不是从 revisioned document publication 获取正文。
- 当前 code editor demo 在 [`code-editor-demo.nui:31`](../crates/neon-wgpu-runtime/tests/fixtures/ui/code-editor-demo.nui:31) 用 `value "..."` 注入源码，同时声明 `source $document`。这证明 demo 能显示编辑器，但不能证明 `source` 绑定已经工作。
- schema 还暴露了 `source_file` 字段和 parser 的 `source_file` 属性（[`nui_flow.rs:4086`](../crates/neon-ui-runtime/src/nui_flow.rs:4086)）。这直接触碰 NUI Flow 禁止文件路径、禁止 UI client 直接访问文件的边界；文件读取应归 editor/project host，不应进入 Flow。

处理建议：

1. 删除或禁止 Flow 中的 `source_file`；文件切换通过 editor/project control-plane RPC 传递 stable document ID。
2. 增加正式 `UiDocumentHandle`、`UiCompletionSetHandle` 或等价的专用 effect/frame，不复用普通 `TextHandle`。
3. UI runtime 只校验 document presentation frame 和 ChangeSet，不拥有正文真相。
4. WGPU 只消费 `UiCodeEditorPresentation`，不从 `UiNode` literal 猜测正文。
5. 新增 service probe：open、adopt、draft、commit、conflict、epoch restart、diagnostics、completion stale、reopen recovery。

### 3.3 P0：editor grammar 仍然是临时副本

[`crates/neon-editor/src/grammar.rs:3`](../crates/neon-editor/src/grammar.rs:3) 明确说明 Flow grammar 目前是 duplicated provisional table。其组件列表没有覆盖当前新增组件，输入类型也没有完整覆盖 `enum`、`vec2`、`vec4`、`struct`、`array` 等当前 schema 能力。

影响：

- IDE 中的高亮和 completion 会与 parser 接受范围不一致。
- agents 生成或修改 Flow 后，编辑器可能把合法语法标成普通 ident 或不给候选。
- 后续增加 `document`、`completion_set`、diff metadata 时会出现第二套临时语法表。

处理建议：先做 grammar single source，再做 agents 专属组件。不要通过继续手工扩展 `FlowGrammar` 解决。

### 3.4 P1：新增组件的代码成熟度不一致

CHANGELOG 记录了 `Switch`、`Toast`、`MenuBar`、`Accordion`、`Spinner`、`Divider`、`Popup` 的加入，但现有 showcase 仍通过外部 `AppState` 手工维护这些组件的 visibility、文字和 selected 状态（见 [`showcase_probe.rs:358`](../crates/neon-wgpu-runtime/src/bin/showcase_probe.rs:358)）。这类 demo 能证明“可以拼出画面”，不能证明组件已经有统一内建交互。

典型差异：

- `Checkbox`/`RadioButton`/`Selectable` 有 renderer-local toggle prediction；`Switch` 的绘制存在，但当前 toggle prediction 路径没有与它们统一。
- `Accordion` 的内容显示和标题箭头在 showcase 外部改写，而不是由 Accordion 自己消费 typed presentation。
- `MenuBar` 的菜单内容、位置和 active menu 由 showcase 外部切换；通用 popup focus、键盘导航和 menu item frame 尚未冻结。
- `Toast` 有 kind 和样式，但没有统一队列、TTL、dismiss、severity、job association contract。

建议每个新增组件先补一条最小 scenario：一次打开/关闭或选择只能产生一个 semantic event；disabled 不可命中；outside click 和 Escape 的结果可追踪；accepted/rejected publication 后 presentation 可恢复。

### 3.5 P1：TreeView 还不是 IDE 文件树模型

当前 TreeView 的 schema 有递归 `UiTreeNode`，但 renderer 的实际路径是把 TreeView 下的 Label 当作扁平行，再用 x 坐标缩进和顺序判断折叠。这个实现适合固定 showcase，不适合真实项目文件树或 agents workspace：

- 没有 revisioned bounded tree frame。
- 没有 `loading`、`error`、`permission_denied`、`unloaded` 节点状态。
- 没有稳定的 domain node payload / `AssetRef` / document ID。
- 没有 lazy expand 的 window request。
- 没有统一的 selection、rename、create、delete、context-menu target contract。
- 没有明确的 keyboard navigation、type-to-select、multi-select 规则。

建议扩展为 `UiTreeFrame`，而不是继续把文件树节点展开成大量固定 Flow 子节点。Flow 只声明列/行 presentation、capacity、事件和允许的操作；project/editor/resource domain 提供 revisioned nodes。

### 3.6 P1：DataGrid 适合表格，不应被当作 agents 消息流

DataGrid 已有较完整的 bounded frame contract，可以承担：

- agent run 列表；
- tool call 列表；
- diagnostics / problems 表；
- 文件资源或搜索结果表。

但它不适合直接承担：

- streaming conversation；
- 多段富文本和代码块；
- diff hunk 的上下文行、折叠和逐 hunk approval；
- terminal/log 的 append-only tail 和自动跟随；
- 工具调用的嵌套输入/输出。

这些场景需要独立的 bounded stream contract，不能把所有数据压成一列 text 或让 Flow 执行过滤、排序、格式化。

### 3.7 P1：Tabs、Dialog、Tooltip 的 IDE 关键行为尚未闭合

- Tabs 缺 close、dirty、pinned、overflow、stable tab ID 和 close rejection。
- Dialog/Modal 缺统一 result channel、focus trap、backdrop pointer blocking 和 `request_id/session_id/job_id` 关联。
- Tooltip 缺 delay、keyboard focus、collision 和不阻塞底层交互的明确 policy。

这些能力对普通展示不是 P0，但对 IDE agents 的“打开文档、审批命令、关闭未保存内容、查看工具说明”是必需行为。

## 4. agents 模块需要新增或扩展的组件

下面只列真正需要 first-class contract 的能力。能用现有组件可靠组合的，不建议马上增加新 kind。

### P0：必须增加

#### 4.1 `TreeFrame` / TreeView 数据协议扩展

不一定新增 `file_tree` kind，优先把现有 TreeView 变成可承载项目树的通用组件。

至少需要：

- `tree_id`、`tree_revision`、`epoch`；
- stable node ID、parent ID、kind、label/text handle、icon key；
- `has_children`、`expanded`、`loading`、`error`、`disabled`、`selected`；
- bounded visible window 或 bounded child page；
- `tree.expand`、`tree.select`、`tree.rename`、`tree.create`、`tree.delete` semantic intents；
- context menu target 使用 stable node ID，不使用 renderer hit ID；
- keyboard focus、single/multi-select 规则；
- stale tree revision 和 lazy-load failure 的稳定错误码。

#### 4.2 `CodeEditor` 正式 document/completion contract

这不是新增视觉组件，而是必须完成现有 `code_editor`：

- document handle/frame；
- source hash、document revision、editor session、epoch；
- diagnostics frame；
- completion set frame；
- draft/commit ChangeSet；
- accepted/rejected/conflict/reopen；
- clipboard、IME、selection、caret 的 renderer/UI bridge；
- no file path in Flow；
- completion and grammar single source。

#### 4.3 `DiffView` / `PatchReview`

agents 必须让用户审阅和批准修改。建议新增 renderer-neutral `diff_view`，而不是用普通 `code_editor` 猜 diff：

- document ID、base revision、patch revision、hunk ID；
- old/new line number、line kind、bounded text handle；
- hunk collapsed/expanded；
- per-hunk accept/reject；
- whole patch accept/reject；
- conflict/changed-after-preview 状态；
- stable `diff.hunk.accept` / `diff.hunk.reject` intents；
- domain 负责 patch apply，renderer 只负责显示和局部选择。

第一版可以只支持 unified two-column 或 inline 一种布局，但必须先冻结数据帧和 revision 规则。

#### 4.4 `AgentMessageList` / `ConversationFrame`

普通 `repeat` 或 DataGrid 无法稳定承载 streaming agents 对话。需要 bounded append/window contract：

- conversation/session ID、sequence、revision、epoch；
- message ID、role、state（streaming/completed/failed/cancelled）；
- text handle segments，而不是把未受限正文塞进普通 scalar input；
- code block、artifact、citation、tool-call reference 的 typed metadata；
- message action intents（retry、copy、open artifact、cancel generation）；
- tail-follow / user-scrolled-away 状态；
- chunk sequence gap、backpressure、overflow diagnostics。

WGPU 可以本地绘制已收到的 presentation，但 agent runtime/domain 仍是消息真相。

#### 4.5 `AgentToolCall` / `ApprovalPrompt`

工具执行和权限批准不能只表现为一个 Spinner。需要可组合或 first-class 的 typed presentation：

- tool call ID、agent session、job ID、request ID；
- tool name、bounded arguments summary、risk level；
- queued/running/succeeded/failed/cancelled/awaiting_approval；
- approve、deny、cancel、retry intents；
- timeout、epoch change、stale approval 的稳定错误码；
- sensitive argument 默认脱敏，不能进入 trace 或 Flow source。

视觉上可先由 `Panel + Dialog + Button + Spinner + Text` 组成，但协议和状态必须先独立定义。

### P1：建议增加或补强

#### 4.6 `TerminalView` / `LogView`

用于 agent tool output、build output、test output：

- append-only bounded window；
- sequence/gap/overflow；
- stdout/stderr/diagnostic level；
- ANSI 或受限 span presentation；
- follow-tail、pause、clear、copy；
- search 不在 Flow 内执行，由 host/domain 提供结果帧。

可以复用 editor 的行虚拟化，但不要复用 editor 的文档 commit 语义。

#### 4.7 `CommandPalette` / `SearchPalette`

适用于 IDE 命令、agent action、文件/符号搜索：

- TextInput + Popup + bounded candidate frame；
- keyboard navigation、Escape、Enter、disabled candidate；
- candidate ID、display handle、detail、shortcut、provider；
- host/domain 提供排序和过滤结果，NUI 不执行 fuzzy search；
- stable selection and invoke intent。

如果第一阶段只做一个 palette，优先做通用 `Popup + ListBox + TextInput` contract，而不是单独堆视觉 kind。

#### 4.8 `DocumentTab` 能力

现有 Tabs 需要扩展 stable tab frame，而不是只加一个 close icon：

- tab ID、document ID、title handle、dirty、pinned、active、loading/error；
- close intent、close confirmation、close rejected；
- overflow and keyboard navigation；
- optional dirty marker and agent task marker。

#### 4.9 `Badge` / `StatusChip` / `Avatar`

这类是 P1 presentation 组件，适合 agent role、job status、permission level、severity。它们不应先于 TreeFrame、document frame、diff和approval contract施工。

## 5. 不建议当前新增的组件

以下能力可以暂时通过现有组件组合，新增 kind 的收益不足以抵消 contract 数量：

- 普通 `Toolbar`：先用 `Panel row`。
- 普通 `Popover`：先统一 `Popup` top-layer contract。
- `ScrollView` 新 kind：当前 `scroll` 已是 canonical scroll container。
- `NumberInput`：先补 `DragValue` 的 step、keyboard increment/decrement 和 typed commit。
- `Switch` 新视觉变体：先补已有 Switch 的内建交互和 typed toggle。
- `Toast` 新 kind 变体：先完成 notification queue/lifecycle。
- `Accordion` 新 kind 变体：先完成内建 state/presentation/event。

## 6. 建议施工顺序

### M0：contract freeze

1. 更新 NUI Flow closed vocabulary、attribute 表和 input kind 表。
2. 生成 parser/formatter/editor completion 共用 grammar table。
3. 为每个组件建立机器可读 capability matrix 和 acceptance level。
4. 明确哪些是正式 V1、兼容 slice、demo-only、planned。

### M1：IDE shell 基础

1. `Panel + scroll + Splitter`：完成 min/max、resize、persist、commit/reject、headless/window parity。
2. `TreeView`：完成 `TreeFrame`、selection、lazy expand、context menu target、keyboard navigation。
3. `Tabs`：完成 document tab frame、close、dirty、overflow。
4. `ContextMenu + Popup`：完成 focus、Escape、outside click、keyboard、viewport clamp。

### M2：正式 editor

1. 移除 `source_file` Flow 属性和 literal source workaround。
2. 接入 document/completion 专用 frame。
3. 接通 editor runtime diagnostics/subscription/dynamic completion。
4. 增加 document service probe 和 code-editor window probe。

### M3：agents data views

1. `ConversationFrame` / `AgentMessageList`。
2. `AgentToolCall` / `ApprovalPrompt`。
3. `DiffView` / `PatchReview`。
4. `TerminalView` / `LogView`。

### M4：视觉增强

1. StatusChip、Badge、Avatar。
2. notification queue and Toast lifecycle。
3. text state style、selection/caret theme、统一 component state style。
4. 组件 animation 和 real window visual acceptance。

## 7. 必须补的验收入口

已有 editor protocol probe 和 NUI Flow diagnostics probe 证明了 loopback、revision、idempotency、stale completion 和 parse diagnostics 的一部分链路，但还不足以验收 IDE/agents UI。

建议新增以下 executable probes/scenarios：

| Probe / scenario | 最小断言 |
| --- | --- |
| `ide_shell_layout_probe` | Splitter resize 后两边 bounds、clip、hit、revision 相同；重启后从 snapshot 恢复 |
| `tree_frame_probe` | lazy expand、stable node ID、selection、rename conflict、epoch change、context target |
| `code_editor_document_probe` | document frame 不来自 literal；draft/commit/conflict/diagnostics/completion 全带 revision/epoch |
| `diff_review_probe` | hunk accept/reject 一次一事件，stale patch 被拒绝，domain apply 后回传新 revision |
| `agent_conversation_probe` | chunk sequence、stream completion、cancel、gap/overflow、tail-follow 状态可追踪 |
| `agent_approval_probe` | approve/deny/cancel 的 request/job/session 关联和重复幂等 |
| `terminal_window_probe` | append window、tail scroll、gap/overflow、copy/clear、PNG/JSONL 证据 |
| `ide_agents_window_probe` | 最终合成 target 非黑屏，popup 不遮挡错误，diff/approval/message list 可见 |

所有跨进程 probe 应输出 JSONL，至少包含：`request_id`、`session_id`、`job_id`、`epoch`、`sequence`、`document/tree/list revision`、producer value、consumer value、frame pairing 和 `pass_result`。不要用固定 sleep 判断 ready。

## 8. 当前验证结果

### 通过

- `cargo test --quiet -p neon-ui-schema`：39 passed。
- `cargo test --quiet -p neon-ui-runtime nui_flow::tests:: -- --test-threads=1`：97 passed，77 filtered out。
- `cargo test --quiet -p neon-editor`：47 passed。
- `cargo test --quiet -p neon-editor-runtime`：无失败测试输出；当前 crate 的主要行为由 probe 覆盖。
- `cargo run --quiet -p neon-editor-runtime --bin editor_protocol_probe`：JSONL 全部 `pass_result:true`，最终 `final.pass_result:true`，覆盖 open、draft、idempotency、revision conflict、commit、completion、stale completion、shutdown。
- `cargo run --quiet -p neon-ui-runtime --bin nui_flow_diagnostics_probe`：三个 compile/submit 场景均 `pass_result:true`，覆盖有效 Flow 和结构化 parse diagnostic。
- `cargo check --quiet --workspace --all-targets`：未观察到编译失败，存在 warning。

### 未通过或未完成

- `cargo test --quiet -p neon-wgpu-runtime --lib`：在 213 个测试中出现多项 renderer/offscreen 失败，并在最后一个长测试超过 120 秒工具超时；至少已观察到 world panel、drop materialization、font preload、offscreen pixels、disabled style、checksum、component gallery、DataGrid interaction、numeric commit、atlas image 等失败项。由于本次没有修改 renderer，报告不把这些失败归因到单一根因，但不能把 WGPU 组件层标记为全量通过。
- 没有执行真实 IDE window probe、IME 人工验收、agents window composition 验收；因此当前最高只能认定为部分 `contract-ready` / `service-ready`，不能认定 `interactive-accepted`。

### 警告摘要

- `neon-editor` grammar 有未使用参数，且 grammar table 仍是 provisional duplicate。
- `neon-ui-runtime` 有未使用 editor viewport 参数、重复 `#[test]` 属性和未使用函数。
- `neon-wgpu-runtime` 有未使用 `ComponentStateStore` 字段、未使用 renderer diagnostics/helper、重复 test attribute 以及大量 dead-code warning。

## 9. 最终判断

当前最值得投入的不是再增加一个通用小控件，而是完成以下闭环：

```text
single grammar source
  -> stable NUI Flow contract
  -> document/tree/list/agent typed frame
  -> renderer-local preview
  -> reliable semantic intent
  -> domain accepted/rejected revision
  -> structured trace + window probe
```

对 IDE agents 模块而言，优先级排序是：

1. `code_editor` 正式 document protocol；
2. TreeView revisioned data frame；
3. DiffView / PatchReview；
4. AgentMessageList / ConversationFrame；
5. AgentToolCall / ApprovalPrompt；
6. TerminalView / LogView；
7. Tabs、Popup、ContextMenu、Dialog 的完整键盘和生命周期 contract；
8. 最后再做 Badge、Avatar、装饰性 text effect 等视觉增强。

## 10. 本轮已实际补齐的半成品行为

本轮没有把“存在 renderer 分支”继续当成完成，而是补了可观察的交互闭环：

- **Switch**：接入与 Checkbox 相同的 renderer-local toggle prediction、`control_value.bool`、fragment replacement 后的本地状态保持路径。
- **TreeView child**：展开/收起由统一 toggle release 路径处理，semantic event 现在携带新的展开 bool，不再只改 WGPU 私有 map。
- **MenuBar / Popup**：MenuBar 直接子节点点击时 local open/close；Popup hidden nodes 保留在 plan 中并按 active popup filter，outside click 和 popup item release 会关闭。
- **Accordion**：直接子节点 header 的 local expanded state、content subtree 隐藏和 plan interaction revision 已接入；默认仍允许 domain 新 fragment 覆盖最终结构。
- **Splitter**：Down/Move/Up/Cancel 已走完整 renderer-local drag 生命周期；Up 产生归一化 `F32 0..1` semantic payload，Cancel 恢复 pointer-down bounds；windowed 和 headless external 两条入口都接入。
- **Interaction plan refresh**：Popup/Accordion local topology 改变会推进 renderer-local plan revision，避免 fragment revision 不变时 refresh early-return 导致视觉不更新。
- **Executable probe**：新增 [`component_interaction_probe.rs`](../crates/neon-wgpu-runtime/src/bin/component_interaction_probe.rs)，真实经过 headless external GPU、surface open、fragment submit、pointer Down/Up 和 semantic event。

本轮仍没有把以下能力伪装成完成：正式 `code_editor` document frame、revisioned TreeFrame、Splitter semantic commit、Toast queue/TTL、完整 Popup keyboard focus。这些仍是下一批独立 contract 工作。

当前主推进已开始补正式 CodeEditor document frame：schema 已有 `UiEditorDocumentBinding`，presentation/commit 已携带文档身份和 ChangeSet，EditorBridge 已支持非阻塞 provider/cache，`neon3-runtime` 已有后台 editor-runtime RPC provider；cache miss 时会在 worker 中执行 `document.open`。端到端 document open/apply/conflict/reveal 验收仍未完成，因此 CodeEditor 当前状态更新为“authority bridge in progress”，不是 complete。

新增 `code_editor_authority.v1` 端到端 probe 后，首次 open/snapshot/presentation 子链路已通过：真实 `neon3-runtime` 提交 Flow 后，WGPU presentation 观察到 `document_id`、`epoch`、`revision=1`、`source_hash` 和 source，与 editor-runtime snapshot 配对一致。ChangeSet apply、revision conflict、reveal 仍需下一阶段 probe。
