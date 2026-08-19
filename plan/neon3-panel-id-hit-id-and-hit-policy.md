# Neon3 PanelId 与 HitId 统一设计方案

> 状态：施工计划，尚未实施。
>
> 目标：让 Panel 的 renderer-local `PanelId` 与 GPU `HitId` 使用同一个稠密整数索引；CPU
> 可以直接通过数组访问对应 Panel 记录，GPU 可以直接把同一个整数写入 `r32uint` ID 图。
> 同时在 NUI Flow 语法中增加最小可用的 `hit: true/false` 属性，先解决节点是否参与命中和
> 是否阻挡后方 UI 的问题。

## 1. 当前问题

当前 renderer 有三套不一致的身份：

```text
PlannedNode.instance_index
  -> Panel/Rect GPU instance buffer 的位置

refresh_hit_bindings() 分配的 hit_id
  -> 从 1 开始重新编号的交互节点 ID

hit_bindings: HashMap<u32, UiHitBinding>
  -> CPU 通过 HashMap 反查语义绑定
```

因此当前没有严格保证：

```text
PanelId == HitId
```

具体问题：

- 不可交互 Panel 没有 HitId。
- 可交互 node 的 HitId 与 Panel instance index 无关。
- HitId 每次刷新 binding 都可能重新编号。
- CPU 命中后需要 `HashMap` 查找。
- GPU ID 图只能表示交互 node，不能直接索引对应 Panel、文字、图片和语义记录。
- 同一 Panel 的背景、文字和 hit binding 没有统一的记录来源。

## 2. 核心结论

建立唯一的 renderer-local `PanelRecord` 表。所有可绘制 Panel、控件、文字、图片、Surface 和
交互 binding 都从这张表获得同一个 `PanelId`：

```rust
pub type PanelId = u32;
pub type HitId = PanelId;

pub const PANEL_ID_NONE: PanelId = 0;

pub struct PanelRecord {
    pub panel_id: PanelId,
    pub node_path: String,
    pub fragment: UiFragmentRevision,

    pub panel_instance: Option<UiInstance>,
    pub text_range: Range<u32>,
    pub image_range: Range<u32>,
    pub surface_range: Range<u32>,

    pub interaction: Option<UiHitBinding>,
    pub hit_policy: HitPolicy,
    pub visible: bool,
    pub enabled: bool,
    pub paint_group_id: PaintGroupId,
    pub occlusion_depth: f32,
}

pub enum HitPolicy {
    /// 当前 Panel 命中后阻止继续检查后方 UI。
    Blocking,
    /// 当前 Panel 可被观察/命中，但命中处理后允许继续检查后方 UI。
    Passthrough,
}
```

ID 图中写入的整数必须直接来自 `PanelRecord.panel_id`：

```text
GPU:
  pixel -> r32uint PanelId

CPU:
  panels[PanelId as usize] -> PanelRecord
```

不使用 `HashMap<u32, UiHitBinding>` 作为命中主路径。

## 3. 稳定性的定义

需要区分两种 ID：

### 3.1 稳定语义身份

跨帧、UI Runtime 重建和 Bevy 重启恢复使用：

```text
fragment_id / stable_row_key / node_path
```

它用于日志、语义事件、Bevy Entity registry 和 UI state 恢复，不能直接作为 GPU `r32uint`，
因为字符串或 64 位 hash 不是高效的 target index。

### 3.2 当前 renderer generation 的 PanelId/HitId

`PanelId` 是当前 renderer epoch + surface generation 内的稠密数组索引：

```text
0       no-hit
1..N    PanelRecord 数组索引
```

在同一个 composition snapshot 内必须稳定。UI 结构、可见性、surface generation 或 renderer
epoch 变化时，可以重新分配，但必须同步更新：

```text
color frame
ID frame
PanelRecord table
PickFrame key
```

因此“稳定 PanelId”准确含义是：**同一 frame key 下 CPU/GPU 严格一致，且在该 frame buffer
生命周期内不可改变**。跨 frame 的语义稳定性由 `node_path + stable_row_key` 提供。

## 4. PanelRecord 构建

在 layout/flatten 完成后，统一建立 Panel table：

```text
1. 遍历当前可见 UI tree。
2. 为每个可绘制或可命中的节点分配 PanelId。
3. PanelId 从 1 连续递增。
4. 创建 PanelRecord。
5. 将 Panel、Text、Image、Surface 的数据挂到同一 record。
6. 记录 paint_group_id、paint_order、depth 和 hit_policy。
7. 用同一 table 生成 color draw items、depth items 和 ID draw items。
```

不应再让以下路径各自生成 ID：

```text
self.instances 单独编号
refresh_hit_bindings() 单独编号
draw_hit_id() 再次编号
```

它们都必须消费同一组 `PanelRecord`。

## 5. GPU 数据布局

Panel color instance 和 hit instance 都携带同一个 `panel_id`：

```rust
#[repr(C)]
struct UiPanelInstance {
    rect: [f32; 4],
    fill: [f32; 4],
    border: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
    depth: f32,
    panel_id: u32,
}

#[repr(C)]
struct UiHitInstance {
    rect: [f32; 4],
    params: [f32; 4],
    panel_id: u32,
    hit_enabled: u32,
    clip: [f32; 4],
}
```

第一阶段不要求 GPU shader 处理复杂穿透链，只要求：

```text
hit_enabled = 0 -> 输出 PANEL_ID_NONE 或 discard
hit_enabled = 1 -> 输出 panel_id
```

ID shader 不得重新生成编号。`panel_id` 必须来自 CPU 创建的 `PanelRecord`。

## 6. NUI Flow 语法

第一阶段增加节点级布尔属性：

```text
button attack {
    hit: true;
    intent: character.attack;
}

panel decoration {
    hit: false;
}
```

建议默认值：

```text
可声明交互语义的控件：hit = true
普通 Label、Image、装饰 Panel：hit = false
```

为了避免默认值导致大面积 UI 拦截，也可以采用更严格的默认：

```text
所有节点默认 hit = false
只有声明 intent、bind、input 或显式 hit:true 才参与命中
```

最终选择应在 parser/schema contract test 中固定，不能由 renderer 猜测。

协议结构：

```rust
pub struct UiHitPolicy {
    pub enabled: bool,
    pub passthrough: bool,
}
```

但第一版语法只暴露一个 `hit` 布尔值即可。建议语义为：

```text
hit: false
  不参与 ID 图，不产生 PanelId 命中结果。

hit: true
  参与 ID 图，并默认阻挡后方 UI。
```

后续再增加：

```text
hit: passthrough
```

或：

```text
hit: true;
hit_policy: passthrough;
```

## 7. 可穿透与不可穿透

### 7.1 第一阶段：严格 hit true/false

第一阶段只实现两种行为：

```text
hit: false
  当前节点不写 ID。
  鼠标会继续命中后方可交互 Panel。

hit: true
  当前节点写自己的 PanelId。
  当前 pixel 的最终 ID 是该 PanelId。
  后方 Panel 的 ID 被覆盖，因此自然不可穿透。
```

这已经可以覆盖大部分需求：

- 装饰背景不拦截点击。
- Button、Grid cell、Input 拦截点击。
- 普通 Panel 通过 `hit:false` 允许点击穿透到子节点或后方内容。

### 7.2 后续：显式 passthrough

如果需要“当前 Panel 自己可以收到观察事件，但点击还要继续给后方 Panel”，再增加：

```text
hit: true;
hit_policy: passthrough;
```

这不是简单的单个 `r32uint` 能完整表达的，因为一个像素只能存一个 ID。后续有三种方案：

1. 只在 CPU/语义层根据 draw stack 继续查询后方候选。
2. 增加第二张或多层 ID target。
3. 使用 per-pixel linked list 或其他复杂 GPU picking 结构。

第一阶段不实施这些复杂 shader 方案。先确保 `hit:true/false` 与 PanelId/HitId 严格一致。

## 8. ID 绘制顺序

ID pass 必须复用颜色 pass 的 Panel group 顺序：

```text
远 World group -> 近 World group -> Screen -> Popup -> Modal -> Tooltip
```

组内顺序：

```text
Panel background -> image/surface -> text/control hit shape -> child subtree
```

命中结果必须和最终视觉最上层一致：

```text
最终显示在最上面的 hit:true Panel
  ==
ID 图像素中的 PanelId
```

`hit:false` 节点不应覆盖已经存在的 ID。对于一个 Panel 的文字：

- 文字本身通常不单独分配 PanelId。
- 文字继承所属交互 Panel 的 PanelId，或使用所属控件的命中矩形。
- 文字的 glyph coverage 不应产生独立的 Panel ID 碎片。

## 9. CPU 命中路径

目标路径：

```rust
let panel_id = readback_value;
if panel_id == PANEL_ID_NONE {
    return PointerOutcome::NoHit;
}

let Some(panel) = pick_frame.panels.get(panel_id as usize) else {
    return PointerOutcome::StaleOrInvalidId;
};

if panel.hit_policy == HitPolicy::Passthrough {
    // 第一阶段不进入此分支；后续定义多层命中策略。
}

resolve(panel.interaction.as_ref());
```

这里不允许：

```rust
HashMap::get(&panel_id)
遍历所有 Panel
根据 node_path 再次扫描 UI tree
把 panel_id 直接当成 Bevy Entity
```

必须绑定当前 `PickFrame`：

```text
PanelId 只对对应 generation/frame_sequence 有效。
```

## 10. Bevy 交互边界

PanelId/HitId 不跨进程作为业务 ID 传播。WGPU Runtime 解析后返回稳定语义目标：

```text
stable_row_key
panel_node_path
node_path
semantic intent
```

Bevy 通过自己的 O(1) registry 查找 Entity：

```text
stable UI target -> Entity
```

完整链路：

```text
Bevy pointer
  -> 采样统一 ID target
  -> PanelId == HitId
  -> WGPU/Neon pick_frame.panels[PanelId]
  -> UiHitBinding / semantic intent
  -> UI Runtime 校验
  -> Bevy 收到稳定 target 或业务 intent
  -> Bevy registry 查 Entity
```

PanelId 不进入业务变量、project 文件、event journal 或 Bevy gameplay command。

## 11. 施工步骤

### 阶段 1：协议与 Flow parser

- 在 `neon-ui-schema` 增加 `UiHitPolicy` 或等价的 `hit_enabled` 字段。
- 在 NUI Flow 语法增加 `hit: true/false`。
- 固定默认值和错误码。
- 增加 parser/schema round-trip tests。

### 阶段 2：统一 PanelRecord

- 定义 `PanelId = u32` 与 `PANEL_ID_NONE = 0`。
- 定义 `PanelRecord`、`PickFrame`。
- 用单一 builder 为每个可见 Panel/control 分配连续 PanelId。
- 删除 `refresh_hit_bindings()` 内部独立递增 HitId 的逻辑。
- 先保留旧 HashMap 作为诊断兼容，不再作为正式查找路径。

### 阶段 3：GPU ID target

- `UiHitInstance.panel_id` 直接来自 `PanelRecord.panel_id`。
- shader 只输出传入的 panel_id，不在 GPU 生成 ID。
- `hit:false` 节点 discard 或不加入 hit instance。
- 使用同一 draw ordering 生成 ID pass。

### 阶段 4：CPU O(1) 解析

- `PickFrame.panels` 使用 `Vec<PanelRecord>`，index 0 为 no-hit。
- 检查 `panel_id < panels.len()`。
- 检查 frame generation、buffer index、frame sequence 和 renderer epoch。
- 删除交互主路径中的 HashMap 查询。

### 阶段 5：Bevy 接入

- Bevy 只接收稳定 target，不接收 raw PanelId 作为业务字段。
- 继续使用 `stable_row_key + node_path -> Entity` 的 registry。
- 旧 frame 的 PanelId 必须拒绝。

## 12. 验收标准

1. 一个 frame 内所有 GPU 写入的 HitId 都能直接索引 `PickFrame.panels[HitId]`。
2. Panel color instance、text group、image/surface group 和 hit binding 来自同一个 PanelRecord。
3. `hit:false` 节点不会写入有效 HitId，也不会阻挡后方 Panel。
4. `hit:true` 节点在重叠区域输出最终视觉最上层的 PanelId。
5. CPU 命中解析不使用 HashMap、不扫描所有 Panel、不扫描 UI tree。
6. PanelId=0 永远表示 no-hit，合法 PanelId 从 1 连续分配。
7. PanelId 在对应 frame key 生命周期内不变；generation/epoch 变化后旧 ID 必须拒绝。
8. Bevy 收到的是稳定 UI target 或 semantic intent，而不是把 PanelId 当 Entity 或业务 ID。
9. Flow parser 能正确解析 `hit:true` 和 `hit:false`，并有默认值测试。
10. 连续动画和 UI 重建场景中，color、ID、PanelRecord 三者 frame sequence 严格一致。
