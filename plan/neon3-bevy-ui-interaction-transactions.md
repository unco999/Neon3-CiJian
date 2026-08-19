# Neon3 Bevy UI 交互事务与状态边界设计

> 状态：讨论稿，尚未实施。
>
> 本文只定义 Bevy 外部宿主与 Neon UI 的交互事务、ID 图拾取与变量权威边界。它不改变
> `AGENTS.md` 中 `neon-wgpu-runtime` 是唯一窗口和 GPU owner 的约束。

## 1. 目标

Bevy 宿主中的 World UI 和普通 UI 由 Neon 在同一张最终 composition 中绘制。普通 UI 永远
位于 World UI 之前。Neon 向 Bevy 导出同一帧、同一缓冲槽的三张只读 target：

```text
color   rgba8unorm  最终 UI 像素
depth   r32float    World UI 与 Bevy 场景 depth 合成所需的遮挡深度
pickId  r32uint     最终可交互 UI 的像素命中图
```

用户点击时，Bevy 使用 `pickId` 判断该像素是否由 Neon UI 截获，但不解释其中的整数。Neon
负责把命中转换为稳定的 UI 语义目标；Bevy 负责将业务 intent 应用到 ECS/gameplay 状态。

需要同时满足：

1. World UI 与普通 UI 在视觉、深度、命中优先级上完全一致。
2. ID 图命中后可以在常数时间找到对应 UI node、UI 实例和 Bevy Entity。
3. UI 变量与 Bevy 业务变量不双向争夺写权，不能形成 pingpong。
4. 滚动、DataGrid selection、tab、展开等 UI 专属状态可由 UI Runtime 管理，并可在 Bevy
   中保存镜像供重启、诊断和下一帧输入恢复。
5. 任一旧帧、旧 surface generation、旧 renderer epoch 的交互结果都不能应用到当前 ECS。

## 2. 所有权

| 对象 | 唯一权威方 | Bevy 的角色 |
| --- | --- | --- |
| 窗口、原始 pointer、hit-test、pointer capture、ID 图、GPU hit ID | `neon-wgpu-runtime` | 接收已导出的 target 并提交原始 pointer sample |
| UI layout、可见/禁用、node 到 semantic intent 的绑定 | `neon-ui-runtime` / WGPU renderer presentation | 提交声明和变量输入 |
| `NeonWorldUi<V>.vars` 中的业务字段 | Bevy gameplay/ECS | 唯一写者 |
| `NeonWorldUi<V, U>.ui_vars` 中的 UI 专属字段 | UI Runtime | 保存镜像、按权威 patch 更新 |
| Bevy Entity 与稳定 UI 实例身份的关联 | Bevy host adapter | 唯一维护者 |
| 项目文件与持久化资产 | `neon-projectd` | 不直接写文件 |

`pickId` 是 renderer-local implementation detail。它绝不进入 project、domain command、
event journal 或 Bevy gameplay API。

## 3. 组件模型

Bevy 已有的泛型 World UI 组件扩展为两套不同归属的状态，而不是使用一个可双向写入的变量
struct：

```rust
#[derive(Component)]
pub struct NeonWorldUi<V: NuiFlowVars, U: NuiFlowVars> {
    /// Bevy/gameplay 权威字段，例如 health、mana、equipment、cooldown。
    pub vars: V,
    /// UI Runtime 权威字段，例如 DataGrid、scroll、tab、展开状态。
    pub ui_vars: U,

    pub flow: NeonFlowBinding,
    pub identity: NuiFlowIdentity,
    pub anchor: WorldAnchorId,
    pub offset: Vec3,
    pub visible: bool,
}

#[derive(Component)]
pub struct NeonUiInstance {
    /// 不使用 Bevy Entity number，必须可跨重启和帧稳定定位。
    pub stable_row_key: String,
}
```

示例：

```rust
pub struct CharacterStatusVars {
    pub health: f32,
    pub mana: f32,
    pub cooldown: f32,
}

pub struct CharacterStatusUiVars {
    pub grid: CharacterGridUiState,
    pub active_tab: u32,
    pub panel_open: bool,
}
```

不要求使用宏。宿主可直接为每种 Flow 定义 `V` 与 `U`，实现相同的 typed snapshot/diff/patch
trait 即可。若以后重复模式足够多，再单独引入声明宏；宏不能成为交互协议的前置条件。

`U` 不是任意 JSON map。它应有稳定、类型化、可比较的字段；复杂 UI 例如 grid 可定义自己的
专用值类型。网络边界仍由 schema 转为受限的 `UiInputValue` / `UiRepeatRow`，不传 Rust 指针、
Bevy `Entity`、GPU handle 或任意 struct。

## 4. 统一三目标与层级

每个导出帧生成相同的 resolved draw list，再由该 draw list 产生 color、depth、pickId。不能让
三个 target 分别重做布局、可见性或排序。

层级规则：

```text
1. WorldDepthTested
   写 color、world depth、pickId。
   Bevy 使用 Neon depth 与自己的 scene depth 判定遮挡。

2. WorldAlwaysVisible
   写 color、depth=0.0、pickId。
   depth=0.0 表示永远在 Bevy 场景前方。

3. ScreenOverlay
   最后写 color、depth=0.0、pickId。
   因为最后绘制，普通 UI 自然覆盖重叠区域的 World UI 与其 ID。
```

ID shader 对透明、clip 外、圆角外或 disabled 的像素执行 discard 或不分配 binding。`pickId=0`
（或 renderer 约定的 no-hit clear value）代表没有可交互 UI。

每个导出 buffer 的三个 target 必须共享：

```rust
pub struct UiFrameKey {
    pub renderer_epoch: u64,
    pub generation: u64,
    pub buffer_index: u8,
    pub frame_sequence: u64,
    pub composition_revision: Revision,
}
```

Bevy 不能混用 frame N 的 color、frame N-1 的 pickId 或 frame N+1 的 depth。三缓冲的 fence、
queue wait、release 和 generation 生命周期也必须以该 buffer slot 为单位处理。

## 5. ID 图的内容与解析

ID 图的一个像素只保存 `u32 hit_id`，不保存 panel 名、node path、Entity、intent、变量值或 JSON。

在一次 composition 中，WGPU renderer 为每个可交互 node 分配暂时的 `hit_id`，并建立私有表：

```rust
pub struct UiHitBinding {
    pub node_path: String,
    pub panel_node_path: String,
    pub fragment: UiFragmentRevision,
    pub instance_key: Option<String>,
    pub intent: Option<UiIntent>,
    pub text_input: Option<UiTextInputBinding>,
    pub data_grid_cell: Option<UiDataGridCellTarget>,
    pub control_value: Option<UiSemanticPayloadValue>,
}

pub struct PickFrame {
    pub key: UiFrameKey,
    /// index 0 保留给 no-hit；hit_id 可直接作为数组下标。
    pub bindings: Vec<UiHitBinding>,
}
```

`hit_id -> bindings[hit_id]` 是 O(1)。不扫描 ID 图，也不按点击遍历所有 panel 或所有
`NeonWorldUi` 实体。当前实现使用 `HashMap<u32, UiHitBinding>`，正式三缓冲路径应改为或封装为
generation/buffer-bound 的 `PickFrame` 稠密表，避免每次哈希查询，也避免旧帧错误引用最新 binding。

每个导出 buffer 必须保存自己的 `PickFrame`，直到 Bevy 使用 consumer release fence 释放该
buffer。不得只维护“最新的 hit binding 表”：同一个整数 `17` 在两个帧中可以对应不同 node。

## 6. Bevy 实体快速关联

Bevy 不以 renderer `hit_id` 查 Entity。Bevy 以 UI Runtime 回传的稳定目标查 Entity：

```rust
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct NeonUiTargetKey {
    pub stable_row_key: String,
    pub panel_node_path: String,
    pub node_path: String,
}

#[derive(Resource, Default)]
pub struct NeonUiInteractionRegistry {
    pub active_frame: Option<UiFrameKey>,
    pub entity_by_instance: HashMap<String, Entity>,
    pub entity_by_target: HashMap<NeonUiTargetKey, Entity>,
}
```

在实体创建、销毁、Flow 重绑定或 `stable_row_key` 改变时维护 `entity_by_instance`。在提交或收到
当前 Flow 的 interaction declaration 时，维护 `entity_by_target`。查找复杂度为 O(1)。

不需要每帧给 Entity 增删“可交互”组件。`NeonUiInstance` 是稳定组件；当前帧有效性由 registry
和 `UiFrameKey` 决定。可见性、enabled、裁剪和 scene-depth 遮挡由 Neon/Bevy 的当前帧判定，
过期的点击结果会因 `UiFrameKey` 不匹配而被丢弃。

## 7. 两种状态回路

### 7.1 Bevy 业务变量回路

`vars` 只由 Bevy gameplay/ECS 写入。典型字段：health、mana、cooldown、selection、equipment。

```text
用户点击
  -> Bevy 读取 pickId，确认 UI 截获
  -> UI Runtime 解析 binding，生成 semantic intent
  -> Bevy 收到 typed intent
  -> Bevy gameplay system 校验并修改 vars
  -> flush system 将 vars 的 sparse diff 提交给 UI Runtime
  -> UI Runtime 重算 UI，Neon 绘制下一帧
```

点击攻击按钮的结果必须是 `attack` intent，不能是 UI Runtime 直接回写 `health=76`。Bevy 是
业务规则、成功/失败和最终业务值的唯一裁决者。

### 7.2 UI 专属变量回路

`ui_vars` 只由 UI Runtime 写入。典型字段：scroll offset、DataGrid selection/sort、active tab、
dropdown open、panel open、文本草稿和局部 presentation state。

```text
用户点击或滚动
  -> UI Runtime 解析并修改自己的 UI state
  -> UI Runtime 返回 UiVarPatch
  -> Bevy 按稳定 target 找到 NeonWorldUi<V, U>
  -> 对 ui_vars 应用权威 patch，作为镜像/恢复状态
  -> 后续需要恢复或重新绑定时，Bevy 将该 ui_vars snapshot 提交给 UI Runtime
```

Bevy 不应在收到 `UiVarPatch` 后重新计算同一字段，再把“自己认为正确”的值写回 UI Runtime。
它只接受、保存和按需要恢复 UI Runtime 的权威结果。

## 8. 点击事务

### 8.1 Bevy 侧截获判定

Bevy 每帧导入同一 `UiFrameKey` 的 color、depth、pickId。在 pointer down/click 时：

1. 从当前已完成且仍有效的 buffer 读取单个 `pickId` 像素，禁止整图 CPU readback。
2. `pickId=no-hit` 时，交给 Bevy 自己的世界 picking 或输入系统。
3. `pickId!=no-hit` 时，若该像素是 `WorldDepthTested`，使用同帧 Neon depth 和 Bevy scene depth
   进行同一遮挡规则判定；被场景遮挡的 World UI 不截获 pointer。
4. 通过后向 UI Runtime 提交可靠 RPC。Bevy 不发送 raw `hit_id`，只发送 frame identity 与 pixel。

```json
{
  "method": "ui.host.inbound",
  "params": {
    "frame_key": {
      "renderer_epoch": 7,
      "generation": 3,
      "buffer_index": 1,
      "frame_sequence": 820,
      "composition_revision": 44
    },
    "pointer_id": 0,
    "sequence": 92,
    "pixel": [901, 422],
    "phase": "click"
  }
}
```

`pointer down`、`pointer up`、`cancel`、`interaction begin/end` 始终走可靠 RPC；高频 move/drag
遵守 `AGENTS.md` 的本地预览与可选 fast path 规则。

### 8.2 Neon 侧解析与应答

WGPU Runtime 用 request 中的 `UiFrameKey` 找到对应 `PickFrame`，从该帧 target 读取或验证命中，
再执行：

```text
pixel -> hit_id -> PickFrame.bindings[hit_id] -> UiHitBinding
```

UI Runtime 只接收稳定语义 binding，不接收 raw hit ID。它校验 fragment/program revision、visible、
enabled、instance/session 与 pointer sequence 后，返回以下两种结果之一：

```text
BusinessIntent
  Bevy 应执行的业务行为；不携带对 vars 的任意写入。

UiVarPatch
  UI Runtime 已裁决的 ui_vars 变化；不包含 vars 中的业务字段。
```

一个交互可以同时产生一个业务 intent 和一个 UI patch，例如点击物品格：UI Runtime 更新 grid
selected row，同时向 Bevy 发出 `inventory.select_item` intent。

```json
{
  "status": "accepted",
  "result": {
    "target": {
      "stable_row_key": "player.main.status",
      "panel_node_path": "character-status/panel",
      "node_path": "character-status/grid/item-42"
    },
    "ui_var_patch": [
      { "key": "grid.selected_row", "value": 42 }
    ],
    "intent": {
      "kind": "inventory.select_item",
      "item_id": "item.42"
    }
  }
}
```

Bevy 先验证 response 的 frame/session/epoch，再由 registry 查到 Entity；找不到 Entity 或对应实例
已经销毁时丢弃 response，并写入结构化诊断。

## 9. 防 pingpong 规则

pingpong 的根源是同一字段被 UI Runtime 和 Bevy 都当作可写状态。以下规则必须固定：

1. `vars` 中每个字段只能由 Bevy gameplay/ECS 修改。UI Runtime 不返回 `vars` patch。
2. `ui_vars` 中每个字段只能由 UI Runtime 修改。Bevy 不根据收到的 patch 再生成同字段的新值。
3. `BusinessIntent` 不等于变量 patch。intent 由 Bevy 执行后，Bevy 再通过 `vars` diff 发布结果。
4. `UiVarPatch` 不等于 gameplay event。Bevy 保存该 patch，但不把它解释为需要改变 health、装备或项目数据。
5. Bevy 发往 UI Runtime 的 input frame 必须声明 source：`bevy_state_sync` 或 `ui_state_restore`。
   UI Runtime 对来自自身已确认 patch 的镜像回送去重，不重新发布 UI patch。
6. 每个 mutation 带 request_id、idempotency_key、origin、causation_id、预期 revision 和 service epoch。
   重复的 causation_id 只能产生幂等响应，不能再执行一次业务动作。
7. 对浮点 UI 状态约定精度或 epsilon；否则序列化/反序列化的小数抖动会持续产生 diff。

理想闭环：

```text
业务按钮：
  ID 图 -> semantic intent -> Bevy gameplay -> vars diff -> UI Runtime -> render

UI 控件：
  ID 图 -> UI Runtime state transition -> ui_vars patch -> Bevy mirror -> render/restore
```

不存在：

```text
UI Runtime 任意改 vars -> Bevy 再改 vars -> UI Runtime 再回写 vars
```

## 10. 重启、失效与诊断

以下情况必须取消 capture/prediction、丢弃旧 response，并重新请求 snapshot：

- renderer epoch 变化。
- surface resize 或 generation 变化。
- Bevy consumer 已 release 对应 buffer，或该 buffer 的 `PickFrame` 已回收。
- composition/program/input revision 不匹配。
- `stable_row_key` 已不存在，或 registry target 映射已变更。
- pointer sequence 过期、重复或存在交互中的 sequence gap。

每个点击事务的 trace 至少包含：

```text
request_id
causation_id
pointer_id / sequence
UiFrameKey
pick result: no-hit / intercepted / scene-occluded
stable_row_key / node_path（脱敏后）
intent kind
ui_vars revision before/after
vars revision before/after
accepted / rejected 与稳定错误码
```

日志、event journal 和 Bevy gameplay event 中不记录 raw hit_id、原始 GPU handle、像素坐标，除非
明确属于受控 debug capture artifact。

## 11. 施工顺序

1. 定义 `NeonWorldUi<V, U>`、`NeonUiInstance`、`UiFrameKey`、`NeonUiTargetKey` 与 registry；为
   `V`、`U` 分别提供 typed snapshot/diff/patch trait。
2. 将 external surface 升级为正式三缓冲 color/depth/pickId，确保同一 buffer slot 的 fence 与
   frame sequence 一致。
3. 让 WGPU renderer 从同一个 resolved draw list 写出三目标，并实现 buffer-bound `PickFrame`。
4. 在 Bevy RenderApp 实现 queue wait、color/depth compositing、单像素 ID readback 与 consumer release。
5. 实现 `ui.host.inbound` 的 frame-key 校验和稳定 target response；raw hit ID 不跨进程。
6. 实现 Bevy registry O(1) Entity 查找、`UiVarPatch` 镜像与 `BusinessIntent` ECS event 分发。
7. 编写 headless scenario：screen UI 覆盖 World UI、被墙遮挡的 World UI 不可点击、grid patch 不触发
   gameplay 写入、业务按钮只经 Bevy 改 vars、旧帧 response 被拒绝。

## 12. 验收标准

1. 同一 `UiFrameKey` 的 color、depth、pickId 使用同一 buffer index、frame sequence 与 release 生命周期。
2. 重叠区域普通 UI 的 color 和 pickId 都覆盖 World UI。
3. `WorldDepthTested` UI 被 Bevy scene depth 遮挡时，既不可见也不可截获 pointer。
4. `hit_id` 解析到 `UiHitBinding` 为 O(1)，Bevy 稳定 target 解析到 Entity 为 O(1)。
5. Bevy 或 UI Runtime 任一方重启、resize 或 epoch 变化后，旧点击不会写入当前状态。
6. UI 专属 DataGrid/tab/scroll patch 只修改 `ui_vars`，不会触发 `vars` 的 gameplay mutation。
7. 业务 intent 只由 Bevy gameplay 修改 `vars`，UI Runtime 不返回业务字段 patch。
8. 同一 causation_id 的重试不会重复执行业务 mutation 或重复应用 UI patch。
