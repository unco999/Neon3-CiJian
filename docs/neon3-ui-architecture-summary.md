# Neon3 UI 架构总结

## 核心目标

Neon3 UI 的目标不是“每次状态变化都重新生成 UI”，而是：

```text
input change
  -> dependency impact
  -> retained frame delta
  -> semantic node delta
  -> WGPU sparse buffer writes
  -> final pixels
```

更新成本应当与受影响节点数量相关，而不是与整个 UI 树规模相关。

## 各层职责

### NUI Flow

NUI Flow 只声明：

- UI 树结构和节点 ID。
- 控件类型、布局、样式和可见性。
- input 声明及其默认值/范围。
- input 到节点属性的依赖关系。
- semantic event 名称。
- 允许的本地 presentation statechart。

NUI Flow 不负责：

- 领域规则。
- 直接写项目或业务状态。
- 创建 wgpu 资源。
- 通过坐标或控件编号操作 UI。
- 用复杂脚本模拟增量更新。

### UI Runtime

UI Runtime 负责接收 semantic event，并由业务/领域层发布新的 revisioned `UiInputFrame`。
它维护 retained frame、input revision 和依赖图：

```text
semantic event
  -> typed input frame
  -> dirty slots
  -> impacted bindings
  -> changed nodes
```

### WGPU Runtime

WGPU Runtime 是唯一的窗口、wgpu resource 和最终 composition owner。
它维护已提交的 fragment 和 semantic node range，将 `UiFragmentDelta` 应用到 resident fragment，
再执行 color/depth/text/image/hit 等 buffer 的局部写入。

## 增量因果更新

例如：

```text
input theme_dark
  -> light-theme.visible
  -> dark-theme.visible
  -> bar-0-light.visible
  -> bar-0-dark.visible
  -> bar-3-light.visible
  -> bar-3-dark.visible
```

改变 `theme_dark` 时：

1. input store 增加 input revision。
2. 依赖图只标记受影响 binding。
3. retained evaluator 只重新计算这些 binding。
4. 生成 `changed_nodes`。
5. UI Runtime 发送 `UiFragmentDelta`。
6. WGPU Runtime 按 `base_revision` 应用 delta。
7. renderer 只更新受影响的 instance/buffer range。

没有被依赖图命中的节点必须保持原有 retained 数据，不得重新 flatten 或重新上传。

## Full rebuild 允许条件

Full rebuild 只适用于结构性变化：

- 首次提交。
- NUI Flow 程序版本变化。
- 节点插入或删除。
- 节点层级/排序变化。
- layout 拓扑变化。
- fragment base revision 不一致。
- runtime epoch 变化或 resident fragment 丢失。

普通 input、visible、opacity、文本值、颜色状态、选中状态和局部样式变化不得触发 full rebuild。

## 写 UI 时的规则

1. 每个会变化的值先声明为 typed input，不要用隐含的本地变量模拟业务状态。
2. 每个动态节点使用稳定、语义化的 node ID。
3. 将 input 绑定到最小范围的节点属性，避免把整个大容器绑定到一个小状态。
4. 一个 input 可以影响多个节点，但依赖关系必须是明确、可追踪的。
5. 样式切换优先使用两个明确的 retained 分支，例如 `visible $theme_light` 和 `visible $theme_dark`。
6. 不要在 NUI Flow 中使用未声明的 input；parser 会拒绝 unknown binding target。
7. 不要依赖 `!$input` 这类未被该属性支持的表达式；需要互斥状态时声明两个 input，由 runtime 一次提交。
8. 不要在每次 input 变化时重新生成整棵 fragment。
9. 不要在每帧无条件重建 instance、text、image 或 hit 数据。
10. 不要把 `submit_rpc` 当作完整视觉响应时间；它只是控制面传输/确认耗时。

## 诊断必须区分

一次更新至少要能区分：

```text
input_apply_ms
retained_evaluate_ms
changed_bindings
changed_nodes
delta_encode_ms
delta_submit_ms
renderer_apply_ms
gpu_write_bytes
frame_sequence
```

`changed_nodes` 是逻辑影响范围，不是 GPU 写入字节数；`submit_rpc_ms` 是 RPC 时间，不代表屏幕已经完成显示。

## UI 验收方式

复杂案例至少包含：

- 多层级面板。
- 表格、文本、图表或柱状图。
- 一个 semantic control。
- 一个 input 同时影响多个局部样式。
- 与 input 无关的静态节点。

验收流程：

```text
点击控件
  -> input revision +1
  -> changed_nodes 正确
  -> delta accepted
  -> unrelated nodes 未变化
  -> 画面确实改变
  -> 下一次点击可以恢复
```

日志显示 `delta_applied=true` 不能单独证明画面改变。必须确认 delta 已触发窗口 composition refresh，并有最终像素或明确的 consumer render evidence。

## 开发完成检查

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test -p neon-ui-runtime --lib
cargo test -p neon-wgpu-runtime --lib <focused-test>
cargo run -p neon-wgpu-runtime --bin <incremental-probe>
```

每次涉及 input、delta 或 renderer 的修改，都要记录：

- input key 和 revision。
- changed bindings/nodes。
- 是否 full rebuild。
- delta 是否被 consumer 应用。
- renderer 实际写入范围。
- 最终视觉验证结果。
