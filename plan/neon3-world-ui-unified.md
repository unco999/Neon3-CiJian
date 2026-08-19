# Neon3 World UI 统一渲染 + 泛型组件施工方案

> 目标读者：接手的其他 AI / 实现者。本文是**调研结论 + 施工设计**，不是已完成实现。
> 配套必读：`AGENTS.md`、`plan/neon3-host-engine-gpu-interop.md`、`plan/neon3-bevy-nui-flow-vars.md`、
> `plan/neon3-gpu-reactive-ui-contract.md`、`plan/neon3-nui-flow.md`。
>
> 2026-08 实测补记：本计划已经在 `D:\bevy-nui-host` 做过单实例验证。Flow 编译、world
> anchor/camera 投影、无窗口 Neon GPU owner、D3D12 shared texture 导出与 Bevy DX12 consumer
> import 均已验证。单缓冲 direct texture import 不可作为正式路径：producer/consumer 是不同的
> D3D12 device，旧原型只轮询 shared fence，缺少 queue wait、资源状态 ownership transfer 与
> consumer release，会导致 Bevy 场景闪烁、UI 残影或 render corruption。第 4.1 与阶段 2A 是
> 本次施工最高优先级。

---

## 0. 结论速览（TL;DR）

需求：场景有 1000 个 world UI，各自绑定一个 `.nui` 脚本与一组变量，每个实例来自不同
world position，要经摄像机正确映射、视锥外剔除、与 screen UI 合成进一张**有正确深度关系**
的最终纹理导出给外部引擎，并接入「鼠标点击 → 变量变化 → 自动回写 Bevy」的交互闭环。

调研结论：**底座大部分已存在**，本方案主要是「把已有能力升级为正式路径 + 补三个缺口」。

| 需求点 | 现状 | 结论 |
| --- | --- | --- |
| 泛型 world UI 组件绑定 `.nui` + 变量 | `NeonWorldUi<V>`、`NuiFlowVars::row_snapshot/apply_changes`、可见 row helper 已接入 host；Mei 已实际挂载 `NeonWorldUi<CharacterStatusVars>` | 仍需把多个组件聚合为正式 `ui.input.repeat` frame，并让 renderer 消费 repeat rows |
| 1000 实例合并渲染 | `world_ui_pipeline.rs` 已有深度测试 quad 管线 + `MAX_WORLD_UI_QUADS`（当前 lab 模式） | 升级为正式路径 |
| 每实例不同 world position | `WorldUiAnchor` + `project_world_point_to_screen*` 已存在 | 复用 |
| 每实例变量通信（难点） | `UiTemplateDeclaration` + `UiRepeatFrame`/`UiRepeatRow` 已存在（批量 per-instance 输入） | 复用，把实例映射为 template row |
| 视锥外剔除 | 投影函数已含 near/far/frustum 判断 | 复用，接上「剔除→不建 quad」 |
| world UI + screen UI 统一深度合成 | 两者目前是分开的 pass（screen UI 无深度） | 需合并为一张深度 pass 并导出 depth |
| 交互层纹理拾取 | ID target（r32uint）+ hit readback + `HostUiPointerClick` 已存在 | 补 Bevy 侧固定系统 + 变量回写协议 |

---

## 1. 现状调研

### 1.1 已存在、可直接复用的能力

**（A）深度测试的 world UI quad 管线 —— `crates/neon-wgpu-runtime/src/world_ui_pipeline.rs`（910 行）**

这是本方案最重要的底座，目前被标记为「内部 lab/未来模式」，但功能已完整并**有测试**：

- `WorldUiQuad { model: [[f32;4];4], color: [f32;4] }`：单位 quad 的世界列主序变换 + 颜色。
- `MAX_WORLD_UI_QUADS = 256`：quad 容量上限（1000 需求要提额或分批）。
- 深度测试状态：`Depth32Float` + `depth_write_enabled: true` + `depth_compare: Less`。
- `textured_pipeline`：采样一张 UI surface 纹理的 quad 管线（把 UI 画到 3D 世界）。
- 已有测试 `world_ui_depth_hides_behind_quad_and_keeps_front_quad_visible`：证明
  「被挡的 quad 隐藏、前面的 quad 可见」——**深度遮挡已经跑通**。
- 有 `surface_quad_buffer`（每实例 quad 数据），已经是实例化思路。

**（B）世界点投影 + 视锥/近远裁剪 —— `neon-wgpu-runtime/src/lib.rs`**

- `project_world_point_to_screen`（5745 行附近）、`project_world_point_to_screen_with_depth`（5766）、
  `project_world_anchor_to_screen`（6662）。
- 注释明确：投影会返回「相机后、near/far 外、视锥外」为失败/None —— **视锥剔除逻辑已经存在**，
  只是还没接到「剔除 → 跳过该实例的 quad 构建」。

**（C）批量 per-instance 输入机制 —— `crates/neon-ui-schema/src/lib.rs`**

```rust
pub struct UiTemplateDeclaration {
    pub template_key: String,
    pub root_node_key: String,
    pub max_instances: u32,
    pub row_schema: std::collections::BTreeMap<String, UiInputKind>,
    pub instance_key_field: String,
    pub overflow_summary: bool,
}
pub struct UiRepeatRow {
    pub stable_row_key: String,
    pub values: std::collections::BTreeMap<String, UiInputValue>,
    pub semantic_payload: std::collections::BTreeMap<String, String>,
}
pub struct UiRepeatFrame {
    pub template_key: String,
    pub list_revision: Revision,
    pub rows: Vec<UiRepeatRow>,
    pub expected_program_revision: UiProgramRevision,
}
```

这正是「一个 flow 模板 + N 个实例、每实例一组变量」的现成契约：一个 world UI 实例 = 一个
`UiRepeatRow`（`stable_row_key` = anchor id，`values` = 该实例变量）。**批量输入不用新造协议**。

**（D）输入与变量权威链**

- `UiInputFrame { program_revision, expected_input_revision, request_id, idempotency_key, changes }`。
- `UiInputChange { key, value }`，`UiInputValue` 枚举（Bool/I32/U32/F32/Vec2/Vec4/Color/Enum/TextHandle/AssetHandle）。
- `nui_flow_vars!` 宏生成「结构体 + `NuiFlowVars` trait（`snapshot`/`diff`）」。
- `emitevent` 变量 → eventd `flow.<flow>.<var>` 事件（观测用，非权威）。

**（E）交互底座**

- ID target `r32uint` + `gpu.ui.draw_hit_id` + hit readback（`try_complete_hit_readback`）。
- `HostUiPointerClick`、`ui.host.inbound`、语义事件（`UiSemanticEvent`）。
- 当前缺口（README 明确）：Bevy 侧 ID target 导入/readback、fence wait、点击自动提交。

**（F）外部 GPU 共享**

- `render.surface.open/acquire/frame`、D3D12 shared texture + shared fence、`DuplicateHandle`。
- 已实现 single buffer 原型；实测确认它只能用于诊断，正式路径必须是三缓冲、双向 fence 和 native state handoff。

### 1.2 关键缺口（本方案要补的）

1. **泛型组件的批量运行链尚未完整**：host 已有 `NeonWorldUi<V>` 和 typed row/writeback，但当前 Mei 案例仍保留旧 `CharacterStatusBridge` 标量 frame；需迁移为 repeat frame 并接 renderer 实例消费。
2. **没有「UI → Bevy 变量回写」方向**：现有变量只单向 `Bevy → UI`；点击导致的变量变化没有
   权威回流通道（eventd 观测不算权威）。
3. **world UI 与 screen UI 仍是两套 pass**，未合成一张带深度的统一纹理导出。
4. **lab quad 管线未接真实 world anchor + 真实 flow 变量**（只画 lab 面板）。
5. **1000 实例的 GPU 侧实例化 UI 绘制**（per-instance 变量驱动的 glyph/bar）尚未实现。
6. **跨 D3D12 device ownership transfer 未完成**：不能把 foreign `ID3D12Resource` 直接当作
   普通 `wgpu::Texture` 采样。`create_texture_from_hal` 的安全约束要求资源来自同一个 device；
   当前原型违反该约束，不能升级为正式实现。

---

## 2. 目标架构

### 2.1 数据流总览

```text
Bevy 世界 (外部宿主)
  ├─ 每个 world UI 实体 = NeonWorldUi<V> { flow, vars, anchor, identity }
  │     ├─ 每帧: diff vars → 批量 UiRepeatFrame(rows=N) ──┐
  │     ├─ 每帧: publish world anchor (entity transform) ─┤
  │     └─ 每帧: publish camera frame ────────────────────┤
  │                                                        ▼
  │                                              neon-ui-runtime (无窗口, 已内嵌)
  │                                                compile flow → UiProgram(含 template)
  │                                                apply batch input → resolved per-instance
  │                                                转发 fragment/template 数据
  │                                                        ▼
  │                                              neon-wgpu-runtime (唯一 GPU owner)
  │                                                cull(视锥) → build quads → 深度合成 pass
  │                                                world UI(带深度) + screen UI(置顶)
  │                                                → 一张 color + 一张 depth
  │                                                        ▼
  │                                               D3D12 shared texture/fence 导出
  │                                                        ▼
  └─ Bevy RenderApp 导入 color(+depth) 合成到自己的 3D 画面
       鼠标点击 → 采样 ID target → ui.host.inbound → 语义事件 → 变量变化
       → 回写 NeonWorldUi<V>.vars (权威回流)
```

### 2.2 进程职责（不越界）

- **neon-ui-runtime**：flow 编译、template 实例化、输入校验/resolved、语义事件路由。无 GPU。
- **neon-wgpu-runtime**：唯一 GPU owner，做视锥剔除、quad 构建、深度合成、共享纹理导出、hit readback。
- **bevy-nui-host**：拥有 `NeonWorldUi<V>` 组件与游戏状态，每帧提交变量 diff + anchor + camera，
  导入共享纹理合成，采样 ID target 发起交互，接收变量回写。

禁止：Bevy 直接改 UI runtime 内存、伪造 hit ID、把 Entity 当变量 key；UI runtime 把事件当游戏命令。

---

## 3. 核心设计

### 3.1 `NeonWorldUi<V>` 泛型组件 + `NuiFlowVars` 扩展

组件定义（`bevy-nui-host`）：

```rust
#[derive(Component)]
pub struct NeonWorldUi<V: NuiFlowVars> {
    /// 绑定的 .nui 流程（编译后得到含 template 的 UiProgram）。
    pub flow: NeonFlowBinding,          // 持 flow 名 + 已编译 program 缓存
    /// 该实例的实时变量值（即"组件内部字段 = 变量字段"）。
    pub vars: V,
    /// 每实例身份：program/input revision、请求序号。
    pub identity: NuiFlowIdentity,
    /// 世界锚点 id（稳定实例标识，等于 template 的 stable_row_key）。
    pub anchor: WorldAnchorId,
    /// 相对实体 Transform 的世界偏移（billboard 锚点）。
    pub offset: Vec3,
    /// 上一帧已提交的 vars 快照（用于 sparse diff）。
    pub last_sent: Option<V>,
    /// 视锥剔除/可见性状态（可观测，非权威）。
    pub visible: bool,
}
```

`NuiFlowVars` trait 需扩展一个**回写方向**（配合 3.5 交互闭环）：

```rust
pub trait NuiFlowVars: Clone + PartialEq {
    const FLOW_NAME: &'static str;
    const COMPONENT_NAME: &'static str;
    fn snapshot(&self, identity: &mut NuiFlowIdentity) -> UiRepeatRow;   // 改为产出 row
    fn diff(&self, previous: &Self, identity: &mut NuiFlowIdentity) -> Option<UiRepeatRow>;
    /// 新增：把权威回流的一批变量变化应用到本实例（宏生成按 key 匹配字段）。
    fn apply_changes(&mut self, changes: &[UiInputChange]) -> Result<(), NuiFlowVarError>;
    /// 新增：稳定实例 key（= anchor id，由组件注入或宏常量）。
    fn stable_row_key(&self, anchor: &WorldAnchorId) -> String;
}
```

宏 `nui_flow_vars!` 在现有 `snapshot`/`diff` 基础上，额外生成 `apply_changes`
（对 `bool/i32/u32/f32/String/enum` 做类型校验 + 字段赋值，未知 key 返回稳定错误）。

要点：

- `V` 的字段**就是**变量字段（`health/mana/level/...`），访问路径 `ui.vars.health`。
- 字段类型沿用受限集合：`bool/i32/u32/f32/String/enum/AssetRef/UiTextHandle`，禁止 `Vec<T>`/指针/Entity。
- 一个 flow 模板对应一个 `UiProgram`（含 `UiTemplateDeclaration { max_instances: N, instance_key_field }`）；
  1000 个 `NeonWorldUi<V>` 实体共享**同一个** program，只是各自占一个 row。

### 3.2 实例化变量输入（难点，但底座已备好）

Bevy 侧新增一个系统 `flush_world_ui_input`，把所有 `NeonWorldUi<V>` 的 diff 聚合为**一个**
`UiRepeatFrame`：

```rust
// 每个可见实例贡献一行（sparse：仅变化字段）
rows: Vec<UiRepeatRow> = world_uis
    .filter(|ui| ui.visible)
    .filter_map(|ui| ui.vars.diff(&ui.last_sent, &mut ui.identity))
    .collect();

UiRepeatFrame {
    template_key: "nameplate",           // flow 里声明的 template
    list_revision: next_revision,
    rows,
    expected_program_revision: program_revision,
}
```

提交方式：新增 RPC `ui.input.repeat`（或复用现有 repeat 通道），一次带 1000 行的批量更新，
而非 1000 次 RPC。`UiRepeatRow.stable_row_key = anchor.0`（实例稳定标识）。

为什么这样是对的：

- 复用 `UiTemplateDeclaration`（`max_instances`）声明容量，`row_schema` 校验每行字段类型。
- 复用现有 input revision / idempotency / 校验 / emitevent 链，**批量输入不引入新的一致性模型**。
- 宿主只提交**变化**的实例（diff 为 None 的实例不发），1000 个里大部分静止时开销趋近 0。

### 3.3 统一深度合成渲染（world UI + screen UI 一张纹理）

**目标**：把现有「world UI quad（带深度）」与「screen UI fullscreen（无深度）」合并进**同一个
render target + 同一个 depth buffer**，导出一张 color + 一张 depth。

合成顺序（单 command encoder）：

```text
Pass 0  清 depth（=1.0 最远）与 color（透明或底色）
Pass 1  world UI 实例（深度测试 quad）
          - 每个可见实例一个 WorldUiQuad(model=世界变换)
          - depth_write=true, depth_compare=Less  → 实例之间正确遮挡
Pass 2  screen UI（fullscreen overlay）
          - depth 清为 0（最近）或 depth_test=Always → 永远在最前
Pass 3  hit-ID pass（r32uint，与 color 同几何同深度）→ 供拾取
```

两个关键决策：

**（1）world UI 的「内容」如何画进 quad**（1000 个不同内容的 UI）

- 目标方案（GPU 实例化）：利用已存在的 `UiInputPacking`/`UiGpuScalarRepresentation`，把每个
  实例的 resolved 变量打包进一个 per-instance GPU buffer，UI 的 vertex/fragment shader 按
  packing 元数据读取 per-instance 变量，驱动 glyph 位置/进度条长度/文本。**一次 instanced
  draw 画全部 1000 个实例**。这是 `gpu-reactive-ui-contract` 已经铺垫的方向，但 GPU 侧工作量最大。
- 中间方案（atlas）：每个实例先把 UI 画到一张 atlas 的小区域（或小 offscreen），quad 采样对应
  region。O(N) 次小 pass，简单但 1000 个时性能差，只作 milestone 过渡。

建议：先按 atlas 跑通「统一深度合成 + 导出」的端到端链路，再替换为 GPU 实例化。

**（2）深度如何导出给外部引擎**

- 导出物：`color`（rgba8unorm）+ `depth`（d32float）+ 各自 shared fence。
- 沿用 `render.surface.open` 的 descriptor 扩展一个 `depth: true`（已有 `depth: bool` 字段）。
- 宿主侧合成选择：
  - **简单模式**：宿主直接 blit color（UI 整体盖在宿主画面上，UI 内部深度已正确）。
  - **与几何互遮挡模式**：宿主用自己的场景 depth，depth-test 采样 Neon 的 world UI 区域
    （world UI 被墙挡住会正确隐藏）。这需要宿主参与 depth 合成，属于第二阶段。

> 说明：Neon 不拥有宿主 3D 场景，所以「world UI 与 3D 几何互遮挡」只能靠宿主拿 Neon 的
> depth 做合成；Neon 能保证的是「world UI 之间」与「screen UI 置顶」的深度正确。

### 3.4 视锥剔除

复用 `project_world_anchor_to_screen`（已含相机后/near/far/视锥外判断）：

```text
每帧，对每个 anchor:
  projected = project_world_anchor_to_screen(anchor, camera_frame, viewport)
  若 None（视锥外/相机后/超 near-far）→ ui.visible = false → 跳过 quad 构建 + 跳过 row 提交
  否则 → ui.visible = true，产出 quad（model=世界变换）+ row（变量）
```

额外可做：按 anchor 到相机距离排序（透明 UI 正确混合）、远距离 LOD（距离过远合并/简化）。

### 3.4.1 已验证的单实例投影规则与修正项

`D:\bevy-nui-host` 原型已验证 Bevy 相机 frame 与 anchor 可以由 Neon 正确投影到 shared UI
viewport。实现者不得在 Bevy 侧重复投影；Bevy 只提交右手 Y-up / -Z forward 的 camera frame 和
world anchor，Neon 是唯一计算屏幕坐标的 owner。

施工时必须保留以下规则：

1. world panel 必须是透明 `surface` 根下的子节点，不能是 fragment root。renderer 会把 fragment
   root 强制布局为整个 viewport；world panel 若是 root，会被重排到左上或绘制为整屏。
2. Neon 完成 camera gate + anchor projection 后，不能再由 `UiWgpuRenderer` 用独立
   `available_cameras` 集合二次 gate。已消费的 `CameraVisibility` effect 必须在 renderer snapshot
   中移除，或 renderer 必须共享同一个 world bridge。
3. world panel 根和透明容器不能对投影区域使用 inherited bounds clip。world panel 可按自身需要 clip
   子内容，但父容器必须 `clip: none`，否则靠近 viewport 边缘时会被错误裁剪。
4. camera 与 anchor 不能作为两个独立渲染提交之间的中间帧。对同一个 frame identity，producer 只在
   成对 camera+anchor snapshot 已提交后生成 UI frame；否则会出现“新 camera + 旧 anchor”的横向漂移。
5. 近大远小使用 camera-space depth，不使用 Euclidean distance：

   ```text
   scale = clamp(reference_depth / camera_space_depth, min_scale, max_scale)
   ```

   scale 必须统一应用到 panel bounds、所有 descendant bounds、padding、margin、gap、glyph rect、glyph
   advance、baseline、line height、文字 inset 与 clip。只放大 panel 而不缩放 text 是错误实现。
6. 生产实现应为每个 projected world node 存储 `WorldUiPresentation { screen_center, depth, scale,
   visible, frame_sequence }`，而不是直接破坏 authoring node 的原始 bounds；layout 阶段消费这份
   renderer-local presentation 数据，便于调试和避免 revision cache 漏刷新。

### 3.5 交互拾取与变量回写（闭环）

Bevy 侧新增**固定系统** `neon_world_ui_interaction`（不随实例变化）：

```text
1. 读输入：鼠标按下/移动 → 得到屏幕像素 (x, y)。
2. 采样 ID target（r32uint 共享纹理，经 fence wait 后 readback 单像素）。
   命中 id != 0（有意义）才继续；id == 0 视为空白，直接返回。
3. 组包 HostUiPointerClick { generation, frame_sequence, id_target_id, pixel } →
   发给 ui-runtime（方法 `ui.host.inbound` 或专用 `ui.pointer.click`）。
4. ui-runtime 用 hit id 反查声明语义绑定 → 校验 visible/enabled/payload → 路由 domain。
5. domain 计算变量变化（如"攻击冷却=5s"），产生新的 resolved input revision。
6. ui-runtime 在响应里带回「权威变量变化」：
   { status: accepted, changed_variables: [UiInputChange], input_revision: N }
7. Bevy 系统 `apply_world_ui_signals` 按 anchor/stable_row_key 找到对应 NeonWorldUi<V>，
   调 vars.apply_changes(changed_variables) 自动改字段值。
8. 下一帧 diff 自然把新值反映到 UI（若值已一致，diff 为空，无重复写）。
```

关键点：

- 回写是**权威回流**（domain 决定的新值），与 eventd 的观测事件分开。
- 用 `origin`/`request_id`/`idempotency_key`/`input_revision` 防回环：Bevy 收到回写后，若值
  已是权威值，后续 diff 为空，不会震荡。
- 采样 ID 纹理走「fence wait → readback 单像素 → 提交 click」，不是每帧全量读回。

---

## 4. 协议与数据结构定义（建议新增/扩展）

### 4.1 `render.surface` 正式三缓冲协议（必须先完成）

**结论**：`buffer_count=1` 不可用于跨 D3D12 device 的连续显示。producer 可能清空/重绘一张
正在被 Bevy shader 采样的纹理；只读 `ID3D12Fence::GetCompletedValue()` 也不是 GPU queue 同步。
正式路径固定为三缓冲。两缓冲在 consumer 慢于 producer 时会频繁阻塞，三缓冲可吸收一帧延迟。

#### 4.1.1 Open / acquire contract

```json
{
  "surface_id": "surface.world.screen",
  "kind": "unified_ui",
  "size": { "width": 1280, "height": 720 },
  "format": "rgba8unorm",
  "depth": true,
  "depth_format": "d32float",
  "buffer_count": 3
}
```

`render.surface.acquire` 不再返回单一 `texture_handle`。返回一个 generation-bound ring；每个
buffer 有独立 color/id/depth resource、producer fence 与 consumer-release fence。handle 只能
通过本机 broker `DuplicateHandle` 传递，JSON 中的数字仅表示本机 duplicated HANDLE，不能进入
journal、event 或持久化数据。

```json
{
  "surface_id": "surface.world.screen",
  "generation": 1,
  "buffer_count": 3,
  "buffers": [
    {
      "buffer_index": 0,
      "color_texture_handle": 123,
      "id_texture_handle": 124,
      "depth_texture_handle": 125,
      "producer_fence_handle": 126,
      "consumer_fence_handle": 127,
      "initial_producer_value": 0,
      "initial_consumer_value": 0
    }
  ]
}
```

每个 response 必须带 adapter LUID、format、size、generation；Bevy consumer 必须在 import 前
验证 backend=DX12、LUID 一致、format/extent 一致。任一不符必须稳定拒绝，禁止 CPU readback 或
隐式 PNG fallback。

#### 4.1.2 每 buffer 的 ownership 状态机

```text
initial: COMMON, producer owns

Producer selects a free buffer i
  wait producer queue on consumer_fence[i] >= consumer_release[i]
  transition COMMON -> RENDER_TARGET
  render color / id / depth
  transition RENDER_TARGET -> COMMON
  signal producer_fence[i] = producer_ready[i]

Consumer selects newest producer-ready buffer i
  Bevy DX12 queue Wait(producer_fence[i], producer_ready[i])
  transition COMMON -> PIXEL_SHADER_RESOURCE
  sample only buffer i in the overlay pass
  transition PIXEL_SHADER_RESOURCE -> COMMON
  signal consumer_fence[i] = consumer_release[i]

Producer may reuse i only after the consumer release value is reached.
```

规则：

- `producer_ready[i]`、`consumer_release[i]` 分别单调递增，不能重用相同值或用 CPU 时间戳代替。
- Bevy 必须在**自己的 GPU queue** enqueue `Wait`；CPU `GetCompletedValue` 只可用于诊断/选择最新帧，
  不能替代 queue wait。
- 所有 native barrier 在 resource 所属 owner 的 command list 上执行。不能要求 wgpu 自动推断另一个
  D3D12 device 改过的 resource state。
- `wgpu::create_texture_from_hal` 不能作为跨 device ownership 的正式实现。若 wgpu 不能表达此
  ownership transfer，应改用完整 native D3D12 overlay path，或把 consumer 设计为同一 D3D12 device。
- ID/depth 与 color 采用相同 `buffer_index`、相同 ready/release 生命周期，禁止 color 帧 N 配 ID 帧 N-1。

#### 4.1.3 Bevy overlay 的实现约束

- 选择 `producer_ready` 最大且未被消费中的 buffer；若无 ready buffer，继续显示最近已 acquire 的完整帧，
  不清屏、不采样正在写的 buffer。
- 共享 UI overlay 放在 `Core3dSystems::PostProcess`，但 native wait/acquire/release 必须与该 pass
  的 command submission 顺序绑定；不能在任意 ECS Update 系统提前 signal release。
- 开发期开关 `NEON3_ENABLE_EXPERIMENTAL_DIRECT_UI` 只能保留作负例/诊断，默认关闭，不能作为验收路径。

### 4.2 批量实例输入

新增（或复用 repeat 通道）RPC：

```json
{
  "method": "ui.input.repeat",
  "params": {
    "template_key": "nameplate",
    "list_revision": 12,
    "rows": [
      { "stable_row_key": "npc.blacksmith", "values": { "health": 82.0, "name": "铁匠" } },
      { "stable_row_key": "npc.merchant", "values": { "health": 100.0 } }
    ],
    "expected_program_revision": { "program_id": "...", "revision": 3, "schema_version": 1, "capabilities": [] }
  }
}
```

### 4.3 变量回写（权威）

语义事件响应里带 `changed_variables`：

```json
{
  "status": "accepted",
  "request_id": "bevy-pointer-11",
  "result": {
    "changed_variables": [ { "key": "cooldown", "value": { "kind": "f32", "value": 5.0 } } ],
    "input_revision": 41
  }
}
```

Bevy 侧由 `apply_world_ui_signals` 消费并调 `vars.apply_changes`。

---

## 5. 分阶段施工计划

### 阶段 0：清理 + 组件骨架（bevy-nui-host）
- 定义 `NeonFlowBinding`、`NuiFlowIdentity`、`NeonWorldUi<V>`、扩展 `NuiFlowVars`（`apply_changes`/`stable_row_key`）。
- 宏 `nui_flow_vars!` 生成 `apply_changes`。
- 单元测试：`apply_changes` 类型匹配/未知 key 报错；`stable_row_key` 稳定。

### 阶段 1：批量输入打通（ui-runtime + schema）
- `ui.input.repeat` handler：复用 `UiTemplateDeclaration`/`UiRepeatFrame` 校验，批量 resolved。
- Bevy `flush_world_ui_input` 系统：聚合 N 个 `NeonWorldUi<V>` 的 sparse diff 成单个 `UiRepeatFrame`。
- 验收：1000 个实例、只改 1 个时只提交 1 行、类型错误被拒、emitevent 仍发布。

### 阶段 2A：三缓冲 D3D12 interop 正确性（wgpu-runtime + bevy host）
- 用第 4.1 的三缓冲 acquire response 替换 legacy single-handle response；旧字段在同一 generation
  内不得静默兼容，开发期开关/明确 versioned capability 可以保留诊断路径。
- Neon producer 为每个 ring buffer 实现 `consumer release wait -> COMMON/RENDER_TARGET acquire ->
  render -> RENDER_TARGET/COMMON release -> producer ready signal`。
- Bevy consumer 为每个 ring buffer 实现 `producer ready queue wait -> acquire -> sample -> release ->
  consumer signal`，并只显示最新完整帧。
- 记录可机器查询的诊断：generation、buffer index、producer ready、consumer release、selected frame、
  dropped frame、wait duration、state transition failure、adapter LUID。
- 验收：连续动画场景下 Bevy 几何、动画与外置 UI 不闪烁；关闭/开启 UI overlay 不改变宿主 scene 的
  checksum；强制 consumer 落后一帧时仍显示最近完整 UI 帧；resize/device lost 后旧 generation 不再采样。

### 阶段 2B：统一深度合成 + 导出（wgpu-runtime）
- 把 `world_ui_pipeline` 从 lab 模式接正式 world anchor + 真实 flow 变量（先 atlas，后实例化）。
- 合并 pass：depth 清 → world UI（深度测试 quad）→ screen UI（置顶）→ hit-ID。
- `render.surface.open` 加 `depth: true`，导出 color + depth 两张共享纹理。
- 验收：world UI 之间正确遮挡、screen UI 置顶、被墙挡（宿主合成）正确隐藏。

### 阶段 3：交互闭环（bevy-nui-host + ui-runtime）
- Bevy `neon_world_ui_interaction` 固定系统：采样 ID target → 提交 click。
- ui-runtime 语义事件响应带 `changed_variables`。
- Bevy `apply_world_ui_signals` 系统：回写 `NeonWorldUi<V>.vars`。
- 验收：点击按钮 → 变量变 → Bevy 字段自动变 → 下一帧 UI 更新，且无回环。

### 阶段 4：性能与容量
- `MAX_WORLD_UI_QUADS` 提额/分批、三缓冲调优、GPU 实例化 UI、距离 LOD、剔除批量化。
- 验收：1000 实例帧率达标、静止时 CPU/GPU 开销趋近 0。

---

## 6. 风险与待定项

1. **GPU 侧 1000 实例 UI 内容绘制**（glyph/bar 由 per-instance 变量驱动）是最大工作量，需在
   `UiInputPacking` 基础上做实例化 shader。先用 atlas 过渡，避免卡在 GPU 上。
2. **「world UI 与 3D 几何互遮挡」依赖宿主 depth 合成**，Neon 只能导出 depth、不能替宿主做；
   要明确这是宿主适配器的职责边界。
3. **`MAX_WORLD_UI_QUADS = 256`** 需提额；注意 batch 上限与 `UiResourceBudget` 一致。
4. **变量回写与 Bevy 权威的语义**：明确「domain 是哪些变量的权威」，否则回写与 Bevy 游戏逻辑
   可能冲突（例如血量由 Bevy 扣、冷却由 domain 管）。
5. **回环防护**：`apply_changes` 后 diff 需为空（值已一致），依赖 `PartialEq` 精确比较，浮点
   变量要约定容差。
6. 交互采样 ID 纹理的 fence wait 与单像素 readback 延迟，需评估对点击响应的影响。
7. **不要使用 raw-window-handle 解决 texture interop**：它仅用于传递 `HWND`/display handle 给
   wgpu 创建 window surface；无窗口 D3D12 shared texture 的同步、queue wait、fence 和 state
   ownership 不由它处理。
8. **Tokyo/高频材质场景是 interop 回归 fixture**：实测单缓冲 direct import 会在复杂动画场景里
   造成 UI 残影和宿主 scene 闪烁。该 fixture 必须成为三缓冲 GPU 验收的一部分，不能只在空场景或
   静态胶囊案例中验收。
9. 若 producer 和 consumer 必须维持独立 D3D12 device，原生 D3D12 path 是正式实现边界；不能以
   `wgpu-hal` private API 的“能显示一帧”作为支持跨 device resource import 的证明。

---

## 7. 验收标准（给实现者的完成判据）

1. `NeonWorldUi<V>` 可绑定任意 `.nui` 与任意 `V: NuiFlowVars`，字段即变量。
2. 1000 实例只发一个批量输入帧；静止实例零开销；类型错误被稳定拒绝。
3. world UI 经摄像机正确投影，视锥外实例完全不参与渲染与提交。
4. world UI + screen UI 合成一张带深度的纹理；world UI 相互遮挡正确，screen UI 置顶。
5. 导出的 color + depth 可被外部引擎按三缓冲、双向 fence、queue wait 和 native state ownership
   正确采样，且不会改变宿主场景的像素/动画稳定性。
6. 鼠标点击有意义的 ID → 语义事件 → 变量变化 → Bevy 字段自动回写 → UI 更新，无回环。
7. 全链路 `cargo test` 通过；`AGENTS.md` 渲染边界条款同步更新（若引入新的共享 texture 形态）。
8. 在持续动画的 Tokyo fixture 中，外置 UI 开启与关闭的宿主 scene capture 除 UI 覆盖区域外
   checksum 一致；无闪烁、无 UI 残影、无 D3D12 device removed / validation error。
