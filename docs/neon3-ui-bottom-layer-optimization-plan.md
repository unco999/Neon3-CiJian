# Neon3 UI 底层增量更新优化推进表

目标：优化 `D:\NEON3` 的 UI 更新底层，让 persistent UI shell 在大量按钮、事务、Agent、Plan task 状态变化时保持低延迟、稳定动画和可预测的增量更新。

适用项目：

- 底层：`D:\NEON3\crates\neon-ui-schema`
- 底层：`D:\NEON3\crates\neon-ui-runtime`
- 渲染：`D:\NEON3\crates\neon-wgpu-runtime`
- 验证：`neon-ui-runtime` / `neon-wgpu-runtime` probes
- 使用方：`D:\neon-ide`

## 0. 总原则

```text
业务状态变化
  -> UI projection
    -> keyed tree diff
      -> coalesced UiPatch
        -> revision check
          -> IR patch
            -> compile affected program/fragment
              -> retained renderer reconcile
                -> frame/render ack
```

必须遵守：

| 状况 | 正确操作 |
|---|---|
| 文本、颜色、状态、visible、enabled 改变 | `SetProperty` / `SetInput` |
| 新增列表项 | `InsertNode` |
| 删除列表项 | `RemoveNode` |
| 列表项排序变化 | `MoveNode` |
| 父节点 children 整体变化 | `ReplaceChildren` |
| 高频状态连续变化 | reducer 后合并，单批 patch 提交 |
| 业务对象身份 | 稳定 semantic key |
| renderer 对象 | 由底层 retained reconcile 管理 |

禁止把所有状态变化实现成 `RemoveNode + InsertNode`。

---

## 1. 当前问题基线

### P0 问题

| 编号 | 问题 | 影响 |
|---|---|---|
| P0-1 | `ui.flow.patch` 与完整 Flow submit 的边界没有统一性能指标 | 无法知道延迟来自 parse、compile、fragment 还是 renderer |
| P0-2 | 高频业务状态由调用方直接触发 full remount | 点击和流式状态响应慢 |
| P0-3 | UI patch 缺少统一 keyed tree diff 层 | 文件树、任务树、事务树容易重复实现和身份错位 |
| P0-4 | patch 没有统一 coalescing/batching | 多个事务变化产生大量小 patch |
| P0-5 | stable key、revision、ack 的失败路径缺少端到端 probe | 可能出现 stale patch、重复 patch、状态回滚错误 |

### 当前 Neon3 已有能力

底层已有结构化 patch：

- `SetProperty`
- `InsertNode`
- `RemoveNode`
- `ReplaceChildren`
- `MoveNode`
- `StartTransition`
- `SetInput`

已有核心路径：

```text
ui.flow.patch
  -> apply_nui_ir_patch
  -> compile_nui_flow_program
  -> UiFragment
  -> ui.fragment.submit
  -> WGPU retained renderer
```

本计划主要优化协议使用、diff、合并和观测，不重新发明 renderer。

---

## 2. Phase 0：建立性能基线

状态：`DONE（2026-09-19）`。测量结果、JSONL 机器比较格式与性能预算见
[`ui-phase0-performance-baseline.md`](./ui-phase0-performance-baseline.md)。
生产者 probe：`cargo run -p neon-ui-runtime --bin ui_patch_baseline_probe`；
消费者 probe：`cargo run -p neon-wgpu-runtime --bin ui_reconcile_baseline_probe`。

### 任务

| ID | 工作 | 文件范围 | 验收 |
|---|---|---|---|
| B0-1 | 给 `ui.flow.submit` 增加分阶段耗时字段 | `neon-ui-runtime` | 返回 parse/compile/fragment/forward 总耗时 |
| B0-2 | 给 `ui.flow.patch` 增加分阶段耗时字段 | `neon-ui-runtime` | 返回 patch_apply/compile/fragment/forward 总耗时 |
| B0-3 | 给 renderer 增加 reconcile 统计 | `neon-wgpu-runtime` | 输出 retained/created/removed/moved/updated 节点数 |
| B0-4 | 增加固定场景 probe | `src/bin` 或 runtime probe | 100/500/1000 节点结果可重复 |
| B0-5 | 定义性能预算 | docs + probe | 形成机器可比较的 JSONL 输出 |

### 固定测试场景

```text
case A: 1 个文本 SetProperty
case B: 50 个事务同时改状态
case C: 100 个 task 列表新增/更新/删除
case D: 500 个文件节点只更新选中状态
case E: 100 个节点中插入 1 个节点
case F: 100 个节点中删除 1 个节点
case G: 100 个节点批量重排
```

### 输出格式

```json
{
  "case": "transaction_state_batch_50",
  "input": {"node_count": 100, "operation_count": 50},
  "producer": {"patch_sequence": 12, "base_revision": 41},
  "consumer": {"accepted_revision": 42, "frame_sequence": 918},
  "timing_ms": {"patch_apply": 1.2, "compile": 2.4, "fragment": 0.8, "reconcile": 1.1, "total": 6.1},
  "retained": {"created": 0, "removed": 0, "updated": 50, "moved": 0},
  "pass": true
}
```

禁止在没有基线的情况下声称“更快”。

---

## 3. Phase 1：统一 stable key 和 semantic path

状态：`DONE（2026-09-19）`。K1-K5 验收由以下测试固化：
`nui_flow::tests::duplicate_node_keys_are_rejected_with_a_structured_diagnostic`、
`nui_flow::tests::patch_insert_rejects_a_key_that_already_exists_anywhere_in_the_tree`（K1）；
`nui_flow::tests::patch_accepts_semantic_paths_but_rejects_indexes`、
`nui_flow::tests::patch_topology_follows_stable_keys_after_reordering`（K2）；
`neon-wgpu-runtime` 的
`tests::reconcile_stats_track_keyed_node_lifecycle_across_fragment_revisions`（K3/K4/K5，
配合 `ui_reconcile_baseline_probe` 的 G 场景 created==0）。

### 目标

确保同一个业务对象在多次 Flow/patch 更新中拥有相同 key。

### Key 规则

```text
file:<workspace-relative-path>
task:<plan-id>/<task-id>
transaction:<transaction-id>
agent:<agent-id>
change:<change-id>
```

禁止：

- 数组 index 作为身份。
- renderer hit id 作为身份。
- 随机 UUID 作为同一对象的每次新 key。
- 使用未经 safe/规范化的路径直接生成 Flow token。

### 任务

| ID | 工作 | 验收 |
|---|---|---|
| K1 | 检查 IR node key 的唯一性校验 | 重复 key 被拒绝并返回结构化诊断 |
| K2 | 为 Insert/Remove/Move 建立 semantic path 测试 | patch 不依赖数组 index |
| K3 | 验证 key 保持时 renderer 对象复用 | reconcile created=0 |
| K4 | 验证 key 改变时明确 remove/create | 不能错误复用旧节点 |
| K5 | 增加 key 重排 probe | reorder 只产生 move，不产生全量 create |

---

## 4. Phase 2：实现通用 Keyed UI Diff

状态：`DONE（2026-09-19）`，实现位于 `crates/neon-ui-runtime/src/ui_keyed_diff.rs`
（`diff_projection_trees` / `build_ui_patch` / `summarize_operations`）。

验收证据：

- D1：`identical_trees_produce_no_operations`、`single_property_change_emits_one_set`。
- D2：`insert_remove_reorder_and_reparent_follow_keys`、`key_change_is_remove_plus_insert_never_reuse`、
  `removing_a_sibling_does_not_move_survivors`（删除/插入兄弟不产生 survivor move）。
- D3：结构操作按新树 pre-order 时间线发射；每个 parent 的 write-head 队列决定 stayer 是否需要
  显式 `MoveNode`，逃逸节点在其 parent block 之后离开时视为 junk、之前离开时自动缺席。
  `random_churn_replays_through_ops_to_a_fixed_point`（200 轮随机增删改移重排，回放后二次 diff 为空）。
- D4：`merge_keeps_last_value_per_node_and_property`。
- D5：`wholesale_child_list_swap_collapses_to_replace_children`、
  `replace_children_is_blocked_when_a_key_escapes_the_subtree`（live 逃逸 key 强制显式操作）。
- D6：`tests/fixtures/ui_keyed_diff_{old,new,expected_ops}.json` 稳定回放；
  `diff_ops_apply_through_the_public_patch_contract` 证明经公开 `apply_ui_patch` 收敛到不动点。
- Probe：`ui_patch_keyed_diff_probe`（7 case：no_op/set/insert/remove/reorder/key_change/replace，
  全部 `pass: true`；reorder 3 节点轮换只发 1 个 move，remove 只发 1 个 remove）。

### 推荐抽象

在 `neon-ui-runtime` 或共享 schema 层新增：

```rust
pub struct UiProjectionTree {
    pub key: String,
    pub kind: String,
    pub properties: BTreeMap<String, Value>,
    pub children: Vec<UiProjectionTree>,
}

pub struct UiTreeDiff {
    pub operations: Vec<UiPatchOp>,
}
```

### Diff 规则

```text
same key + same parent
  -> compare properties

new key
  -> InsertNode

missing old key
  -> RemoveNode

same key + different parent/index
  -> MoveNode

same parent child list changed significantly
  -> ReplaceChildren only when cheaper and semantically safe
```

### 任务

| ID | 工作 | 验收 |
|---|---|---|
| D1 | 实现 property diff | 相同值不生成操作 |
| D2 | 实现 child keyed diff | 新增/删除/移动正确 |
| D3 | 实现操作排序 | remove、move、insert 顺序不会破坏路径 |
| D4 | 实现操作合并 | 同一节点同一属性只保留最后值 |
| D5 | 实现 ReplaceChildren 阈值 | 大规模变化避免生成数千个小操作 |
| D6 | 增加旧树/新树 JSON fixture | 结果稳定、可回放 |

### 操作排序建议

```text
1. RemoveNode，从深到浅
2. MoveNode
3. InsertNode，从浅到深
4. SetProperty
5. SetInput
6. StartTransition
```

---

## 5. Phase 3：Patch coalescing 和 frame batching

状态：`DONE（2026-09-19）`

### 目标

把短时间内的多条业务事件合并成一批 UI patch。

### 推荐机制

```rust
pub struct UiPatchBatcher {
    pending: BTreeMap<NodeKey, BTreeMap<PropertyKey, UiPatchOp>>,
    structural: Vec<UiPatchOp>,
    next_flush: Instant,
}
```

### 合并规则

```text
task-1 status RUNNING -> DONE
  => 只发 DONE

transaction-1 fill active -> warning -> success
  => 只发 success，除非中间状态需要动画事件

多个节点变化
  => 一个 UiPatch，单 revision

结构操作和属性操作同一 frame
  => 一个有序 operations array
```

### flush 策略

| 场景 | flush |
|---|---|
| 点击按钮 | 下一次 event loop tick，目标 < 16ms |
| Agent token 流 | 30-60ms coalesce |
| 事务状态变化 | 下一 frame |
| 关键审批状态 | immediate flush |
| renderer ack 未返回 | 禁止发送下一批冲突 revision |

### 验收

- 50 个同时事务变化最多产生 1 个 patch。
- 高频 token 不触发 full Flow compile。
- patch revision 严格递增。
- 重复状态不会产生 patch。

### 完成证据（2026-09-19）

- `crates/neon-ui-runtime/src/ui_patch_batcher.rs`：`UiPatchBatcher`
  （`enqueue`/`flush`/`note_ack`/`note_rejected`）。SetProperty 按
  `(node_path, property)` last-value-wins 合并；结构/transition/input 操作保持
  到达顺序；ack 未返回时 `flush` 拒绝发出下一批（上表 “renderer ack 未返回” 行）。
- `crates/neon-ui-runtime/src/bin/ui_patch_coalesce_probe.rs`
  （`ui-patch-coalesce.v1`，JSONL）6/6 pass：
  `fifty_events_one_patch`（50 事件 → 1 patch、50 ops、apply 后 fixed point）、
  `token_stream_last_value_wins`（16 次 token → 1 个 set、0 结构 op）、
  `ack_gate_and_strict_revisions`（base 11→12→13 严格递增、2 次被 gate 阻塞）、
  `duplicate_state_no_patch`、`mixed_ops_keep_order_and_converge`、
  `rejection_adopts_authoritative_revision`（rejected 后采用权威 revision 并丢弃旧批）。
- 时间窗 flush 策略（< 16ms tick、30-60ms token coalesce、immediate flush）属于
  event loop 调度，推迟到 Phase 7 落地；本阶段固化的是合并语义与 revision 纪律。
- 验证：`cargo test -p neon-ui-runtime --lib` 198/198（含 batcher 5 个单元测试）、
  probe 6/6、`cargo fmt -- --check` 干净、clippy 对新增文件 0 告警。

---

## 6. Phase 4：Neon3 runtime patch pipeline 优化

状态：`DONE（2026-09-19，R2/R3 按编译器调研结论收敛为安全子集）`

### 目标

减少 `ui.flow.patch` 的无关工作，同时保持 IR/program/fragment 一致。

### 任务

| ID | 工作 | 验收 |
|---|---|---|
| R1 | patch dry-run 返回 impacted nodes | 只包含受影响节点 |
| R2 | 区分 property-only 和 structure patch | property-only 不触发不必要结构重建 |
| R3 | 缓存未受影响的 compiled resources | 资源 digest 不变时不重复创建 |
| R4 | patch 编译失败保持旧 program | 失败后旧画面仍可用 |
| R5 | patch render 失败回滚 | 返回明确 fallback reason |
| R6 | 增加 patch telemetry | patch_apply/compile/fragment/reconcile 分开统计 |

注意：不能为了性能绕过 revision 校验、source hash 或 renderer ack。

### 完成证据（2026-09-19）

- R1：`ui.flow.patch` 新增 `dry_run: true`。patch 在 IR 克隆上完整校验
  （含 revision 校验），返回 `impacted_nodes` / `patch_kind` / `would_apply_revision`，
  不 compile、不 submit、不改动任何 runtime 状态；dry-run 后同 revision 的真实 patch 仍可用。
- R2：每个 patch 响应携带 `patch_kind`（`property_only` / `structural`）。编译器调研
  （Explore 报告）证明 program 内嵌节点属性值（node_templates、layout_records、literal
  text handles、layout_hash、glyph 容量门），因此 **program/adapter 级 property-only 复用不安全**，
  未实现；安全子集落地为：`NuiFlowStateMachineRuntime` 不再每 patch 重建
  （`state_machines` 声明不可能被 patch 触及，旧实现每次 patch 都会重置活着的 statechart 状态）。
- R3：source-file digest 缓存 `UiRuntime::source_file_digests`：`source_file` 绑定文件的
  (mtime, size) digest 不变时跳过磁盘读取与全文 clone；digest 与 flow_document 在同一
  commit 点提交，失败/被拒的 patch 会强制下次重读。wgpu renderer 侧的按 key 资源复用
  （created==0）已在 Phase 1 K3 证明。
- R4：compile 失败 / stale revision / 非法 patch / activation 失败全部改为结构化
  `rejected`，error code 稳定（`ui_flow_patch_stale_revision`、`nui_flow_compile`、
  `ui_flow_patch_apply_failed`、`ui_flow_activation_failed`、`ui_flow_patch_params_invalid`、
  `ui_flow_patch_no_active_flow`），result 携带
  `{"state":"patch_rejected","fallback":"previous_program_retained","retained_revision":N}`；
  旧 program/adapter/fragment 保持不变，后续健康 patch 正常 accepted。
- R5：renderer 拒绝替换 fragment 时返回
  `{"state":"patch_render_fallback","fallback_reason":"<renderer error code>"}`，
  不提交任何状态，同一 patch 可在原 revision 直接重试。
- R6：accepted patch 响应携带 `timing_ms` 全阶段拆分（patch_apply/compile/fragment/forward…）
  与 renderer ack 的原始 result（`renderer` 字段，含 graph revision / reconcile 信息）。
- Probe：`ui_patch_revision_probe`（`ui-patch-revision.v1`）5 case 全绿，含 scripted
  renderer rejection；`ui_patch_baseline_probe` 全场景回归通过。
- 验证：`cargo test -p neon-ui-runtime --lib` 198/198、revision probe 5/5、baseline probe
  pass、`cargo fmt -- --check` 干净、clippy 对改动文件 0 告警。

---

## 7. Phase 5：Neon IDE 两棵 UI projection

状态：`DONE`（2026-09-19）

### 7.1 FileTreeProjection

```text
scan_files
  -> FileTreeProjection
    -> UiTreeDiff
      -> sidebar patch
```

支持：

- add
- remove
- rename
- select
- directory expand/collapse
- visual operation state

### 7.2 AgentWorkbenchProjection

```text
AgentWorkbenchState
PlanRuntime/task-graph
Coordinator projection
  -> AgentWorkbenchProjection
    -> UiTreeDiff
      -> panel patch
```

稳定节点：

```text
agent.header
agent.status
agent.plan.list
task:<plan>/<task>
transaction:<id>
change:<id>
approval:<id>
```

状态变化只修改属性；列表变化才 insert/remove/move。

### 验收

- 点击切换文件不触发完整 Flow submit。
- Plan task 状态变化不触发完整 Flow submit。
- 新增 task 只 insert 一个节点。
- 删除 task 只 remove 一个节点。
- 面板切换不重建整个 workspace。

### 完成证据（2026-09-19）

- 新增 `crates/neon-ui-runtime/src/ide_projection.rs`：`FileTreeProjection` +
  `AgentWorkbenchProjection` + `IdeWorkspaceProjection`。projection 直接构造与 Flow
  parser 归一化形状逐字段一致的 `UiNode` 树；`generated_source_parses_back_to_the_projection_tree`
  单测证明 `initial_source()` 解析回的空 diff（builder/parser parity）。
- 计划中的 `task:<plan>/<task>` 稳定 key 因 Flow `valid_key` 词汇限制（不允许 `:`，`/`
  会破坏语义 path）改用单射编码 `encode_key_segment`（`-`→`_h`、`.`→`_d`、`/`→`_x2f`、
  `_`→`__`），key 语义不变：状态变化只 set 属性，列表变化才 insert/remove。
- `IdeWorkspaceProjection::sync()` 走 Phase 2 keyed diff → `build_ui_patch`，本地对
  baseline IR 回放推进 revision；任何无法增量补丁的情况显式返回
  `IdeProjectionUpdate::FullSubmitRequired`，禁止 silent no-op。IR `move_node` 忽略
  index（追加到尾部），因此 projection 的顺序语义全部通过 insert/remove 表达，不 emit move。
- 单测：`cargo test -p neon-ui-runtime --lib` 212/212，其中本模块 14 个（select/switch
  仅 2 个 set、expand/collapse 连续 insert/remove、rename = 1 remove + 1 insert、
  busy = opacity set、无变化 = NoChange、task 状态 = 单 set、complete 释放 dependent =
  1 insert + 1 set、add/remove task 单 op、面板切换仅 `visible` set 且不出 agent 子树、
  approval/change resolve 保持 key、key 编码单射且 Flow-valid）。
- Probe（真实 `serve_forwarder` RPC 管线 + headless fake renderer，1 次 submit 之后全部
  走 `ui.flow.patch`）：
  - `file-tree-incremental.v1` 8/8：select/switch/busy 为 `property_only`；collapse =
    2 remove + 1 set（目录标记）、expand = 2 insert + 1 set；add/remove/rename 分别为
    1 insert / 1 remove / 1 remove + 1 insert；revision 7→15，renderer frame 1→9。
  - `agent-task-incremental.v1` 7/7：status line、task 状态、approval resolve 均为单
    set `property_only`；add_task/record_transaction 各 1 insert；remove_task 1 remove；
    面板切换恰好 `set workspace/agent/agent.section.records.visible` 且 0 个 op 触及
    sidebar；每 patch 端到端 total ≈ 2.6–4.5ms。
  - `plan-dependency-incremental.v1` 3/3：依赖未满足的 task 不在 UI 树中，对其状态改动
    = `NoChange`（0 跨进程流量）；complete 直接/链式 dependent 各恰好 1 insert + 1 set
    （`structural`）。
- 验收层级：`service-ready` + probe 级 `composition-ready`（renderer ack 计数验证）；
  外部 Neon IDE host 切换到这棵 projection 属于后续接入工作。


---

## 8. Phase 6：动态效果和交互反馈

状态：`IN_PROGRESS`（Stage 6a/6b `DONE` 2026-09-19）

动态效果必须绑定状态变化，不使用无意义的常驻动画。

| 状态 | 动效 |
|---|---|
| Running | 轻量 pulse/sweep，持续但低频 |
| Waiting approval | amber pulse + 明确按钮高亮 |
| Blocked | warning tint + 一次性进入动画 |
| Completed | 一次 success sweep，不持续闪烁 |
| Failed | error flash + Retry button 出现 |
| Queued | 低强度等待动画 |

要求：

- 动效不改变布局尺寸。
- 动效不触发 Flow remount。
- 动效只通过 `StartTransition`、state token 或 renderer-local transition。
- 失败状态不能只靠颜色表达，必须有文字和操作。

### Stage 6a 完成证据（2026-09-19）

- 修复 `ui.flow.patch` RPC 通道对 presentation op 的处理：此前 adapter 只识别
  legacy `kind` 形状，`start_transition` / `replace_children` / `set_input` 会被静默
  降级为默认 "set"，`StartTransition` 永远无法到达 IR。现在含 presentation op 的
  patch 走正式 `neon_ui_schema::UiPatch` envelope（在 legacy 归一化之前捕获原始
  `operations`，规避 `deny_unknown_fields`），由 IR 级 `apply_ui_patch` 应用：
  单次 revision bump + validate，不 remount、不改布局尺寸。
- 禁止 silent no-op：presentation op 与 legacy `kind` 形状混用时结构化拒绝
  `ui_flow_patch_params_invalid`，文档 revision 不变；stale revision 仍映射稳定码
  `ui_flow_patch_stale_revision`（提取为 `patch_apply_error_code` helper）。
- `patch_kind` 分类扩展：presentation 路径下 set_property/start_transition/set_input
  全部计入 `property_only`，混入结构 op 才报 `structural`。
- 测试：新单测 `rpc_start_transition_patch_lands_on_the_ir_with_one_revision_bump`
  端到端（真实 runtime + fake renderer 转发）断言 Accepted、`patch_kind =
  property_only`、revision 恰好 +1、目标节点 `enter_transition` 与提交的
  `UiTransition`（含 `motion_key`）逐字段相等、混形 patch 被
  `ui_flow_patch_params_invalid` 拒绝且 revision 不变。
  `cargo test -p neon-ui-runtime --lib` 213/213。
- 未完成（Stage 6c）：renderer 侧“立即触发该 transition”入口（当前动画引擎仅在目标
  visual 变化时启动 track）。

### Stage 6b 完成证据（2026-09-19）

- projection 层动效意图 API：`AgentWorkbenchProjection::set_task_status` 在状态真正
  变化时入队一次性 `UiTransition`（Completed = success sweep 240ms、Failed = error
  flash 180ms，均只改 opacity，`motion_key` 单调序号防重放）；`sync()` 把意图排空为
  `StartTransition` ops 追加到同一 patch，与 diff 共享一次 revision bump。
- 约束：transition 只允许命中当前文档中已存在的行（IR 在 patch inserts 之前解析
  StartTransition），未露面依赖任务的终态改动 = `NoChange`（0 跨进程流量）；tree 无
  变化但有 motion 时发 motion-only patch（仍单 revision bump）。retry 按下是纯状态
  变化（remove + set，0 transition）。
- 失败态不止颜色：行文本 `plan / name: failed` + 新增 Button-kind `task.<p>.<n>.retry`
  操作行（文本 `retry plan / name`），随 patch 增量 insert，retry 后增量 remove；
  Flow writer/parser 对 `button` 节点类型补齐（round-trip parity 测试）。
- 单测：6 个新用例（sweep 同 patch、failed 三 op 合成、retry 无动效、隐藏任务静默、
  motion-only patch、button parity），`cargo test -p neon-ui-runtime --lib` 219/219。
- Probe（真实 `serve_forwarder` RPC）：`motion-feedback-incremental.v1` 3/3
  （failed = insert+set+transition `structural`、retry = remove+set 无 transition、
  complete = set+transition `property_only`）；Phase 5 三个 probe 同步更新断言后
  8/8、7/7、3/3 全绿。
- 已知缺口（如实记录，计入后续阶段）：程序事件表（`node_key -> intent`）只在初始
  submit 的 Flow source 中声明，IR patch insert 的 Button 暂时无法携带 intent；
  retry 按钮当前是 presentation + 结构正确的操作行，intent 接线需要 program 层补丁
  通道（或预声明事件节点），归入 6c 之后的交互接线阶段。Running/Queued 低频持续
  动效属 renderer-local 常驻动画，同样依赖该后续（避免常驻伪动效）。
- 验收层级：`service-ready` + probe 级 `composition-ready`；未声称
  `wgpu-rendered`/`interactive-accepted`。

---

## 9. Phase 7：低延迟事件循环

状态：`IN_PROGRESS`

### 必须完成

- 所有事件订阅使用非阻塞 poll 或统一 event multiplexer。
- 禁止在一个 loop tick 内串行等待多个 1ms timeout。
- UI patch flush 与 event polling 分离。
- Agent streaming 不得每 token full remount。
- 使用 bounded sleep/yield，避免 CPU busy loop。

### 指标

```text
button event -> state mutation < 2ms
state mutation -> patch enqueue < 2ms
patch enqueue -> runtime accepted < 50ms local target
runtime accepted -> rendered frame bounded and observable
```

---

## 10. Probe 和验收矩阵

每个跨边界行为都必须有 JSONL probe。

| Probe | 验证 |
|---|---|
| `ui_patch_keyed_diff_probe` | insert/remove/move/set |
| `ui_patch_coalesce_probe` | 50 状态变化合并为 1 patch |
| `ui_patch_revision_probe` | stale/duplicate/ack/retry |
| `ui_reconcile_retained_probe` | stable key 复用 renderer object |
| `file_tree_incremental_probe` | 文件树增删改不 full submit |
| `agent_task_incremental_probe` | task 状态只改节点属性 |
| `plan_dependency_incremental_probe` | A 完成后 B 自动 insert/start |
| `ui_latency_probe` | event-to-frame latency |

所有 probe 必须输出：

```json
{
  "input": {},
  "frame_sequence": 0,
  "program_revision": 0,
  "flow_document_revision": 0,
  "operations": [],
  "retained": {},
  "timing_ms": {},
  "pass": true
}
```

---

## 11. 依赖顺序

```text
Phase 0 性能基线
  -> Phase 1 stable key
    -> Phase 2 keyed diff
      -> Phase 3 coalescing
        -> Phase 4 Neon3 patch pipeline
          -> Phase 5 neon-ide 两棵 projection
            -> Phase 6 动效
              -> Phase 7 延迟收口
```

禁止跳过 Phase 0。没有 telemetry 就不能判断优化是否有效。

---

## 12. 交给其他 AI 的执行规则

每个 AI 只领取一个 Phase 或一个明确 ID。

执行前：

1. 读取本文档。
2. 读取 `AGENTS.md`。
3. 检查当前 git diff，不覆盖已有工作。
4. 说明要修改的文件和不修改的边界。

执行中：

1. 先写 focused test/probe。
2. 再改实现。
3. 不把状态变化实现成无条件 remove/insert。
4. 不绕过 revision、ack、permission、authority。
5. 不把 full remount 当成正常高频更新路径。

执行后：

1. 运行 focused probe。
2. 运行相关 crate tests。
3. 运行 `cargo fmt -- --check`。
4. 运行 `cargo clippy --all-targets -- -D warnings`。
5. 更新本文档状态和结果。
6. 报告 warning 与 failure 分离结果。

---

## 13. 完成定义

底层优化完成必须同时满足：

- 稳定 key 的节点可以 retained reuse。
- property-only 状态变化不触发结构重建。
- 结构变化只产生最小 Insert/Remove/Move 集合。
- 同一 frame 的状态变化被合并。
- patch revision/ack 严格正确。
- patch 失败后旧画面保持可用。
- 文件树增删改不依赖 full Flow remount。
- Agent/Plan/Transaction 状态更新不依赖 full Flow remount。
- 按钮点击到 patch/frame 有可测低延迟。
- 动效不引起布局跳动或重复 mount。
- 所有跨边界行为有 JSONL 验收证据。
