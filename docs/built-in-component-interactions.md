# Neon3 组件内置交互机制设计原理

> 版本: v1.0 | 日期: 2026-09-12 | 状态: 设计稿

## 1. 背景与问题

### 1.1 当前架构

Neon3 的 UI 架构分为两层：

```
┌─────────────────────────────────┐
│  neon-ui-runtime (业务/语义层)   │
│  - input 状态管理                 │
│  - 事件路由                       │
│  - 业务逻辑                       │
└──────────────┬──────────────────┘
               │ submit_fragment / ui.host.inbound
┌──────────────▼──────────────────┐
│  neon-wgpu-runtime (渲染层)      │
│  - 布局/渲染                      │
│  - hit-testing (ID buffer)       │
│  - 指针事件捕获                   │
└─────────────────────────────────┘
```

WGPU Runtime 捕获指针事件后，通过 `ui.host.inbound` RPC 将 `UiSemanticEvent` 转发给 UI Runtime，由 UI Runtime 更新状态并重新提交 fragment。

### 1.2 问题

对于组件的**固定交互逻辑**（如 Checkbox 点击切换勾选、TreeView 点击展开折叠、Slider 拖拽改值），每次使用都需要外部自己接事件、更新状态、重新提交 fragment。这导致：

1. **重复劳动**：每个案例/应用都要重新实现一遍基础交互
2. **状态分散**：组件状态存储在外部，渲染器不知道当前状态
3. **视觉延迟**：必须等外部重新提交 fragment 才能看到视觉变化
4. **案例复杂**：showcase 等测试案例需要写大量状态管理代码

### 1.3 目标

将组件的**固定交互逻辑内置到 WGPU Runtime**，外部只需要接**自定义业务逻辑**。

---

## 2. 设计原理

### 2.1 核心原则

> **内置交互负责视觉和状态，固定事件负责通知外部。**

```
用户交互
    ↓
┌──────────────────────────────────────┐
│  WGPU Runtime 内置交互处理器          │
│                                      │
│  1. 查组件类型 → 匹配内置行为        │
│  2. 更新内部状态表                   │
│  3. 标记重绘 → 视觉立即反馈          │
│  4. 发送 UiSemanticEvent → 外部监听  │
└──────────────────────────────────────┘
    ↓
外部收到事件后可以：
- 做自定义业务逻辑（勾选后触发什么）
- 记录日志/埋点
- 通过重新提交 fragment 覆盖内部状态
```

### 2.2 两层交互分离

| 层级 | 职责 | 实现位置 |
|------|------|---------|
| **内置交互** | 组件固定行为（勾选、拖拽、展开折叠） | WGPU Runtime |
| **业务交互** | 自定义逻辑（勾选后触发什么操作） | 外部 UI Runtime / 客户端 |

内置交互是**默认行为**，外部可以通过事件监听做额外逻辑，也可以通过重新提交 fragment 覆盖状态。

### 2.3 事件驱动的状态同步

内置交互更新内部状态后，**必须**发送固定格式的 `UiSemanticEvent` 给外部。原因：

1. **可观测性**：外部能知道发生了什么交互
2. **业务逻辑**：外部可以基于事件触发自定义逻辑
3. **状态同步**：外部可以将内部状态同步到自己的状态管理
4. **向后兼容**：现有基于事件的外部逻辑不需要修改

---

## 3. 架构设计

### 3.1 组件行为表

在 `neon-ui-schema` 中定义每个组件的内置行为元数据：

```rust
pub enum BuiltinInteraction {
    /// 点击切换布尔值（Checkbox）
    ToggleClick,
    /// 点击选中（同组互斥，RadioButton）
    RadioSelect { group: String },
    /// 按钮按下/释放（无持久状态）
    ButtonPress,
    /// 拖拽改数值（Slider/Scrollbar/DragValue）
    DragNumeric { axis: DragAxis, min: f32, max: f32 },
    /// 点击展开/折叠（TreeView 节点）
    TreeToggle,
    /// 拖拽调整分割比例（Splitter）
    SplitterDrag,
    /// 点击选中项（Selectable/ListBox/Tab）
    ItemSelect,
    /// 键盘输入文本（TextInput）
    TextInput,
    /// 右键显示/点击外部关闭（ContextMenu）
    ContextMenu,
    /// 点击展开/选择项（Combo/Dropdown）
    DropdownSelect,
}
```

### 3.2 内部状态存储

在 WGPU Runtime 中维护 `ComponentStateStore`：

```rust
pub struct ComponentStateStore {
    /// node_path -> 布尔值（Checkbox/Radio/Tree 展开）
    toggles: HashMap<String, bool>,
    /// node_path -> (value, min, max)（Slider/Scrollbar/Progress）
    numerics: HashMap<String, (f32, f32, f32)>,
    /// node_path -> 文本值（TextInput）
    texts: HashMap<String, String>,
    /// node_path -> 选中索引（Tabs/ListBox/Combo）
    selections: HashMap<String, i32>,
    /// node_path -> 可见性（ContextMenu/Dropdown 弹出层）
    visibilities: HashMap<String, bool>,
    /// node_path -> 分割比例（Splitter）
    split_ratios: HashMap<String, f32>,
}
```

- key 使用 `node_path`（如 `showcase/check-demo`）
- 初始值从 fragment 的 `ControlPresentation` effects 中提取
- 交互时更新内部状态
- 渲染时，内部状态**覆盖** fragment 中的 ControlPresentation

### 3.3 交互处理流程

修改 WGPU Runtime 的 pointer 事件处理：

```
pointer_down / pointer_move / pointer_up
    ↓
hit-test → hit_id → UiHitBinding
    ↓
查组件类型 → 有内置行为？
    ├─ 是 → 执行内置行为
    │        ├─ 更新 ComponentStateStore
    │        ├─ mark redraw_pending
    │        └─ 有 ui_endpoint → forward_pointer_click / forward_drag_drop
    └─ 否 → 有 ui_endpoint → forward_pointer_click（外部处理）
```

### 3.4 渲染时状态合并

在 `refresh_plan` / `compose_sampled_visuals` 阶段：

1. 从 fragment 中提取 ControlPresentation effects 作为初始值
2. 用 ComponentStateStore 中的值覆盖（如果存在）
3. 将最终的 ControlPresentation 应用到节点视觉

这样外部提交的 fragment 可以设置初始值，内置交互可以动态修改，外部也可以通过重新提交 fragment 来重置/覆盖。

---

## 4. 组件行为详细定义

### 4.1 点击型组件

| 组件 | 交互 | 内部状态 | 事件类型 | control_value |
|------|------|---------|---------|---------------|
| **Checkbox** | 点击 | `toggles[path] = !old` | `PointerClick` | `Bool { value: new }` |
| **RadioButton** | 点击 | 同组其他设 false，自身设 true | `SelectionChanged` | `Bool { value: true }` |
| **Button** | 按下/释放 | 无持久状态，仅 pressed 视觉 | `PointerClick` | 无 |
| **TreeView 节点** | 点击 | `toggles[path] = !old` | `PointerClick` | `Bool { value: expanded }` |
| **Selectable** | 点击 | `selections[parent] = index` | `SelectionChanged` | `I32 { value: index }` |
| **ListBox item** | 点击 | `selections[parent] = index` | `SelectionChanged` | `I32 { value: index }` |
| **Tab** | 点击 | `selections[parent] = index` | `SelectionChanged` | `I32 { value: index }` |

### 4.2 拖拽型组件

| 组件 | 交互 | 内部状态 | 事件类型 | control_value |
|------|------|---------|---------|---------------|
| **Slider** | 拖拽中 | `numerics[path].0 = new` | `ValuePreview` | `F32 { value: new }` |
| **Slider** | 释放 | 同上 | `ValueCommit` | `F32 { value: final }` |
| **Scrollbar** | 拖拽/点击轨道 | `numerics[path].0 = new` | `ValueCommit` | `F32 { value: new }` |
| **DragValue** | 拖拽 | `numerics[path].0 = new` | `ValuePreview`/`ValueCommit` | `F32 { value }` |
| **Splitter** | 拖拽中 | `split_ratios[path] = ratio` | `ValuePreview` | `F32 { value: ratio }` |
| **Splitter** | 释放 | 同上 | `ValueCommit` | `F32 { value: ratio }` |

### 4.3 复杂组件

| 组件 | 交互 | 内部状态 | 事件类型 |
|------|------|---------|---------|
| **TextInput** | 键盘输入 | `texts[path] = new` | `TextInputCommit`（回车/失焦） |
| **ContextMenu** | 右键触发 | `visibilities[path] = true` | `PointerClick` |
| **ContextMenu** | 点击外部/菜单项 | `visibilities[path] = false` | `PointerClick` |
| **Combo/Dropdown** | 点击展开 | `visibilities[path] = !old` | `PointerClick` |
| **Combo/Dropdown** | 选择 item | `selections[path] = index` | `SelectionChanged` |
| **ProgressBar** | 只读 | - | 无交互 |

---

## 5. 事件格式

### 5.1 UiSemanticEvent（保持现有格式）

```json
{
  "event": "pointer_click",
  "event_id": "wgpu-pointer-click-123",
  "renderer_epoch": 1,
  "composition_revision": 42,
  "fragment": { "id": "showcase", "revision": 5 },
  "intent": {
    "invoke": {
      "action": "demo.check.toggle",
      "params": {}
    }
  },
  "pointer": { "id": 0, "sequence": 123 },
  "control_value": { "bool": { "value": true } },
  "text": null,
  "drag_drop": null,
  "data_grid_cell": null
}
```

### 5.2 事件类型枚举

```rust
pub enum UiSemanticEventType {
    PointerClick,      // 点击完成（Checkbox/Button/Tree 节点）
    ValuePreview,      // 值预览（Slider 拖拽中）
    ValueCommit,       // 值提交（Slider 释放/Scrollbar）
    SelectionChanged,  // 选中变化（Radio/Tabs/ListBox）
    TextInputCommit,   // 文本输入提交
    DragDrop,          // 拖拽完成
    FocusChanged,      // 焦点变化
    InteractionCancelled, // 交互取消
}
```

### 5.3 control_value 类型

```rust
pub enum UiSemanticPayloadValue {
    Bool { value: bool },
    I32 { value: i32 },
    F32 { value: f32 },
    TextHandle { value: u64 },
    Enum { value: String },
}
```

---

## 6. 与外部的关系

### 6.1 外部可以做什么

1. **监听事件**：通过 `ui.host.inbound` 接收所有交互事件
2. **业务逻辑**：基于事件触发自定义操作（如勾选后保存配置）
3. **覆盖状态**：通过重新提交 fragment（带新的 ControlPresentation）覆盖内部状态
4. **禁用内置**：通过节点标记（如 `no_builtin_interaction`）禁用特定组件的内置行为

### 6.2 外部不需要做什么

1. ~~不需要自己管理 Checkbox 勾选状态~~
2. ~~不需要自己处理 TreeView 展开折叠~~
3. ~~不需要自己计算 Slider 拖拽值~~
4. ~~不需要每次交互都重新提交 fragment~~（除非要覆盖）

### 6.3 向后兼容

- 事件格式和传输方式完全不变
- 现有外部 UI Runtime（如 DemoInputDomain）不需要修改
- 没有设置 `ui_endpoint` 时，内置交互仍然工作（只是不转发事件）
- 外部重新提交 fragment 时，内部状态会被新的 ControlPresentation 重置

---

## 7. 实施步骤

### 阶段 1：基础框架
- [ ] 在 `neon-ui-schema` 定义 `BuiltinInteraction` 枚举和组件行为表
- [ ] 在 WGPU Runtime 实现 `ComponentStateStore`
- [ ] 修改 pointer 事件处理，接入内置行为分发
- [ ] 渲染时合并内部状态和 fragment 状态

### 阶段 2：点击型组件
- [ ] Checkbox 点击切换勾选
- [ ] RadioButton 选中互斥
- [ ] Button pressed 视觉反馈
- [ ] TreeView 节点展开折叠
- [ ] Selectable/ListBox/Tab 选中

### 阶段 3：拖拽型组件
- [ ] Slider 拖拽改值
- [ ] Scrollbar 拖拽/点击轨道
- [ ] DragValue 拖拽改值
- [ ] Splitter 拖拽调整面板

### 阶段 4：复杂组件
- [ ] TextInput 键盘输入
- [ ] ContextMenu 右键显示/关闭
- [ ] Combo/Dropdown 展开选择
- [ ] Tabs 切换

### 阶段 5：集成与文档
- [ ] 事件转发验证
- [ ] 外部覆盖/禁用机制
- [ ] showcase 简化（移除手动状态管理）
- [ ] 文档和示例

---

## 8. 设计权衡

### 8.1 为什么内置到 WGPU Runtime 而不是 UI Runtime

- **视觉即时反馈**：不需要等外部重新提交 fragment
- **减少 IPC**：基础交互不需要跨进程通信
- **案例简单**：测试案例不需要启动完整 UI Runtime
- **渲染器知道布局**：Slider 拖拽值计算需要知道轨道几何，渲染器最清楚

### 8.2 为什么仍然转发事件

- **业务逻辑需要**：外部需要知道交互发生了
- **可观测性**：调试和日志需要事件流
- **向后兼容**：现有基于事件的架构不变
- **状态同步**：外部可以同步内部状态到自己的状态管理

### 8.3 状态存储位置

内部状态存储在 WGPU Runtime 而不是 UI Runtime，因为：
- 渲染器需要立即访问状态来渲染
- 减少跨进程状态同步
- 外部可以通过事件获取状态变化，也可以通过重新提交 fragment 来设置状态

---

## 9. 参考实现

- `neon-ui-schema/src/lib.rs` — UiSemanticEvent, UiControlPresentation, UiEffect
- `neon-wgpu-runtime/src/ui_renderer.rs` — hit-testing, refresh_hit_bindings, draw_hit_id
- `neon-wgpu-runtime/src/lib.rs` — pointer 事件处理, forward_pointer_click, forward_drag_drop
- `neon-ui-runtime/src/demo_domain.rs` — DemoInputDomain（外部 UI Runtime 参考实现）
