# Bevy 宿主 NUI Flow 变量桥接设计

## 1. 目标

Bevy 宿主的 ECS 状态不能通过散落的 `serde_json::json!` 手工拼装成 UI 输入。宿主应使用
声明式映射，把一个状态结构体声明为某个 NUI Flow 的变量源:

```text
Bevy component/resource
  -> nui_flow_vars declaration
  -> typed UiInputFrame
  -> neon-ui-runtime
  -> UiProgram / UiFragment revision
  -> neon-wgpu-runtime render
```

UI Runtime 仍然是 UI 变量和程序 revision 的权威方。Bevy 只拥有游戏状态，并提供经过声明
和类型验证的外部输入值。

## 2. 核心边界

```text
Bevy ECS                         neon-ui-runtime                 neon-eventd
gameplay state                   UI schema/input authority       event fan-out
health/mana/name/equipment  ->   validate/apply UiInputFrame  ->  flow.* variable event
       ^                                  |                             |
       |                                  v                             v
       +----------- semantic intent result / accepted revision --------+
```

禁止:

- Bevy 直接修改 UI Runtime 内部变量缓存。
- Bevy 读取或伪造 UI renderer hit ID。
- Bevy 把 `Entity` 数值当成 NUI variable key。
- UI Runtime 把变量事件当成游戏业务命令。
- eventd 保存 ECS 指针、GPU handle 或 Bevy World。

允许:

- Bevy 结构体字段通过宏声明为 NUI Flow variable。
- 变更检测生成可靠、revisioned 的 `UiInputFrame`。
- UI Runtime 校验类型、program revision、input revision 和幂等键。
- eventd 广播 `flow.<flow>.<variable>` 观测事件。
- Bevy 订阅事件，更新本地诊断/同步状态，但不把观测事件当权威业务输入。

## 3. 声明宏

首个 API 使用声明式宏，避免过程宏与 Bevy 版本绑定:

```rust
nui_flow_vars! {
    CharacterStatusVars => {
        flow: "character-status",
        component: "character.player.main.status",
        fields: {
            health: f32 => "health",
            mana: f32 => "mana",
            level: u32 => "level",
            name: String => "name",
        }
    }
}
```

宏生成:

```rust
pub struct CharacterStatusVars {
    pub health: f32,
    pub mana: f32,
    pub level: u32,
    pub name: String,
}

impl NuiFlowVars for CharacterStatusVars {
    const FLOW_NAME: &'static str = "character-status";
    const COMPONENT_NAME: &'static str = "character.player.main.status";

    fn snapshot(&self, identity: &NuiFlowIdentity) -> UiInputFrame;
    fn diff(&self, previous: &Self, identity: &NuiFlowIdentity) -> Option<UiInputFrame>;
}
```

字段类型只允许协议已有的有限类型:

```text
bool
i32
u32
f32
String
enum/string key
AssetRef
UiTextHandle
```

`Vec<T>`、任意 struct、指针、Entity、Texture、GPU handle 不允许直接作为变量。列表和复杂
结构必须使用稳定的 bounded DataGrid/window contract 或显式多个变量。

## 4. 变更语义

每个声明实例拥有一个 `NuiFlowIdentity`:

```rust
pub struct NuiFlowIdentity {
    pub session_id: String,
    pub program_revision: UiProgramRevision,
    pub expected_input_revision: Revision,
    pub request_sequence: u64,
    pub renderer_epoch: u64,
}
```

首次绑定发送完整 snapshot:

```text
UiInputFrame { all declared fields }
```

后续只发送变化字段:

```text
UiInputFrame { changed fields only }
```

每个 frame 必须有:

- `request_id`。
- `idempotency_key`。
- `program_revision`。
- `expected_input_revision`。
- 宿主单调 `request_sequence`。

如果 UI Runtime 返回新的 accepted input revision，宿主更新自己的 `expected_input_revision`。
如果返回 `stale` 或 `revision_conflict`，宿主必须重新获取 UI snapshot，再重新生成完整 frame，
不能盲目重放旧 diff。

## 5. 类型映射

```text
Rust bool    -> UiInputValue::Bool
Rust i32     -> UiInputValue::I32
Rust u32     -> UiInputValue::U32
Rust f32     -> UiInputValue::F32
Rust String  -> UiInputValue::TextHandle 或受控 text bridge
```

动态文本不能无限制地把任意字符串塞进每次 event。正式实现应使用 UI Runtime 的 text registry。
在第一个简单案例中，`String` 通过受限 text value adapter 进入输入帧；当 text registry client
稳定后切换为 `UiTextHandle`，宏 API 不变。

## 6. 交互回流

用户点击路径:

```text
Bevy color/id target pointer sample
  -> HostUiPointerClick { generation, frame_sequence, id_target_id, pixel }
  -> UI Runtime resolves ID target against declared semantic binding
  -> UiProgramSemanticEvent validation
  -> UI Runtime / domain host response
  -> Bevy adapter receives accepted/rejected semantic result
  -> Bevy ECS event or typed gameplay command
```

Bevy 不根据 ID 图整数自行猜 action。ID 图只用于把屏幕采样交给 Neon 的 renderer/UI authority。

## 7. eventd 变量事件

NUI Flow 中声明:

```text
flow character-status
input health f32:0..100 default 82 emitevent
```

UI Runtime 应用来自 Bevy 的 `health` frame 后，若值发生变化，向 eventd 发布:

```text
flow.character-status.health
```

事件是观测通知:

```json
{
  "module": "character-status",
  "surface": "character.player.main.status",
  "variable_key": "health",
  "kind": "f32",
  "old_value": 82.0,
  "new_value": 76.0,
  "origin": "bevy-nui-host"
}
```

Bevy 可以订阅该事件用于:

- 记录 UI 接受了哪个 revision。
- 调试 UI/游戏状态是否同步。
- 驱动非权威的 UI bridge 状态。

Bevy 不应收到事件后再次把同值写回游戏状态，否则会形成回环。回环防护使用:

- `origin`。
- `request_id`。
- `idempotency_key`。
- UI input revision。

## 8. 头顶角色状态案例

Bevy 案例拥有一个 `player.main` entity:

```text
SceneRoot(character.glb)
Neon3HostObject { object_id: "player.main" }
Neon3WorldUi { surface_id: "character.player.main.status" }
CharacterStatusVars { health, mana, level, name }
```

角色移动属于 Bevy ECS。角色头顶 UI 属于 Neon3 NUI:

```text
avatar + name + level
health bar
mana bar
status effect
```

Bevy 每帧只发布字段变化和摄像机 snapshot。World UI 的 transform/anchor metadata 由宿主
提供，surface texture 由 Neon 渲染，Bevy 负责在自己的 3D world 中消费 surface。

## 9. 验收标准

1. 宏声明可以生成完整 snapshot。
2. 相同结构体值不会生成重复 frame。
3. 只变化 `health` 时 frame 只包含 `health`。
4. frame 带 program revision、expected input revision、request ID 和幂等键。
5. UI Runtime 接受 frame 后产生新的 input revision。
6. `emitevent` 变量向 eventd 发布 `flow.character-status.health`。
7. Bevy 不把 eventd 观测事件回写成业务 mutation。
8. glTF 角色移动不影响 NUI variable authority。
9. camera revision 单调增加，stale camera frame 被拒绝或丢弃。
10. color target 和 ID target 尺寸、generation、frame sequence 一致。

## 10. 当前实现进度（2026-08-18）

已实现:

- `nui_flow_vars!` 声明宏，生成结构体、完整 snapshot 和 sparse diff。
- 支持严格映射的 `f32`、`u32`、`i32`、`bool` 变量类型。
- Bevy 案例 `CharacterStatusVars` 启动时生成完整 frame，后续只发送变化字段。
- `neon-ui-runtime` 新增独立 `ui.input.frame` RPC，使用 `UiHostAdapter::apply_external_input`。
- 外部输入仍走现有 input revision、idempotency、schema 校验和 `emitevent` publisher。
- Bevy 案例的 surface/camera 请求走 WGPU endpoint，变量/semantic intent 走 UI endpoint。
- Bevy adapter 已加入可选 eventd `flow.` 前缀订阅，将变量观测事件放入 `Neon3VariableEvents`
  Resource；不会自动把观测事件回写 ECS。
- WGPU Runtime 已按 targets 创建 color `rgba8unorm` 和 ID `r32uint` shared resources，
  两者分别完成 UI color draw / hit-ID draw，并返回两组 duplicated handles。
- Bevy `RenderApp` 已注册 `Neon3ExternalSurfaceGpu` RenderWorld resource 和 Render schedule
  consumer 入口，下一步在其 Windows 专用层完成 native handle import/fullscreen draw。

尚未实现:

- Bevy eventd 长连接订阅和 `flow.*` 事件回流 Resource。
- 动态 `String` 变量的 text registry 上传；当前案例的名字是静态 NUI 文本。
- Neon WGPU Runtime 的第二张真实 `r32uint` shared ID texture 分配和绘制。
- Bevy RenderGraph 对 color/id 两张 native texture 的实际导入、fence wait 和采样。
