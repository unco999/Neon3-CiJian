---
title: Neon3 UI Input Impact Graph and Incremental Update Architecture
status: design
version: 1
owners:
  - neon-ui-runtime
  - neon-wgpu-runtime
---

# Neon3 UI Input Impact Graph and Incremental Update Architecture

## 0. 结论

Neon3 UI 必须在编译/初始化阶段确定一个不可变的 **Input Impact Graph**。
运行时不再根据完整 UI 树猜测哪些组件受到影响，而是使用已经序列化的稳定语义依赖：

```text
input key
  -> binding / branch / template / derived dependency
  -> semantic node key
  -> bound property
  -> invalidation domains
  -> retained CPU record / renderer-local GPU range
```

目标不是“每次只返回一个小 JSON”，而是保证从 input 或 interaction 开始，整个更新链路都能
证明受影响范围：

```text
one input change
  -> one impact set
  -> only impacted node state changes
  -> only required layout subtrees recalculate
  -> only required instance ranges upload
  -> only required render passes redraw
```

GPU instance index、buffer offset、hit ID 仍然是 `neon-wgpu-runtime` 的私有数据，不能进入
Flow、JSON IR、跨进程协议或 AI inspection API。公开层只使用稳定 semantic node key；WGPU
在本地把 node key 解析为当前 plan 的 index/range。

## 1. 当前真实行为

### 1.1 Input 绑定

NUI Flow 中的这些写法会生成受控 input 绑定：

```text
button publish checked $can_publish
input terrain-name value $terrain_name
slider amount numeric $amount
combo mode state $mode
panel inspector visible $inspector_visible
```

编译后已有：

```rust
UiBinding {
    binding_id,
    input_key,
    node_key,
    property,
    expected_kind,
}
```

`UiDependencyIndex.input_to_bindings` 也已经存在。

### 1.2 控件交互不是直接写 input

当前控制流是：

```text
WGPU pointer/hit test
  -> local pressed/hover/focus/value preview
  -> semantic event / UiHostInbound
  -> ui-runtime validates declaration and revision
  -> domain owner processes intent
  -> domain returns UiHostPublication.scalar_frame
  -> UiInputStore applies UiInputFrame
  -> new authoritative input snapshot
```

因此：

- `checked $feature_enabled` 是受控显示绑定；
- checkbox 点击不会直接把 `feature_enabled` 写成新值；
- domain 返回新的 `UiInputFrame` 后，`feature_enabled` 才成为权威新值；
- WGPU 可以在等待响应时显示本地预测，但预测必须可回滚；
- `UiControlPresentation` 是 renderer/domain 提供的 presentation 数据，不等同于
  `UiInputStore` 的权威输入写入。

### 1.3 已有交互分类

| 交互 | 即时 owner | 是否直接修改 authoritative input | 最终确认路径 |
| --- | --- | --- | --- |
| hover | WGPU | 否 | 无需 domain |
| pressed | WGPU | 否 | 语义事件或取消 |
| focus | WGPU | 否 | 通常无 domain 写入 |
| button click | WGPU -> UI runtime/domain | 否 | semantic event -> input publication |
| checkbox/radio/selectable | WGPU local preview | 否 | semantic event -> bool input |
| slider/drag value | WGPU local value preview | 否 | preview/commit event -> numeric input |
| combo/dropdown/tabs/list | WGPU 计算选择项 | 否 | selection event -> enum input |
| text input | WGPU 本地编辑缓冲 | 否 | text commit -> text handle/input publication |
| scroll | WGPU local scroll | 通常否 | 可选 window request 或 presentation update |
| drag | WGPU local offset | 否 | drop semantic event -> accepted revision/input |
| data grid cell | WGPU cell hit/typed payload | 否 | DataGridCell event -> domain publication |

## 2. 设计原则

### 2.1 初始化确定依赖，运行时只传播变化

以下关系必须在 `compile_ui_program` 阶段建立并序列化到 `UiProgram`：

- 直接 binding；
- branch predicate；
- bounded template/repeat source；
- 允许的 derived input dependency；
- node/property 对 layout、text、resource、color、depth、hit 的影响分类；
- interactive node 的本地 presentation 影响分类；
- semantic event 对应的受控 input keys（仅用于诊断和回流关联，不让 UI runtime 直接改 domain）。

运行时禁止每帧扫描全量 `binding_records` 来重新发现依赖。

### 2.2 值和影响分离

同一个 input 无论值是 `false`、`true`、`0`、`100` 或某个 enum variant，影响图都不变。
值只决定新的 resolved state；影响图决定哪些记录需要被重新计算。

### 2.3 预测和权威分离

interaction preview 可以改变 renderer-local presentation state，但不能伪装成
`UiInputFrame`。权威输入只能来自通过 revision/idempotency 校验的 publication。

### 2.4 公开语义 key，私有物理 offset

跨进程和 debug 输出：

```text
node_key = "surface/root/amount-slider"
property = "numeric_value"
```

WGPU 内部：

```text
node_key -> plan_index -> instance_range -> byte_offset
```

后者不能序列化到公开协议，因为 plan index 和 buffer offset 会因 fragment、viewport、
data-grid window 或 renderer epoch 改变。

## 3. 编译期元模型

### 3.1 Invalidation domain

在 `neon-ui-schema` 中增加稳定枚举：

```rust
enum UiInvalidationDomain {
    NodeState,
    Layout,
    TextLayout,
    Resource,
    ColorInstances,
    DepthInstances,
    HitTarget,
    SemanticBindings,
    InteractionPresentation,
}
```

它描述“需要重新生成什么”，不描述 GPU 实现细节。

### 3.2 Binding impact

```rust
struct UiBindingImpact {
    binding_id: u32,
    node_key: String,
    property: UiBoundProperty,
    domains: Vec<UiInvalidationDomain>,
}
```

属性到 domain 的初始固定映射：

| 属性 | 必需 domain |
| --- | --- |
| `Visible` | NodeState, Layout, ColorInstances, DepthInstances, HitTarget |
| `Enabled` | NodeState, ColorInstances, HitTarget |
| `TextValue` | NodeState, TextLayout, Layout, ColorInstances, DepthInstances |
| `Selected` | NodeState, ColorInstances |
| `Active` | NodeState, ColorInstances |
| `NumericValue` | NodeState, ColorInstances |
| `ImageAsset` | NodeState, Resource, ColorInstances, DepthInstances |
| `Opacity` | NodeState, ColorInstances, DepthInstances |
| `StateToken` | NodeState, ColorInstances |
| `ScrollOffset` | NodeState, Layout, ColorInstances, DepthInstances, HitTarget |
| `CanvasData` | NodeState, Resource, ColorInstances, HitTarget |

后续可以按组件能力收窄，但第一版宁可多标记 domain，不允许漏更新。

### 3.3 Input impact

```rust
struct UiInputImpact {
    input_key: String,
    binding_impacts: Vec<UiBindingImpact>,
    binding_ids: Vec<u32>,
    branch_keys: Vec<String>,
    affected_node_keys: Vec<String>,
    domains: Vec<UiInvalidationDomain>,
}
```

`UiDependencyIndex` 增加：

```rust
input_impacts: BTreeMap<String, UiInputImpact>
```

所有数组必须按稳定编译顺序排序并去重。序列化 round-trip 后必须保持完全相同。

### 3.4 Branch impact

对：

```text
branch ready when $state=ready
```

编译期记录：

```text
state -> branch.ready -> branch.node_range + layout ancestors
```

branch 的 input 变化至少影响：

```text
NodeState + Layout + ColorInstances + DepthInstances + HitTarget
```

第一版不要根据 predicate 的新旧结果过早优化；只要 predicate input 发生变化，就产生完整
branch impact。确认正确性后再增加“predicate 结果没有改变则跳过 layout”的优化。

### 3.5 Interaction impact

交互不绑定到 domain input 的直接写入，而是绑定到 renderer-local presentation。编译期为
每个可交互 node 生成：

```rust
struct UiInteractionImpact {
    node_key: String,
    interaction_kinds: Vec<UiInteractionKind>,
    preview_domains: Vec<UiInvalidationDomain>,
    semantic_intents: Vec<String>,
    controlled_input_keys: Vec<String>,
}
```

`controlled_input_keys` 来自该 node 的 direct bindings 和 event declarations，用于：

- 诊断“点击哪个控件最终影响哪个 input”；
- 将 semantic event、publication、下一次 render delta 串成同一个 trace；
- 不允许 renderer 直接修改这些 input。

建议 interaction kinds：

```text
Hover
Pressed
Focus
TogglePreview
NumericPreview
ChoicePreview
TextEditPreview
ScrollPreview
DragPreview
DropResolution
```

## 4. 运行期增量模型

### 4.1 ImpactSet

每次变化先统一转换为一个内部 `UiImpactSet`：

```rust
struct UiImpactSet {
    cause: UiChangeCause,
    input_keys: Vec<String>,
    interaction_nodes: Vec<String>,
    binding_ids: Vec<u32>,
    node_keys: Vec<String>,
    domains: BTreeSet<UiInvalidationDomain>,
    branch_keys: Vec<String>,
    semantic_sequence: Option<u64>,
    input_revision: Revision,
    fragment_revision: Revision,
}

enum UiChangeCause {
    InputPublication,
    LocalInteractionPreview,
    LocalInteractionCommit,
    ProgramActivation,
}
```

来源不同，但进入 evaluator/renderer 前必须统一去重和排序。

### 4.2 Input publication

```text
UiInputStore.apply(frame)
  -> compare canonical old/new values
  -> changed_slots
  -> lookup input_impacts[key]
  -> UiImpactSet
  -> retained CPU state delta
  -> renderer delta / WGPU upload
```

相同值写入不得产生 UI dirty slot。空变化和 canonicalized 后未变化的变化不得推进 input
revision，除非协议明确要求保留一个外部 command receipt；receipt revision 与 UI state
revision 必须分开。

### 4.3 Local interaction preview

```text
pointer / keyboard
  -> hit binding resolved locally
  -> UiInteractionImpact(node_key, kind)
  -> local presentation delta
  -> only affected node/group instance upload
```

preview 不进入 `UiInputStore`，不产生 domain revision，也不能被 domain 当成权威事实。

### 4.4 Semantic event and authoritative response

```text
local preview
  -> semantic event {interaction_id, sequence, node_key, intent, input_revision}
  -> domain/UI validation
  -> accepted/rejected
  -> UiHostPublication.scalar_frame
  -> InputPublication ImpactSet
  -> confirm or rollback local preview
```

同一个 interaction 必须通过 `interaction_id`、semantic sequence、request ID、input revision
串起来。拒绝、超时、epoch 变化和 focus loss 都必须清理 preview。

## 5. CPU evaluator 改造

### 5.1 保留 full evaluator

`evaluate_ui_program()` 保留为：

- program activation；
- fragment/program revision 替换；
- debug baseline；
- replay baseline；
- 增量结果校验的黄金参考。

### 5.2 新增 retained evaluator

增加：

```rust
evaluate_ui_program_initial(...) -> UiRetainedFrame
apply_ui_impact_set(
    program: &UiProgram,
    retained: &mut UiRetainedFrame,
    inputs: &UiResolvedInputs,
    local: &UiLocalPresentationState,
    impact: &UiImpactSet,
) -> UiFrameDelta
```

`UiRetainedFrame` 按 stable node key 保存：

```text
node state
logical layout
clip
render primitive
semantic target
```

增量 evaluator 只执行 `binding_ids` 和 `branch_keys` 指向的工作。

### 5.3 Layout 传播规则

| 变化 | 处理 |
| --- | --- |
| opacity/selected/active/numeric/state | 只更新 node state 和 visual primitive |
| enabled | 更新 node state、visual、hit binding |
| text | 重新测量该 text node；若尺寸改变，向父 layout ancestors 传播 |
| visible/branch | 重新计算 branch subtree 和受影响父 layout tracks |
| scroll | 只更新 scroll subtree 的 presentation/layout 与 hit |
| image/resource | 更新 resource residency/visual，不重新编译 program |
| camera/world transform | 只更新 final visual transform，不重新测量 logical layout |

布局算法必须返回 `changed_layout_nodes`，不能只返回一个 `layout_dirty: bool`。

## 6. WGPU renderer 改造

### 6.1 本地索引

每次 plan reconcile 后建立 renderer-local：

```rust
HashMap<SemanticNodeKey, RendererNodeRecord>
```

其中 `RendererNodeRecord` 可以包含：

```text
plan index
paint group
color instance range
depth instance range
hit instance range
text range
```

这些字段只能存在 WGPU 进程。

### 6.2 Delta 消费

`UiFrameDelta` 只携带稳定 node key 和逻辑属性变化。WGPU 解析本地索引后：

```text
node key
  -> renderer node record
  -> update in-memory instance
  -> queue.write_buffer(exact range)
```

必须禁止因为一个 opacity/numeric/selected 变化而：

- 重新 flatten 全树；
- 重新测量无关文本；
- 重建 pipeline/bind group；
- 重建无关 hit map；
- 上传整个 instance buffer。

### 6.3 兼容旧 fragment path

旧的 `UiFragment` 全量提交路径保留。当没有 `UiProgramDelta` capability 时继续走旧路径。
新增增量路径必须显式 capability/version，不得改变旧客户端含义。

## 7. Interaction 与 Input 的最终关系

必须明确区分以下两种状态：

```text
RendererLocalPresentation
  hover / pressed / focus / drag offset / numeric preview / text edit buffer

AuthoritativeUiInput
  checked / numeric / enum / text handle / visible / enabled / branch predicate
```

典型 checkbox：

```text
1. pointer down: WGPU 只设置 pressed preview
2. pointer up: WGPU 发送 declared semantic intent
3. domain/UI authority 返回 feature_enabled=true 的 input frame
4. UiInputStore 产生 input impact
5. checkbox 与依赖它的 status text 产生 delta
6. WGPU 确认 preview 或回滚 preview
```

不能在第 1 或第 2 步直接修改 authoritative input。

## 8. 诊断和 AI 查询

每个增量更新必须输出结构化记录：

```json
{
  "event": "ui.incremental_update.applied",
  "cause": "input_publication",
  "input_revision": 12,
  "fragment_revision": 8,
  "input_keys": ["feature_enabled"],
  "binding_ids": [4, 7],
  "node_keys": ["feature-toggle", "feature-status"],
  "domains": ["node_state", "color_instances", "hit_target"],
  "layout_rebuilt": false,
  "text_remeasured": false,
  "gpu_ranges_written": 2,
  "status": "applied"
}
```

必须支持查询：

```text
ui.debug.input-impact <program> <input-key>
ui.debug.node-impact <program> <node-key>
ui.debug.interaction-impact <program> <node-key>
ui.debug.delta.get <request-id>
ui.debug.replay <program> <input-timeline>
```

AI 不得从最终截图或普通日志猜测影响范围。

## 9. 必须通过的测试

### Contract

1. `UiInputImpact` JSON round-trip 保持稳定。
2. 每个 input 都有 impact record，即使它没有 binding，影响为空且可诊断。
3. 每个 binding 都出现在对应 input impact 中。
4. 每个 branch predicate input 都包含完整 branch node range。
5. 不允许出现未知 node/property/binding ID。

### CPU incremental

1. 改变一个 input 只执行对应 binding IDs。
2. 无关 node state、layout、primitive 完全 bitwise/equality 不变。
3. text 改变只重测目标文本和必要父 ancestors。
4. visible/branch 改变只更新 branch subtree 与必要 layout ancestors。
5. 相同 canonical value 不产生 dirty 或 revision advance。
6. full evaluator 与 incremental evaluator 的最终 frame 完全一致。

### Interaction

1. hover/pressed/focus 不修改 authoritative input。
2. checkbox preview 可确认、拒绝、取消和 epoch reset。
3. slider preview 不发送每个 pointer move 的可靠 RPC；commit 只发送一次。
4. button semantic event 返回的 input frame 能产生正确 impact。
5. stale interaction、stale fragment、focus loss 都清理本地 preview。

### WGPU

1. 一个 numeric/opacity/selected 变化只写目标 instance range。
2. 无 layout domain 时不重新 layout。
3. 无 hit domain 时不重建 hit target。
4. 静态 node/binding/branch buffer 不重复上传。
5. GPU consumer 使用 input revision/frame sequence 正确配对。

### Executable probe

新增 `src/bin/ui_input_impact_probe.rs`，输出 JSONL，至少包含：

- request/sequence；
- input key/value hash；
- input revision；
- binding IDs；
- semantic node keys；
- invalidation domains；
- CPU changed node count；
- layout/text/hit flags；
- GPU written ranges/count；
- final pass/fail。

Probe 必须固定输入、固定 timeout、无固定 sleep 猜测完成，并以失败退出码结束。

## 10. 施工顺序

### Phase A: metadata

1. 在 `neon-ui-schema` 增加 impact domain、binding impact、input impact、interaction impact。
2. 在 `compile_ui_program` 生成并排序完整 dependency metadata。
3. 更新 `UiDependencyIndex` JSON schema 与手工 fixture。
4. 添加 metadata contract tests。

### Phase B: CPU retained delta

1. 建立 initial retained frame。
2. 建立 `UiImpactSet` builder。
3. 实现 binding-only node state delta。
4. 实现 branch subtree delta。
5. 实现 layout ancestor propagation。
6. 用 full evaluator 做 equality oracle。

### Phase C: interaction convergence

1. 为 renderer-local presentation 生成 interaction impact。
2. 将 hover/pressed/focus/value preview 接入 local delta。
3. 将 semantic event -> publication -> input impact 接入 trace。
4. 明确 confirm/rollback/epoch-reset 生命周期。

### Phase D: WGPU ranges

1. 建立 node key -> renderer-local range index。
2. 让 delta 更新 color/depth/hit/text 对应 range。
3. 删除无必要的 full plan refresh 和 full instance upload。
4. 运行 GPU probe，核对 producer/consumer frame pairing。

## 11. 禁止事项

- 不允许把 renderer-local instance index、buffer offset、hit ID 写入 UI IR。
- 不允许按钮点击直接修改 `UiInputStore` 绕过 semantic/domain 边界。
- 不允许把 local preview 当成 authoritative input。
- 不允许只添加 `dirty: bool` 而不记录具体 node/property/domain。
- 不允许保留 impact metadata 却继续每帧全量扫描并宣称增量完成。
- 不允许以最终像素正确作为增量范围证明。
- 不允许通过增加 timeout 或固定 sleep 掩盖 revision/sequence 配对错误。

## 12. 完成定义

只有同时满足以下条件，才能称为“精准增量 UI 更新”：

```text
编译期完整序列化 input/interaction impact graph
CPU evaluator 使用 impact set，不扫描无关 binding
layout/text/hit/render domain 有明确传播范围
WGPU 使用 stable node key 映射本地 range 并局部写入
interaction preview 与 authoritative input 分离且可确认/回滚
full evaluator 与 incremental evaluator 有 equality oracle
JSONL probe 能输出 producer/consumer 范围和最终结果
```

只有 input buffer 局部写入、retained plan 复用或 patch 操作数量减少，不能单独宣称完成。

## 13. 实施状态与证据

### Phase A: metadata — 已完成（commit `59b2ad3`）

- `neon-ui-schema`：`UiInvalidationDomain`、`UiBoundProperty::invalidation_domains()`、
  `UiBindingImpact`、`UiInputImpact`、`UiInteractionKind`、`UiInteractionImpact`。
  `UiDependencyIndex` 增加 `#[serde(default)] input_impacts` / `interaction_impacts`，
  domain 列表按声明序排序去重，round-trip 字节稳定。
- `neon-ui-runtime/src/ui_input_impact.rs`：编译期构建 input/interaction impact，含
  branch predicate input key 归因、layout ancestor 闭包、derived slot（`input x bool =
  $hp < 0.3`）到源 slot 的不动点继承、drag/drop 交互合并。
- 证据：`cargo test -q -p neon-ui-runtime --lib ui_input_impact` →
  `test result: ok. 7 passed; 0 failed`。

### Phase B: CPU retained delta — 已完成

- `crates/neon-ui-runtime/src/ui_retained_evaluator.rs`：
  `UiImpactSet::from_input_publication`、`evaluate_ui_program_initial`、
  `apply_ui_impact_set`、`UiFrameDelta`。
- 回放集合 = impacted binding 所属 node ∪ 受影响 branch 的 `node_range`。刻意不含
  layout ancestor：ancestor 自身没有 binding，纳入就会执行其兄弟 binding，违反
  §9 CPU 测试 1。ancestor 的可观察影响只有可见性链，由 primitive assembly 重连体现。
- 每个回放节点重放它自己的全部 binding（binding_id 升序），使结果与 full evaluator 的
  “按 binding 顺序写入”完全一致；覆盖该节点的 branch 用预索引 `branches_covering_node`
  重新 gate，predicate 不满足时强制 `visible = false`。
- `layout_unchanged` 语义：`logical_layout` 记录只来自编译记录加 local presentation
  拖拽偏移，`InputPublication` 类 delta 不会改写它；可见性翻转只触发
  `rebuild_primitive_assembly`（含 root clamp），由 `render_primitives_rebuilt` 单独报告。
- 任何 diagnostic 逃逸或 revision/presentation/cause 守卫不匹配都返回稳定错误码
  （`ui_incremental_stale_frame` / `ui_incremental_unsupported_cause` /
  `ui_incremental_unknown_binding` / `ui_incremental_unknown_branch` /
  `ui_incremental_diagnostic_escape`）并把 frame 标记为 `degraded`，调用方回退 golden
  full evaluator；不做静默猜测。
- 输入存储契约（§4.2）：全等值 publication 不再 bump revision、不产生 dirty slot，
  保留旧 snapshot；idempotent receipt 仍被记录，command receipt revision 与 UI state
  revision 保持分离。
- 范围决定：DataGrid/template 行数据不进入 `input_impacts`，因为行数据通过 runtime
  publication 而非具名 input slot 到达；其增量由 Phase D 的 renderer range 写入覆盖。
- 生产路径现状：`refresh_fragment_from_program` 仍调用 full evaluator，retained 路径
  目前是并行的、被 oracle 证明的实现；切换到默认路径在 Phase C/D 完成后进行。
- 证据（实际命令输出）：
  - `cargo test -q -p neon-ui-runtime --lib ui_retained_evaluator` →
    `test result: ok. 7 passed; 0 failed`（含 §9 测试 1–5 与多 slot union、stale 拒绝、
    跨 7 步变更序列的 full/incremental 逐字段相等 oracle）。
  - `cargo test -q -p neon-ui-runtime --lib` → `233 passed; 0 failed`。
  - `cargo clippy -q -p neon-ui-runtime --lib --all-targets` →
    `ui_retained_evaluator.rs` 0 warning。
  - `cargo check -q --workspace --all-targets` → exit code `0`。
  - `rustfmt --edition 2024 --check` 对新增/改动文件 → 无 diff。
- 因存储契约变更而调整的既有测试：`demo_domain` 组件画廊 headless scenario 原先断言
  “每个语义事件必定 `Revision(index + 1)`”。第 12 个事件（`gallery-text` 的
  TextEditCommit 回写同一文本）在新契约下是真正的 no-op，因此该断言改为按 publication
  是否真的改变 slot 值来推导期望 revision（改变了 +1，未改变保持不变），其余断言不变。

### Phase C: interaction convergence — CPU 侧已完成

- `UiImpactSet::from_local_interaction(program, kind, node_key, renderer_epoch,
  semantic_sequence, input_revision, fragment_revision)`：只从编译期
  `interaction_impacts` 取范围；不携带 binding id、branch key 或 input key，因此
  preview 在类型层面就不可能触达权威求值。未声明的 kind 返回
  `ui_incremental_undeclared_interaction_kind`，非交互节点返回
  `ui_incremental_unknown_interaction_node`。
- preview 以 kind 为键保存在 retained frame 的 overlay 里（一个指针/焦点同一 kind
  只能预测一个节点），并记录它预测时的 `input_revision` 与 `semantic_sequence`。
  `UiRetainedFrame::frame()` 不含 overlay，所以权威 frame 与 golden evaluator 恒等。
- 生命周期：
  - `apply_ui_impact_set` 的 `LocalInteractionPreview` 分支只返回
    `preview_kind/preview_node_key/displaced_preview_node_key/preview_revision`，
    `input_revision`、`changed_states`、primitive assembly 全部不动；
  - `resolve_ui_interaction_preview(Confirmed)` 要求权威 revision 已经前进，否则
    `ui_incremental_confirm_without_authority`，即“预测不能自我确认”；
    `RolledBack`/`Cancelled` 总是允许并返回需要恢复的 domains；
  - 权威 publication 命中被预测节点时，delta 报 `superseded_preview_kinds` 并丢弃该
    overlay；
  - `impact.input_revision` 落后于当前显示帧 → `ui_incremental_stale_preview`，同时在
    拒绝时删除该 overlay（它永远不可能被确认）；
  - epoch 不一致 → `ui_incremental_preview_epoch_mismatch` 且不改状态，调用方必须显式
    `reset_ui_interaction_previews(new_epoch)`，该函数返回每个被清除 preview 的恢复记录。
  - `LocalInteractionCommit`/`ProgramActivation` 作为 cause 进入 CPU delta 路径会被拒绝：
    commit 的权威路径是 semantic event → domain → InputPublication。
- 诊断（§8）：`UiIncrementalUpdateRecord::applied/rejected` 产出
  `ui.incremental_update.applied|rejected` 结构化记录，含 cause、input revision、
  input keys、binding ids、changed node keys、domains、layout/text/primitives 标记、
  preview 信息与 status。`gpu_ranges_written` 在 CPU 侧固定为 `null`，只有 renderer
  报告后才有值，避免任一层替另一层宣称完成。
- AI 查询（§8）：`UiDebugSession::input_impact` / `interaction_impact` /
  `node_impact`（反向：哪些 input、binding、branch、domain 能打到一个节点），返回
  只含稳定 node key / binding id / domain 的 `UiNodeImpactSummary`。
- §9 Interaction 覆盖：1 preview 不改权威（`interaction_preview_leaves_the_authoritative_frame_untouched`）；
  2 confirm/rollback/cancel/epoch reset（`confirm_requires_authority_...`、
  `authoritative_publication_supersedes_...`、`epoch_reset_clears_every_preview_...`）；
  3 slider 连续 preview 不产生 revision、commit 只发布一次
  （`repeated_slider_preview_costs_no_revision_and_one_commit_publishes_once`，跨进程 RPC
  批量的另一半在 Phase D 的 renderer 侧）；4 事件返回的 input frame 产生正确 impact
  （`interaction_record_links_the_node_to_its_authoritative_input`）；
  5 stale interaction 清理 preview（`stale_preview_is_rejected_and_its_overlay_is_dropped`）。
- 证据：
  - `cargo test -q -p neon-ui-runtime --lib ui_retained_evaluator` →
    `test result: ok. 18 passed; 0 failed`（Phase B 7 + Phase C 11）。
  - `cargo test -q -p neon-ui-runtime --lib debug::tests` → `1 passed; 0 failed`。
  - `cargo test -q -p neon-ui-runtime --lib` → `245 passed; 0 failed`。
  - `cargo clippy -q -p neon-ui-runtime --lib --all-targets` → 新增/改动文件 0 warning。
  - `cargo check -q --workspace --all-targets` → exit code `0`。

### Phase D: WGPU ranges — 未开始（GPU 上传统计与 headless probe 已先行落地，
commit `b833761`）
