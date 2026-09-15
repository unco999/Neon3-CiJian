# Neon3 NUI Flow IDE 多行代码编辑器施工文档

> 版本：v1.0
> 日期：2026-09-15
> 状态：施工进行中，M1 内核与 M2 declaration contract 子集已完成
> 影响层：schema、editor domain、UI declaration、WGPU composition

## 0. 结论先行

Neon3 的 NUI Flow 多行代码编辑器不能只做成一个“带多行文本的
`TextInput`”。它需要同时具备：

- WGPU 进程内立即响应的文本编辑交互；
- 无窗口、可单测的文本缓冲、词法高亮、符号索引和补全内核；
- 无窗口的编辑器领域服务，负责文档 revision、诊断、补全和 draft/commit 语义；
- UI Runtime 对 Flow 声明、文档帧和 ChangeSet 的协议校验；
- WGPU Runtime 对可见行、光标、选区、IME、补全弹层和诊断标记的最终绘制；
- 当前不包含文件写入、项目服务或持久化职责。

当前阶段只完成编辑器逻辑和组件契约：内存文档、编辑操作、选区、撤销重做、
语法高亮、诊断、补全、revision 和可靠控制面。暂不启动 UI 预览，不实现文件保存，
不依赖任何项目文件服务。后续有窗口后，再把这些逻辑接到 WGPU 组件并做真实窗口测试。

本文件是施工主文档。`docs/nui-flow-code-editor.md` 保留为背景设计稿，实施时
以本文件的边界、字段和验收标准为准。

---

## 1. 当前跟进结果

### 1.1 GLM 已经完成的内容

当前工作区已有以下未提交改动：

| 内容 | 当前状态 | 证据 |
| --- | --- | --- |
| `neon-editor-core` workspace crate | 已创建，M1 子集可用 | `crates/neon-editor-core/` |
| 基础多行文本缓冲 | 已有 | `src/buffer.rs`，按行保存，位置使用 Unicode scalar column |
| 编辑操作和 undo/redo | 已有雏形 | `src/edits.rs` |
| NUI Flow 行 tokenizer | 已有雏形 | `src/highlight.rs` |
| 增量高亮 cache | 已有雏形 | `HighlightCache::update` |
| 文档符号索引 | 已有雏形 | `src/symbols.rs` |
| 上下文补全 | 已有雏形 | `src/completion.rs` |
| `EditorCore` facade | 已有雏形 | `src/lib.rs` |
| Flow `#` 行尾注释 | parser 已支持 | `neon-ui-runtime/src/nui_flow.rs` |
| `code_editor` declaration contract | 已接入 schema/parser/formatter/lowering，仍为 Panel compatibility node | `UiCodeEditorDeclaration` + `UiIrDocument.code_editors` |
| core 单测 | `33 passed` | `cargo test --quiet -p neon-editor-core` |

### 1.2 尚未完成的内容

以下内容当前**不存在或尚未接通**，不能对外宣称 `code_editor` 已完成：

- 完整文档帧和动态补全帧；当前 editor protocol 已有 open/snapshot/change/commit/completion 子集；
- `document` / `completion_set` 输入类型或等价的专用绑定 effect；
- `neon-editor-runtime` 的诊断发布、动态补全和订阅能力；当前基础服务已建立；
- 文档 revision、draft/commit、冲突恢复链路；
- WGPU 多行文本布局、行号、选区、光标、波浪线和补全弹层；
- 完整键盘编辑、系统剪贴板和中文 IME 多行行为；
- Playground/预览热更新闭环；该内容不属于当前编辑器逻辑阶段；
- 更完整的窗口输入 probe；当前已有 loopback editor protocol probe；
- 真实窗口 PNG/JSONL 验收。

### 1.3 当前雏形的技术风险

这些不是本轮直接修复项，但在进入下一切片前必须处理或明确记录：

1. `TextBuffer` 当前是 `Vec<String>`，不是 rope/piece table。10k 行基准通过前，
   不得把“增量编辑”描述成已经达到 IDE 性能。
2. `FlowGrammar` 当前在 `neon-editor-core` 中有临时表，和
   `neon-ui-runtime/src/nui_flow.rs` 的 parser 不是单一数据源。
3. highlighter 的 span 使用字符列，但测试/渲染适配必须禁止把字符列直接当作
   UTF-8 byte offset；中文、组合字符和 IME 必须有明确转换测试。
4. 当前补全已具备稳定 ID、`insert_text` 和 replacement range，但还没有
   editor protocol 的 document revision 和 stale-response 保护，不能直接跨进程使用。
5. 当前 `EditorCore::take_change_set()` 的 full resync 机制适合 headless 核心，
   还没有和公开 RPC 的 `request_id`、`idempotency_key`、epoch、revision 对齐。
6. 当前 parser 的错误路径偏向遇到第一处错误即返回；IDE 需要有界错误恢复，
   在一份坏文档中尽量返回多个诊断；当前阶段不启动 compile gate。

---

## 2. 产品范围和明确非目标

### 2.1 V1 必须交付

1. 编辑 `nui_flow` 语言的多行文档。
2. 光标、选区、鼠标拖选、双击选词、Home/End、上下左右移动。
3. 插入、删除、换行、自动缩进、Tab/Shift+Tab、Backspace/Delete。
4. Ctrl/Cmd+A/C/X/V/Z/Y、系统剪贴板和本地 undo/redo。
5. Windows 中文 IME：Enabled、Preedit、Commit、Disabled。
6. 行号、当前行、可见行虚拟化、横向滚动、纵向滚动、裁剪。
7. token 高亮：关键字、节点类型、节点 key、属性、输入引用、颜色、数字、
   字符串、intent、注释、非法 token。
8. 诊断波浪线、行号 gutter 标记、状态栏错误计数。
9. 上下文补全：顶层语句、节点类型、属性、`$input`、输入类型、声明的 intent。
10. Ctrl+Space 手动补全、自动触发、上下键、Enter/Tab 接受、Escape 关闭。
11. 宿主动态补全，例如 editor session 提供的文档符号或 intent 集合。
12. 文档 draft/commit、revision conflict 和内存恢复。
13. 带窗口后验证编辑器像素和交互；当前阶段不启动 UI 预览、不编译被编辑 Flow。

### 2.2 V1 不做

- 多光标、矩形选择、多 caret；
- 代码折叠、minimap、全文搜索替换 UI；
- LSP、插件脚本、任意语言执行器；
- 在 Flow 中写 JavaScript/Rust/Lua、表达式、回调或文件路径；
- 自动执行编辑中的 Flow；
- 通过 renderer-local hit ID 识别代码符号；
- 将每个按键变成跨进程 RPC；
- shader 参数补全、图片 cross-fade、scroll/clip keyframe；这些属于动画计划的
  独立边界，不作为编辑器完成条件。

---

## 3. 进程和所有权

### 3.1 目标进程图

```text
                         public neon3.rpc / neon3.event
                                      |
                         +------------------------+
                         |                        |
                   neon-ui-runtime          neon-editor-runtime
                   Flow declaration         document session
                   UI bridge/validation     diagnostics/completion
                         |                        |
                         +---- typed editor/render updates ----+
                                      |
                            neon-wgpu-runtime
                         only window + only WGPU
```

`neon-editor-core` 是被多个进程复用的纯库，不是业务进程：

```text
neon-editor-core
  no window
  no wgpu
  no filesystem
  no network
  no file mutation
  deterministic buffer/highlight/symbol/completion/edit primitives
```

### 3.2 权威状态表

| 状态 | 唯一权威 | 允许谁修改 |
| --- | --- | --- |
| 原始文档正文、document revision | `neon-editor-runtime` | 可靠 editor RPC |
| parse 诊断 | `neon-ui-runtime` | UI Flow parser/validator |
| 动态补全集合 | editor domain 或项目资源 owner | revisioned completion RPC |
| 光标、选区、preedit、补全弹层选择 | `neon-wgpu-runtime` | 本地输入事件 |
| 最终像素、glyph atlas、clip、hit-test | `neon-wgpu-runtime` | WGPU render loop |

### 3.3 禁止事项

- `neon-editor-runtime` 不创建窗口、不创建 wgpu resource。
- `neon-ui-runtime` 不把代码正文塞进普通 `UiInputFrame`。
- `neon-wgpu-runtime` 不解析 Flow 语法、不决定语义诊断、不写文件。
- 编辑器客户端不直接访问文件系统。
- 光标位置、物理像素、hit ID、GPU handle 不进入 document ChangeSet。
- 不把 React/Tauri callback 或 UI element ID 定义成编辑器协议。

---

## 4. 用户可见产品结构

后续带窗口测试的编辑器 shell 先只包含编辑区域和状态栏，不包含预览区域：

```text
+----------------------------------------------------------------+
| toolbar: document | revision | diagnostics | commit          |
+--------------------------+-------------------------------------+
|                          |                                     |
|  code_editor (full width)                                      |
|  line numbers                                                   |
|  syntax highlight                                               |
|  selection/caret           |                                     |
|  completion popup          |                                     |
|                          |                                     |
+--------------------------+-------------------------------------+
| status: clean / draft / committed / error / conflict            |
+----------------------------------------------------------------+
```

编辑器 shell 本身可以是一个 NUI Flow 文档；被编辑的 Flow 文档是另一个独立的
document session。当前只验证组件声明和逻辑服务，不启动 shell 预览。

---

## 5. NUI Flow 声明设计

### 5.1 推荐语法

文档正文不写进 Flow。Flow 只声明编辑器绑定到哪个稳定 document handle：

```text
version 1
surface ide-shell revision 1

input active_document document default document:empty
input read_only bool default false
input dynamic_completions completion_set default completion_set:empty

surface ide column w 1440 h 900 gap 8 pad 12 fill #101820
  panel toolbar row h 40 gap 8
    text title value "NUI Flow Editor"
  panel body row grow 1 gap 8
    code_editor source_view source $active_document language nui_flow
      line_numbers true
      wrap none
      font_size 14
      tab_size 2
      read_only $read_only
      completions $dynamic_completions
      gutter_diagnostics true
      event editor.document.commit
      
```

约定：

- `source` 改为 `document` 绑定，值是 opaque document handle，不是正文；
- `language nui_flow` 是 V1 唯一语言，未来扩展必须新增 capability；
- `event` 只表达语义通知，不携带光标、像素、hit ID 或任意 payload；
- `read_only` 只控制本地编辑是否允许，复制、滚动、选择仍可用；
- `gutter_diagnostics` 只是 presentation 开关，诊断来源仍是 domain/parser；
- `wrap none` 为 V1 唯一正式布局，软换行推迟到 V2，避免列映射和鼠标命中复杂化。

### 5.2 Schema 类型

在 `neon-ui-schema` 增加以下 renderer-neutral 类型。字段必须
`serde` 可序列化，禁止嵌入 renderer handle。

```rust
pub struct UiDocumentHandle {
    pub document_id: String,
    pub revision: Revision,
    pub language: String,
}

pub struct UiCodeEditorDeclaration {
    pub node_id: UiNodeId,
    pub document_input_key: String,
    pub language: UiEditorLanguage,
    pub line_numbers: bool,
    pub wrap: UiEditorWrap,
    pub font_size: f32,
    pub tab_size: u8,
    pub read_only_input_key: Option<String>,
    pub completion_input_key: Option<String>,
    pub gutter_diagnostics: bool,
}

pub enum UiEditorLanguage {
    NuiFlow,
}

pub enum UiEditorWrap {
    None,
}
```

实现时不要把这些字段加入 `UiStyle`。编辑器配置是 component declaration，
不是普通 panel visual style。

### 5.3 输入类型选择

正式版本中 `document` 和 `completion_set` 不得复用普通 `text`。当前 M2
compatibility slice 暂时使用 `TextHandle` 作为稳定占位，以便先冻结声明和
lowering；M3 前必须替换为带 document/completion 语义的专用 control-plane frame：

- `text` 继续只表示稳定的 `UiTextHandle`；
- `document` 表示 editor domain 的稳定文档引用；
- `completion_set` 表示有界、带 revision 的候选集引用；
- 正文和补全正文通过专用 editor protocol 帧传递；
- 大文本不得进入 `UiInputFrame` 的普通标量 slot。

如果直接在 `UiInputKind` 增加类型导致普通 GPU input packing 被迫支持大文本，
改用 `UiEffect::CodeEditorBinding` + document handle registry；不要把字符串
按固定 GPU slot 上传。

---

## 6. 文档协议和 revision

### 6.1 文档快照

`neon-editor-runtime` 提供有界文档快照：

```json
{
  "document_id": "doc.playground.main",
  "session_id": "editor-session-uuid",
  "language": "nui_flow",
  "epoch": 3,
  "revision": 18,
  "line_count": 42,
  "byte_length": 1396,
  "source_hash": "sha256:...",
  "source": "version 1\n...",
  "diagnostics": []
}
```

约束：

- 第一版最大正文 4 MiB、最大 100,000 行；超限返回稳定错误码；
- 当前逻辑 slice 直接返回完整 source；chunk streaming 是大文档优化项，不是当前完成条件；
- 文本必须是 UTF-8；禁止 NUL；换行规范化为 `\n`；
- 快照必须包含 `epoch`、`revision`、`source_hash`；
- source 只用于 editor control plane，不进入 GPU buffer；
- 诊断包含 line/column/end、severity、code、message、suggestion；
- 当前阶段不编译、不预览；WGPU 只在后续窗口 slice 消费 editor presentation frame。

### 6.2 ChangeSet

```json
{
  "document_id": "doc.playground.main",
  "session_id": "editor-session-uuid",
  "epoch": 3,
  "base_revision": 18,
  "change_set_id": "changeset-uuid",
  "kind": "draft",
  "ops": [
    {
      "kind": "insert",
      "start": {"line": 8, "column": 2},
      "text": "  text title value \"Hello\"\n"
    },
    {
      "kind": "delete",
      "start": {"line": 12, "column": 0},
      "end": {"line": 12, "column": 8}
    }
  ],
  "cursor": {"line": 8, "column": 34},
  "selection": null
}
```

正式协议类型应使用 Rust struct，不使用上述自由 JSON。位置 V1 统一为
0-based Unicode scalar column；每层都必须有中文和 emoji 的转换测试，不能
把它误当成 UTF-8 byte column 或 UTF-16 LSP column。

ChangeSet 规则：

- `base_revision` 不匹配必须返回 `editor_revision_conflict`；
- `change_set_id` 是幂等键的一部分，重复请求返回原结果；
- `request_id`、`idempotency_key`、client identity、epoch 必须在 RPC envelope；
- 单次最多 256 个 op，单 op 插入最多 64 KiB；
- delete 必须是半开区间，空区间是明确错误或被规范化为 no-op，不能 silent no-op；
- `draft` 可在 debounce 中合并，但不能悄悄丢失编辑；合并后 sequence 必须可追踪；
- `commit`、`undo`、`redo` 使用可靠 RPC；这里的 commit 只确认 editor session 基线，不写文件；
- 服务 epoch 变化后，WGPU 取消本地 pending composition/capture，重新拉取快照。

### 6.3 方法集合

第一版 editor service 至少提供：

```text
service.health
service.describe
editor.document.open
editor.document.snapshot.get
editor.document.change.apply
editor.document.change.commit
editor.document.close
editor.completion.request
editor.completion.resolve
editor.subscribe
debug.snapshot.get
debug.trace.subscribe
debug.trace.query
debug.command.get
debug.diagnostics.get
```

当前阶段没有保存流程。文件来源、文件监视和持久化属于未来另一个模块；本编辑器
只处理已提供的内存 source 和 editor session，不依赖、不调用、不设计 projectd。

---

## 7. 诊断和逻辑解析

### 7.1 两类解析

当前编辑器逻辑只需要容错诊断解析：

1. 用户正在输入时尽量继续扫描并返回多个错误；
2. 不产生可渲染 IR，不启动预览；
3. 严格 compile gate 属于未来 UI/预览阶段，不是本阶段依赖。

两者必须共享词法和 grammar table，但不能让容错 parser 产生可渲染的非法 IR。

### 7.2 诊断结构

复用 `NuiFlowParseDiagnostic` / `UiDiagnostic` 的字段语义，补充 editor context：

```rust
pub struct UiEditorDiagnostic {
    pub diagnostic_id: String,
    pub document_id: String,
    pub revision: Revision,
    pub severity: UiDiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub span: NuiSourceSpan,
    pub suggestion: Option<String>,
    pub source: UiDiagnosticSource,
}

pub enum UiDiagnosticSource {
    Lexer,
    FlowParser,
    FlowCompiler,
    EditorProtocol,
}
```

稳定错误码至少包括：

```text
editor_document_not_found
editor_document_too_large
editor_invalid_utf8
editor_revision_conflict
editor_epoch_stale
editor_change_set_invalid
editor_change_set_overflow
editor_read_only
editor_completion_stale
nui_flow_unknown_keyword
nui_flow_unknown_component
nui_flow_unknown_attribute
nui_flow_invalid_literal
nui_flow_invalid_binding
nui_flow_mixed_indentation
nui_flow_unclosed_string
```

### 7.3 当前诊断发布规则

- 新输入先改变 WGPU 本地编辑镜像和高亮；
- editor runtime 接收 draft/commit 后发布 document revision；
- 逻辑 parser 发布与当前 document revision 对齐的 diagnostics；
- 旧异步结果不能覆盖新 revision 的 diagnostics；
- 当前没有预览状态、保存状态或编译提交状态。

---

## 8. `neon-editor-core` 施工设计

### 8.1 公共 API 目标

保持当前 facade 方向，但补齐协议无关的数据类型：

```rust
pub struct EditorCore { ... }
pub struct TextBuffer { ... }
pub struct Position { line: u32, column: u32 }
pub struct Selection { anchor: Position, active: Position }
pub struct EditOp { ... }
pub struct ChangeSet { ... }
pub struct CompletionItem { ... }
pub struct CompletionContext { ... }
pub struct TokenSpan { ... }
pub struct DiagnosticSpan { ... }
```

`EditorCore` 只负责：

- 本地文本镜像；
- 编辑和 undo/redo；
- 光标/选区辅助计算；
- token/highlight cache；
- document symbol index；
- 静态 completion candidate 计算；
- 输出 ChangeSet。

它不负责：

- RPC、socket、文件；
- WGPU buffer、glyph atlas、窗口事件；
- parser 的项目业务规则；
- 启动窗口、渲染预览或写入任何外部状态。

### 8.2 Buffer 实现策略

第一刀保持 `TextBuffer` API 不变，先补齐行为和基准；内部实现按以下顺序推进：

1. 修复 Unicode scalar column 与 byte index 的所有边界测试；
2. 增加可合并的 edit transaction 和 position mapping；
3. 对 1k/10k/100k 行执行插入、跨行删除、undo、redo benchmark；
4. 若超过预算，再在 `TextBuffer` 后替换为 rope/piece table，禁止污染上层 API；
5. document full resync 只作为 revision 冲突恢复路径，不作为每次按键路径。

### 8.3 高亮

静态 grammar table 的唯一来源应落在 `neon-ui-schema` 或独立无 GPU grammar
模块，至少包含：

- 顶层 keyword；
- node kind；
- common attributes；
- per-node attributes；
- input kinds；
- enum/value context；
- Flow comment、string、color、numeric、intent 词法规则。

`neon-editor-core` 的 `FlowGrammar` 改为从该表构造，不再维护临时副本。

高亮 cache 每行保存：

```text
input lexical state
token spans
output lexical state
grammar context summary
```

编辑一行后只向后传播到 output state 不再变化的行。每个 span 必须带字符列
和长度；渲染转换到 glyph position 时只能调用明确的 column mapper。

### 8.4 补全

补全 item 必须从当前 `label/kind/detail` 扩展为：

```rust
pub struct CompletionItem {
    pub item_id: String,
    pub label: String,
    pub insert_text: String,
    pub replace_start: Position,
    pub replace_end: Position,
    pub kind: CompletionKind,
    pub detail: String,
    pub sort_text: String,
    pub source: CompletionSource,
    pub commit_characters: Vec<char>,
}
```

本地静态补全必须在 16ms 内产生；远端动态补全必须带：

```text
document_id
document_revision
request_id
position
trigger_kind: automatic | invoked | trigger_character
```

响应 revision 不是当前文档 revision 时只记录 stale diagnostic，不得插入候选。

---

## 9. WGPU Runtime 施工设计

### 9.1 renderer-local editor state

每个可见 `code_editor` 实例持有一个 renderer-local state：

```text
EditorInstance
  document_id / accepted_revision
  EditorCore local_mirror
  scroll_x / scroll_y
  caret position
  selection
  preferred_x_for_vertical_move
  ime enabled / preedit
  completion popup state
  mouse capture state
  pending changeset metadata
```

它是显示和交互镜像，不是 document authority。fragment refresh 或 service epoch
变化时，必须按 document revision adoption 规则恢复，不能将本地缓存当真相。

### 9.2 绘制顺序

固定绘制顺序：

```text
editor background
current-line highlight
selection rectangles
visible line numbers
visible syntax-colored text runs
diagnostic underlines/gutter marks
caret
IME preedit underline/text
completion popup
```

所有编辑器绘制都使用 editor node 的 clip；补全 popup 必须走明确的 top-layer
composition policy，不得因为 overflow 临时关闭父级 clip。

### 9.3 行虚拟化

- 只布局 `[first_visible - overscan, last_visible + overscan]`；
- 行高来自固定 `font_size` 和 line spacing，不允许 token 改变行高；
- 行号槽宽度根据总行数的十进制位数更新，但有稳定最小宽度；
- 横向滚动只改变代码内容 x，不改变父 `ScrollView` offset；
- completion popup 锚定 caret 的逻辑坐标，滚动时同步移动；
- 诊断波浪线不能改变布局尺寸。

### 9.4 键盘、剪贴板和 IME

WGPU 当前已经拥有 `WindowEvent::Ime`、焦点和 TextInput 基础路径。多行扩展必须：

- 保持 preedit 只在 renderer-local state；
- `Ime::Commit` 变成一次本地 insert transaction；
- Enter 在普通编辑状态插入换行，在 completion popup 接受候选；
- Escape 优先关闭 completion，再取消 preedit，再取消拖选；
- Ctrl/Cmd+C 只复制选区，不触发 domain command；
- Ctrl/Cmd+V 通过 WGPU 所属窗口的 clipboard adapter 插入文本；
- Ctrl/Cmd+X 复制并删除选区；
- focus loss 清理 capture/preedit，并按策略提交或取消未提交 ChangeSet；
- IME rect 使用 caret 的逻辑 bounds，不使用整行 bounds。

### 9.5 不把按键变成 RPC

输入路径必须是：

```text
OS key/pointer/IME
  -> WGPU local EditorCore mutation
  -> local highlight/caret/completion/render immediately
  -> debounce ChangeSet
  -> reliable editor protocol
```

不得等待 editor runtime 返回后才显示输入字符。可靠链路只负责权威 revision、
诊断、preview、commit 和保存。

---

## 10. UI Runtime 和桥接施工

### 10.1 Parser/lowering

修改位置：

```text
crates/neon-ui-schema/src/lib.rs
crates/neon-ui-runtime/src/nui_flow.rs
crates/neon-ui-runtime/src/lib.rs
```

施工顺序：

1. 增加声明 struct、枚举、绑定 effect 和 capability name；
2. 把 grammar table 抽出并让 parser/formatter/completion 共享；
3. 加入 `code_editor` closed vocabulary；
4. 实现属性重复、类型错误、未知语言、越界 font/tab 等稳定 diagnostics；
5. lowering 生成 `UiCodeEditorDeclaration`，不生成动态 children；
6. `format_nui_flow` 保留注释并输出稳定 canonical order；
  7. 校验 document binding，但不把 document 正文编译进 Flow IR；
8. 增加 JSON round-trip 和 old document compatibility tests。

### 10.2 Bridge

不要把 editor domain 直接写进 `UiHostAdapter` 的普通输入 slot。新增明确的
inbound/outbound variant，例如：

```rust
UiHostInbound::CodeEditorChangeSet { change_set }
UiHostPublication::CodeEditorDocument { document }
UiHostPublication::CodeEditorDiagnostics { diagnostics }
```

这些 variant 必须包含 document/session/revision/epoch，并由 UI Runtime 做 schema
校验后转发。UI Runtime 不应用 ChangeSet，不维护另一份正文。

---

## 11. 当前阶段：纯逻辑验收

### 11.1 责任

`neon-editor-runtime` 只提供内存 document session、ChangeSet、revision、诊断和
静态补全。它不打开窗口、不渲染预览、不编译被编辑 Flow、不读写文件，也不依赖
任何 project service。

`neon-wgpu-runtime` 的窗口测试延后到 M4/M5；在此前，所有行为用
`neon-editor-core` 单测和 editor loopback probe 验证。

### 11.2 队列策略

当前不实现 compile/preview queue，只保留一条有序 editor RPC lane：

```text
local edit: immediate in EditorCore
draft ChangeSet: bounded and ordered
commit: reliable RPC, editor baseline only
completion: revision-bound request/response
RPC timeout: 2 s normal
```

超时、取消和 stale 结果必须有结构化 trace，不能通过增加等待时间掩盖。

### 11.3 逻辑状态

```text
revision 1 opened -> clean
revision 2 draft accepted -> dirty/draft
revision 2 commit accepted -> clean/committed
stale revision -> editor_revision_conflict
```

状态只描述内存编辑 session，不代表文件保存或 UI 预览成功。

---

## 12. 结构化验收入口

跨进程、窗口或 renderer 行为必须有 `src/bin/` 可执行 probe；不能只靠单测或手点。

建议新增：

```text
crates/neon-editor-runtime/src/bin/editor_protocol_probe.rs
crates/neon-ui-runtime/src/bin/code_editor_service_probe.rs
crates/neon-wgpu-runtime/src/bin/code_editor_window_probe.rs
crates/neon-dev/src/bin/nui_playground_probe.rs
```

### 12.1 JSONL 通用记录

每条记录至少包含：

```json
{
  "probe": "editor-protocol.v1",
  "sequence": 12,
  "epoch": 3,
  "request_id": "uuid",
  "document_id": "doc.playground.main",
  "revision": 19,
  "frame_sequence": 882,
  "input": {},
  "producer": {},
  "consumer": {},
  "diagnostics": [],
  "result": "passed",
  "pass_result": true
}
```

编辑器不是 depth bug，但跨进程行为仍要同时输出 producer/consumer：

- producer：本地 buffer text hash、cursor、selection、ChangeSet base revision；
- consumer：editor runtime accepted revision、diagnostics revision、completion revision；
- frame pairing：逻辑 request sequence 与 document revision 的对应关系；
- final：pass/fail 和失败稳定 code。

### 12.2 Probe 固定场景

`editor_protocol_probe` 必须覆盖：

1. `open` 返回完整快照；
2. 插入/删除/换行/中文/emoji 的 ChangeSet；
3. 重复 idempotency key 不重复应用；
4. stale revision 返回 `editor_revision_conflict`；
5. service epoch 变化后旧 ChangeSet 被拒绝；
6. draft 继续编辑，commit 只更新 editor baseline；
7. 当前不提供 save/file mutation；
8. completion request 的 stale response 被丢弃；
9. close/reopen 能恢复最后 accepted snapshot。

`code_editor_service_probe` 必须覆盖：

1. `code_editor` Flow 声明 parse/lower/serialize；
2. unknown attribute、wrong input kind、invalid range diagnostics；
3. comment/string/color 的高亮与 parser 结果一致；
4. draft/commit 只改变 editor session，不触发 WGPU 或其他服务；
5. 多个诊断和 source span 精确可定位；
6. 多个诊断的 source span 精确可定位。

`code_editor_window_probe` 必须覆盖：

1. 真实窗口和真实 WGPU text pipeline；
2. 10,000 行只绘制可见行 + overscan；
3. 行号、彩色 token、当前行、caret、selection、diagnostic underline；
4. Ctrl+Space popup、键盘选择、候选插入；
5. scroll 后 caret/popup/诊断位置配对；
6. 中文 IME preedit/commit；
7. PNG capture 非黑屏、无明显重叠、无父容器越界。

### 12.3 声明式 scenario

```yaml
id: ui.code-editor.highlight-and-complete.v1
source_fixture: fixtures/editor-playground.nui
steps:
  - target: editor-runtime
    method: editor.document.open
    params: { document_id: doc.playground.main }
    expect: { revision: 1, language: nui_flow }
  - target: editor-runtime
    method: editor.document.change.apply
    params: { line: 2, column: 2, text: "slide", kind: draft }
    expect:
      local_highlight_contains: [ident]
  - target: editor-runtime
    method: editor.completion.request
    params: { line: 4, column: 7, trigger_kind: invoked }
    expect_completion: slider
  - apply_local_completion: slider
  - target: editor-runtime
    method: editor.document.change.commit
    expect: { status: accepted }
  - target: editor-runtime
    method: editor.document.change.commit
    expect: { status: accepted, revision: 2 }
```

---

## 13. 性能、安全和可观测性预算

### 13.1 性能预算

目标硬件为当前 Windows x86_64 开发机，先记录基线再调优：

| 指标 | 预算 |
| --- | ---: |
| 本地按键到可见 caret/text | p95 < 16 ms |
| 单行高亮更新 | < 1 ms，10k 行文档 |
| 静态补全 | < 16 ms |
| completion popup 首帧 | < 16 ms |
| 可见行 layout | 只处理可见 + 2 屏 overscan |
| editor RPC normal timeout | 2 s |

任何超预算都写入 perf probe，不允许靠固定 sleep 假装通过。

### 13.2 安全和资源限制

- 文档大小、行数、ChangeSet ops、补全数量均有硬上限；
- 文本内容不得被写入日志，trace 只记录 hash、长度和 span；
- completion detail 可以包含脱敏 label，不得包含 token、密钥或任意路径；
- Flow 仍禁止代码执行、网络、URL、文件路径和动态拓扑；
- 当前不提供文件保存和外部持久化；
- 诊断消息来自稳定 code + bounded message，不能接受任意远端 HTML。

### 13.3 结构化 trace

editor command 至少经过：

```text
editor.document.change.received
editor.document.change.validated
editor.document.change.accepted | rejected
editor.document.diagnostics.published
editor.completion.requested
editor.completion.accepted | rejected
```

同一 command 使用同一 `request_id`，异步 compile 使用 `job_id`，每条包含
document/session/revision/epoch。

---

## 14. 分阶段施工顺序

### M0：审计和契约冻结

- [x] 将本文件作为施工主文档；标记 GLM 雏形边界；
- [x] 冻结当前 document position、compatibility declaration 和 capability name；
- [ ] 冻结完整 document frame、revision、size limit、error code；
- [x] 新增 code_editor parser contract tests；
- [ ] 新增完整 document protocol fixture 与 schema JSON tests。

完成标准（当前子集）：声明字段含义已固定；跨进程 document frame 仍待 M3 冻结。

### M1：editor-core 可用内核

- [x] 完成基础 TextBuffer Unicode、跨行和 position mapping；
- [x] 完成基础 undo grouping、full resync、ChangeSet；
- [ ] 提取 grammar single source；
- [x] 完成 token cache、symbols 和 replacement-range completion；
- [ ] 接入正式 selection model、剪贴板命令和 ChangeSet 上限；
- [ ] 增加 property-based/fuzz 测试和 10k 行 benchmark；
- [ ] core 不新增窗口/GPU/IO 依赖。

完成标准（当前子集）：`cargo test --quiet -p neon-editor-core` 实际 `33 passed`；完整 M1 仍需 single-source grammar、selection 和 benchmark。

### M2：Flow schema 和 parser 接入

- [x] `UiCodeEditorDeclaration` compatibility contract；
- [x] `code_editor` parser/formatter/lowering 到 `UiIrDocument.code_editors`；
- [x] source/read_only/completions 基础类型校验与 font/tab/wrap 范围校验；
- [x] `#` 注释和 source span 行为统一；
- [ ] document/completion dedicated input kinds 与完整 declaration diagnostics；
- [x] `cargo test --quiet -p neon-ui-schema`、`cargo test --quiet -p neon-ui-runtime`。

完成标准（当前子集）：`code_editor` 能进入 canonical IR，formatter 可 round-trip，错误 source 类型会被拒绝；仍是 Panel compatibility node，尚未可渲染编辑。

### M3：editor-runtime 文档服务

- [x] 新建 `crates/neon-editor-runtime`；
- [x] document open/snapshot/change/commit/close；纯内存 editor baseline；
- [x] revision conflict、epoch、idempotency、bounded op/size limits；bounded compile queue 待后续；
- [x] static completion request/response 和 stale rejection；动态 host completion 待后续；
- [ ] 发布容错 Flow diagnostics；
- [ ] 动态 completion frame 和订阅。

完成标准（当前子集）：`editor_protocol_probe` 真实 loopback JSONL 全部 `pass_result:true`，退出码 `0`；窗口展示和 IME 仍未接通。

### M4：只读 WGPU code_editor composition

- [ ] 绑定 document snapshot；
- [ ] 行号、token text runs、clip、scroll、current line；
- [ ] 诊断 underline/gutter；
- [ ] visible rows + overscan；
- [ ] PNG capture 和 renderer diagnostics。

完成标准：`wgpu-rendered`，但尚不接受键盘编辑。

### M5：本地编辑交互

- [ ] caret/selection/mouse drag；
- [ ] insert/delete/newline/indent/tab；
- [ ] undo/redo；
- [ ] clipboard；
- [ ] IME preedit/commit；
- [ ] local mirror 与 ChangeSet debounce。

完成标准：打字不等待 IPC，headless input replay 与真实窗口行为一致。

### M6：completion popup 和动态候选

- [ ] local static candidate；
- [ ] RPC completion request；
- [ ] replacement range apply；
- [ ] popup keyboard navigation；
- [ ] stale response/epoch cancellation；
- [ ] dynamic completion frame capacity/diagnostics。

完成标准：`ui.code-editor.highlight-and-complete.v1` 通过。

### M7：后续集成（不属于当前逻辑阶段）

- [ ] 左编辑器窗口 shell；
- [ ] 预览或文件集成由后续独立计划定义；
- [ ] restart/reopen recovery；
- [ ] visual capture + JSONL scenario。

完成标准：`playground.invalid-flow-keeps-last-preview.v1` 通过，达到
`composition-ready` 和 `wgpu-rendered`。

### M8：人工交互验收

- [ ] 中文 IME；
- [ ] 大文档滚动；
- [ ] 选择、复制、粘贴、撤销；
- [ ] 补全可读性和遮挡；
- [ ] 诊断定位；
- [ ] 窗口交互工作流。

完成标准：由人工确认后，才写 `interactive-accepted`。自动化 probe 不能代替这一层。

---

## 15. 文件和 crate 变更清单

### 已存在、需要继续修改

```text
Cargo.toml
Cargo.lock
crates/neon-ui-schema/src/lib.rs
crates/neon-ui-runtime/src/lib.rs
crates/neon-ui-runtime/src/nui_flow.rs
crates/neon-wgpu-runtime/src/lib.rs
crates/neon-wgpu-runtime/src/ui_renderer.rs
```

### 已有雏形

```text
crates/neon-editor-core/Cargo.toml
crates/neon-editor-core/src/buffer.rs
crates/neon-editor-core/src/completion.rs
crates/neon-editor-core/src/edits.rs
crates/neon-editor-core/src/grammar.rs
crates/neon-editor-core/src/highlight.rs
crates/neon-editor-core/src/lib.rs
crates/neon-editor-core/src/symbols.rs
```

### 计划新增

```text
crates/neon-editor-runtime/Cargo.toml
crates/neon-editor-runtime/src/lib.rs
crates/neon-editor-runtime/src/document.rs
crates/neon-editor-runtime/src/protocol.rs
crates/neon-editor-runtime/src/completion.rs
crates/neon-editor-runtime/src/bin/editor_protocol_probe.rs
crates/neon-ui-runtime/src/bin/code_editor_service_probe.rs
crates/neon-wgpu-runtime/src/bin/code_editor_window_probe.rs
crates/neon-dev/src/bin/nui_playground_probe.rs
tests/scenarios/ui-code-editor-highlight-complete.yaml
tests/scenarios/playground-live-edit-loop.yaml
tests/fixtures/editor-playground.nui
```

### 依赖纪律

- `neon-editor-core` 不依赖 Neon3 runtime crate；
- `neon-editor-runtime` 通过 `neon-protocol` / `neon-ipc` / observability；
- editor runtime 与 UI runtime 不通过 Rust crate 共享业务状态；
- WGPU 仍是唯一窗口、唯一 wgpu owner；
- 当前阶段不依赖任何项目文件服务。

---

## 16. 最终完成定义

只有同时满足以下条件，才可以说“IDE 多行 NUI Flow 编辑器完成”：

1. `code_editor` Flow 声明有稳定 schema、parser、formatter、IR 和 capability；
2. editor-core 的 buffer/highlight/completion/undo 有 headless 测试；
3. editor-runtime 能以 revision/idempotency/epoch 处理 document ChangeSet；
4. WGPU 本地打字、选区、滚动和 IME 不等待 IPC；
5. 真实 WGPU 窗口能绘制行号、token、caret、selection、diagnostics 和 popup；
6. completion candidate 带 replacement range，旧响应不会污染新文档；
7. draft/commit 只改变 editor session，不触发外部写入或 UI 预览；
8. 窗口阶段的真实输入和像素由独立 window probe 验收；
9. editor protocol probe、service probe、window probe 都输出 JSONL 且 exit code 正确；
10. 真实 capture 已读回检查，人工完成 IME 和工作流验收；
11. 计划中未实现的折叠、多光标、LSP、搜索替换没有被伪装成 V1 能力。

当前结论：逻辑阶段已达到 **M1 子集 + M2 declaration contract 子集 + M3 editor service 子集**；
尚未进入 WGPU 专用绘制和带窗口交互验收。当前明确不启动 UI 预览、不接文件保存、不设计或依赖 projectd。
