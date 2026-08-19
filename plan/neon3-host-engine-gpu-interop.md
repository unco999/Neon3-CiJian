# Neon3 宿主与外部引擎 GPU 互操作设计

> 状态:架构方向已定，首个实现目标为 Windows D3D12 + Godot D3D12；实现前必须同步更新 `AGENTS.md` 的渲染边界条款。
> 日期:2026-08-18

## 1. 目标

Neon3 不是只服务某一个游戏引擎。Godot、Unity、Bevy、Unreal 或自研引擎都应通过同一套
协议接入 Neon3，让 Neon3 负责普通 UI 与 World UI 的声明、资源、布局、GPU 绘制和测试验收。

外部宿主负责提供:

- 自己的 GPU 后端偏好与能力。
- UI Flow / UiProgram / UiFragment。
- 图片、字体、材质等输入资源。
- World UI 的锚点、变换、相机和场景上下文。
- 交互意图与测试场景。

Neon3 负责:

- 匹配宿主后端与 GPU adapter。
- 创建和管理可共享的 GPU surface/texture/fence。
- 绘制普通 UI 与 World UI。
- 将 UI surface 合成到屏幕或 3D 世界。
- 通过 eventd 发布生命周期、帧同步和诊断事件。

首要要求:

> 宿主后端必须匹配成功后才能建立 GPU 共享会话；不接受“先启动、后猜后端”的隐式行为。

这是一项对现有进程模型的有意扩展。当前 `AGENTS.md` 的基础规则把
`neon-wgpu-runtime` 定义为唯一 GPU owner，并禁止跨进程共享纹理。今天确定的新方向仍然
保留“只有 `neon-wgpu-runtime` 是 GPU resource owner”的原则，但允许它向经过后端匹配的
外部宿主导出受控的只读/采样型共享 surface。外部宿主不是第二个 Neon GPU owner，只是
经过授权的 consumer。

在实现代码前，必须把这条例外正式写回 `AGENTS.md`，否则实现与仓库架构约定会互相矛盾。

## 2. 架构边界

```text
External Engine / Host
        |
        | neon3.rpc + neon3.event
        v
   neon-eventd
   session / capability / lifecycle / sequence
        |
        | validated GPU session command
        v
 neon-wgpu-runtime
 backend / adapter / texture / fence / composition
        |
        +--> screen UI target
        +--> world UI target
        +--> capture / diagnostics
```

`neon-eventd` 是事件与会话协调中心，但不是 GPU owner。

只有 `neon-wgpu-runtime` 可以创建、持有或销毁:

- `wgpu::Device`、`wgpu::Queue`。
- `wgpu::Texture`、`TextureView`、Sampler、BindGroup。
- 原生 D3D12/Vulkan/Metal texture 与 synchronization object。
- 最终 window surface、offscreen render target 和 render graph。

eventd 不保存 GPU handle、不传输纹理对象、不执行 GPU 命令。跨进程的原生 handle 通过本机
handle broker 按 session token 领取，eventd 只发布 token 和状态。

### 2.1 Owner 与 Consumer

这里的“共享纹理”不代表把 Neon 的 GPU 所有权拆给外部引擎:

```text
Neon WGPU Runtime
  owns resource creation, resize, generation, state transition, release

Host Engine Adapter
  owns a duplicated consumer handle and a view/material binding
  may wait and sample
  may not resize, destroy, or reinterpret the Neon surface
```

宿主的渲染设备可以继续渲染自己的场景，但 Neon surface 的生命周期和内容权威仍在
`neon-wgpu-runtime`。任何需要宿主写入的方向都必须另行定义 producer surface，不得把
当前 consumer surface 默认变成双向共享。

## 3. 已有能力盘点

当前仓库已经具备重要基础:

### 3.1 WGPU Runtime

已有 capability:

```text
wgpu.ui.render_surface.v1
```

已有内部能力:

```text
register_render_surface
ensure_render_surface
ensure_ui_render_surface
resident_render_surfaces
wgpu.render.target.capture
```

`world_ui_pipeline.rs` 已能把 UI texture 作为纹理面片投影到 3D 世界并参与 depth test，但
它只作为内部 lab/未来模式。外部宿主的正式 World UI 路径是：宿主提交 world anchor
（`wgpu.world.ui.anchor.submit`），Neon 用 world info + camera frame 投影到屏幕坐标，在
自己的 fullscreen composition 里渲染，而不是把画布挂到宿主 Entity 上。

### 3.2 UI IR

`neon-ui-schema` / `neon-ui-runtime` 已有 `render_surface` 组件和 `UiFragment`、`UiProgram`。
外部引擎接入应优先发送声明和资源引用，而不是发送引擎私有控件 ID。

### 3.3 eventd

eventd 已有:

- namespace/name registry。
- publish/subscribe/ack。
- epoch + global sequence。
- replay ring。
- idempotency。
- `emitevent` 的 `flow.<flow>.<variable>` 事件。

这些机制可以直接扩展到 GPU session 生命周期。

### 3.4 当前缺口

当前 `render_surface` 主要是 WGPU Runtime 内部能力，还缺少公开的外部 GPU 互操作协议:

- backend negotiation。
- adapter identity matching。
- external texture session。
- shared texture + shared fence descriptor。
- native handle broker。
- surface generation / resize / device-lost 生命周期。

## 4. 后端匹配协议

后端必须在 session 建立前协商。

请求:

```json
{
  "protocol": "neon3.rpc",
  "version": 1,
  "request_id": "req-backend-001",
  "target": "wgpu-runtime",
  "method": "render.backend.negotiate",
  "params": {
    "preferred_backends": ["dx12"],
    "required_features": [
      "shared_texture",
      "shared_fence",
      "rgba8unorm_srgb",
      "depth_test"
    ],
    "adapter": {
      "vendor_id": 4318,
      "device_id": 1234,
      "luid": "optional-native-adapter-luid"
    },
    "consumer": {
      "kind": "godot",
      "pid": 12345,
      "plugin_version": "neon3-godot-adapter-1"
    }
  }
}
```

成功:

```json
{
  "status": "accepted",
  "result": {
    "backend": "dx12",
    "adapter": {
      "vendor_id": 4318,
      "device_id": 1234,
      "luid": "...",
      "name": "..."
    },
    "transport": "d3d12_shared_texture_v1",
    "features": ["shared_texture", "shared_fence", "depth_test"]
  }
}
```

失败必须明确:

```text
backend_not_available
adapter_mismatch
shared_texture_unsupported
shared_fence_unsupported
format_unsupported
consumer_protocol_too_old
```

Neon 不能因为宿主要求 DX12 就盲目返回成功。最终 adapter 必须真实可用，并且必须满足共享
资源和同步能力。

匹配是硬门槛，不是建议值。session 状态只能按以下顺序前进:

```text
created
  -> negotiating
  -> matched
  -> gpu_session_opened
  -> surface_ready
  -> streaming
```

任何一步失败都必须进入 `failed`，不能自动切换到 PNG、CPU frame 或另一个未经宿主确认的
backend。宿主如果想接受候选后端，必须在请求中明确声明允许的 backend 列表。

## 5. GPU Surface 会话

```json
{
  "method": "render.surface.open",
  "params": {
    "session_id": "host-session-001",
    "surface_id": "surface.quest-panel",
    "kind": "screen_ui",
    "size": {"width": 1280, "height": 720},
    "format": "rgba8unorm_srgb",
    "depth": false,
    "sharing": "external_gpu"
  }
}
```

World UI 不申请独立的 world surface，也不使用 placement 描述。宿主通过
`wgpu.world.ui.anchor.submit` 持续提交 world anchor：

```json
{
  "method": "wgpu.world.ui.anchor.submit",
  "params": {
    "anchor_id": "npc.blacksmith",
    "world_space_id": "project.world.main",
    "producer_epoch": 1,
    "sequence": 42,
    "timestamp_monotonic_ns": 123456,
    "position": [0.0, 1.8, 0.0],
    "billboard": true
  }
}
```

Neon 结合 world info + camera frame 把 anchor 投影成屏幕坐标，在自己的 fullscreen
composition 里渲染 NUI。

普通 UI 与 World UI 共享:

- surface identity。
- UI declaration。
- texture registry。
- input intent。
- frame sequence。
- capture 与 diagnostics。

差异只在最终 placement/composition。

## 6. D3D12 共享纹理

首个实现采用 Windows D3D12:

```text
Neon WGPU/D3D12 Device
  -> 创建可共享 D3D12 Resource
  -> CreateSharedHandle(texture)
  -> 创建共享 D3D12 Fence
  -> CreateSharedHandle(fence)
  -> broker 按 consumer PID DuplicateHandle
  -> Godot Native Adapter 打开 texture/fence
```

每个 surface 返回 opaque broker token，而不是把 Windows HANDLE 放进 JSON:

```json
{
  "surface_id": "surface.quest-panel",
  "generation": 1,
  "transport": "d3d12_shared_texture_v1",
  "broker_token": "broker-surface-001",
  "adapter_luid": "...",
  "texture": {
    "format": "rgba8unorm_srgb",
    "width": 1280,
    "height": 720,
    "mip_levels": 1
  },
  "sync": {
    "type": "d3d12_shared_fence",
    "initial_value": 0
  }
}
```

Godot adapter 使用 token 通过本机 broker 领取 duplicated handles:

```text
texture_handle
fence_handle
initial_fence_value
```

不能用纯 TCP 传递原始 HANDLE；不能把 handle 写入 eventd journal；不能让外部引擎修改 Neon
的权威 texture 生命周期。

### 6.1 单缓冲还是环形缓冲

正式实现不能假定永远只有一张 texture。surface descriptor 应允许至少配置:

```text
buffer_count: 2 or 3
```

双缓冲/三缓冲用于避免 Neon 写入下一帧时宿主仍在采样上一帧。每个 buffer 都有明确的
generation、resource state 约束和 fence value。`frame.ready` 指向可采样的 buffer index，
宿主必须等待对应 fence 后再绑定该 buffer。

resize 或 device lost 时，旧 generation 继续保持有效直到宿主收到 replacement 并完成
detach；不能复用旧 handle 伪装成新尺寸。

## 7. Frame 同步

共享纹理必须配套共享 Fence:

```text
Neon render pass complete
  -> Signal(fence, N)
  -> eventd: render.surface.frame.ready { sequence, fence_value: N }
  -> Host engine Wait(fence, N)
  -> Host samples the texture
```

事件:

```text
render.surface.opened
render.surface.ready
render.surface.frame.ready
render.surface.resized
render.surface.consumer.attached
render.surface.consumer.detached
render.surface.device_lost
render.surface.failed
render.surface.released
```

帧事件只发送 metadata:

```json
{
  "surface_id": "surface.quest-panel",
  "generation": 1,
  "frame_sequence": 184,
  "fence_value": 527
}
```

像素不进入 eventd。

## 8. WGPU 与原生 D3D12 的现实边界

`wgpu` 能选择 DX12 backend，但公共 `wgpu` API 不等于完整的外部 D3D12 resource import/export
API。D3D12 共享资源通常需要:

- `wgpu-hal` DX12 adapter/device 访问。
- 原生 `ID3D12Resource` 和 `ID3D12Fence`。
- Windows shared heap/resource flags。
- `CreateSharedHandle` / `OpenSharedHandle`。
- 正确的 resource state barrier 和 fence ownership。

因此原生互操作代码必须隔离在:

```text
neon-wgpu-runtime/src/platform/dx12_interop.rs
```

不能污染 `neon-protocol`、`neon-ui-runtime`、eventd 或普通 UI domain。

对外仍然暴露统一协议；协议里的 transport descriptor 根据后端变化。

## 9. Godot 适配器

Godot 不进入 Neon3 核心 crate。提供一个独立 Native/GDExtension adapter:

```text
tools/neon3-godot-adapter/
  neon3_godot_extension.cpp
  neon3_surface_consumer_dx12.cpp
  protocol_client.cpp
```

Godot 项目只需要:

```text
Godot scene
  -> Neon3SurfaceConsumer node
  -> session.open
  -> surface.open
  -> broker.acquire
  -> external texture material
  -> fence wait
```

Godot 使用 D3D12 renderer 时，adapter 从 Godot RenderingDevice/原生 D3D12 backend 获取设备
上下文，打开 Neon 导出的 shared resource，并将它绑定到材质或 World UI quad。

如果 Godot 当前进程实际使用 Vulkan，则必须拒绝 D3D12 session，而不是偷偷转换。之后增加
`vulkan_external_memory_v1`，协议不变。

Godot 适配器的首个可验收目标不是“做一个漂亮的 Godot 控件”，而是证明四件事:

1. Godot 能报告它实际使用的 renderer、adapter identity 和进程 ID。
2. Neon 能拒绝错误 backend 或错误 adapter，而不是继续运行。
3. Godot 能通过 broker 打开 shared resource/fence，并按 fence 消费新帧。
4. Godot 材质看到的像素确实来自 Neon 的 screen/world surface，而不是插件偷偷做的 CPU copy。

## 10. 外部引擎统一适配器契约

每个引擎只实现相同的五类能力:

```text
connect / backend.negotiate
surface.open / surface.close
surface.attach_texture
surface.wait_frame
surface.submit_intent
```

引擎差异只存在 adapter 内部:

```text
Godot -> GDExtension + D3D12 native import
Unity -> Native Rendering Plugin + D3D12 native import
Bevy  -> Rust native backend/resource import
Unreal -> RHI/D3D12 plugin
```

核心协议不出现 `godot`、`unity`、`bevy` 专用 method。

引擎适配器允许有引擎私有实现，但私有部分只能解决“如何把已打开的 native resource 绑定到
引擎材质/图像对象”，不能改变 Neon 协议语义:

```text
协议层: surface_id / generation / format / fence / frame_sequence
适配器层: Godot RID、Unity native texture、Bevy Image、Unreal RHI resource
```

适配器必须能报告自己的状态:

```text
detected
negotiating
attached
waiting
consuming
detached
failed
```

## 11. 普通 UI 与 World UI 统一模型

普通 UI:

```text
UiProgram/UiFragment
  -> Neon UI renderer
  -> screen surface texture
  -> final window composition
```

World UI:

```text
Host world entity + world-space position
  -> wgpu.world.ui.anchor.submit
  -> Neon 结合 world info + camera frame
  -> fullscreen projection（屏幕坐标）
  -> Neon UI renderer
  -> screen surface texture（fullscreen composition）
```

外部引擎如果只需要展示 Neon UI，可直接采样共享 surface。
外部引擎如果需要 Neon 把 UI 放进自己的世界，则它提供 world info、camera frame 与
world anchor（`wgpu.world.ui.anchor.submit`）；Neon 在自己的 fullscreen composition 里
把这些 anchor 投影成屏幕坐标再渲染 NUI，不交换私有 renderer object，也不把画布挂到
宿主的 Entity 上。`world_ui_pipeline` 的 3D Quad 仅作为内部 lab/未来模式。

## 12. eventd 与 GPU session

eventd 承载:

- backend negotiation result。
- session/surface lifecycle。
- generation 和 sequence。
- fence-ready metadata。
- capture request/result。
- device lost、resize、consumer disconnect。
- 外部引擎 intent 与诊断事件。

eventd 不承载:

- texture bytes 的长期存储。
- native HANDLE。
- GPU pointer、wgpu handle、bind group。
- render pass 或 resource barrier。

GPU 控制命令仍发给 `neon-wgpu-runtime`；eventd 负责状态传播和订阅。

建议的控制方向是:

```text
Host -> neon-wgpu-runtime: RPC command
neon-wgpu-runtime -> eventd: lifecycle/frame metadata
eventd -> Host: subscribed event stream
Host -> broker: native handle acquire
```

这样即使 eventd 重启，也不会让 GPU resource ownership 转移；session 需要依据 epoch 重新
订阅并重新获取当前 surface snapshot。

## 13. 验收标准

首个 D3D12/Godot slice 必须证明:

1. 宿主宣布 D3D12，Neon 实际选择 DX12 adapter。
2. adapter LUID 匹配，否则 session 明确失败。
3. Neon 创建 shared texture 和 shared fence。
4. Godot 通过 broker 获取 duplicated handles。
5. Godot 直接采样 Neon texture，无 PNG、无 CPU readback、无二次像素上传。
6. Neon signal fence 后，Godot 按 frame sequence 正确等待和采样。
7. 普通 screen UI 能显示。
8. World UI 通过 anchor 投影到屏幕坐标后，在 fullscreen composition 中正确显示。
9. resize/device-lost/generation replacement 能恢复。
10. eventd 能回放完整 session 生命周期。

还必须有负向验收:

11. Godot 运行 Vulkan、Neon 只提供 D3D12 时，协商明确失败。
12. adapter LUID 不同，即使 vendor/device 相同，也必须拒绝共享。
13. fence value 未完成时，宿主不能把该帧标记为 consumed。
14. 旧 generation 的 handle 在 resize 后不能继续被当成新 surface 使用。
15. broker token 泄漏、错误 PID、重复领取和过期领取都返回稳定错误码。
16. eventd 日志和网络抓包中不出现原始 native HANDLE。

## 14. 实施文件规划

```text
crates/neon-protocol/
  backend negotiation
  render surface session
  external texture descriptor
  frame-ready events

crates/neon-eventd/
  render session event namespace
  session lifecycle validation

crates/neon-wgpu-runtime/
  platform/dx12_interop.rs
  shared texture/fence owner
  render.surface RPC handlers

crates/neon-dev/
  service/session manifest includes eventd and GPU transport

tools/neon3-godot-adapter/
  D3D12 native consumer
  broker client
  fence wait
  Godot texture/material binding

crates/neon-testkit/
  backend matching scenario
  shared texture smoke test
  ordinary UI/world UI frame assertions
```

## 14.1 当前代码实现状态（2026-08-18）

已落地:

- `neon-protocol` 已有 backend negotiation、adapter identity、surface open、shared texture/
  fence descriptor、generation 和 frame metadata 类型。
- `AGENTS.md` 已允许受控 external host consumer，同时保留 `neon-wgpu-runtime` 唯一 GPU
  resource owner 原则。
- `neon-wgpu-runtime` 已注册 `wgpu.external_host.backend_match.v1` capability。
- `render.backend.negotiate` 已实现硬门槛和稳定失败码。
- Windows DX12 HAL interop 已从当前 `wgpu::Device` 取得 `ID3D12Device`，并可创建带
  `D3D12_HEAP_FLAG_SHARED` 的 texture、带 `D3D12_FENCE_FLAG_SHARED` 的 fence。
- `render.surface.open` 已接到窗口线程，并创建 runtime-owned shared resource。
- `render.surface.acquire` 已按 consumer PID 使用 `DuplicateHandle` 交付目标进程句柄。
- external surface 已加入窗口 redraw：当前 UI fragment 会绘制到共享 texture，并在同一
  DX12 command queue 上安排 shared fence signal。
- `render.surface.frame` 已提供当前 generation、frame sequence、buffer index 和 fence value
  查询结果。

尚未宣称完成:

- shared surface 尚未接入普通 UI / World UI 的实际绘制 pass。
- 尚未实现每帧 fence signal、`frame.ready` 发布和 consumer wait 生命周期。
- `frame` metadata 查询已实现，但 eventd 的正式 `frame.ready` 发布和订阅端 wait/consume
  回执仍未完成。
- 尚未实现 Godot GDExtension/RID 绑定，因此当前不能称为 Godot 端到端完成。
- 当前只开放单 buffer；双缓冲/三缓冲仍需在实际 frame pipeline 中接入。

验证备注:

- `cargo check --workspace` 通过。
- `cargo test -p neon-protocol` 通过。
- 关键 World UI、offscreen UI 和 renderer-owned surface 测试单独通过。
- Windows 下 `cargo test -p neon-wgpu-runtime` 并发运行时曾出现一次
  `STATUS_ACCESS_VIOLATION`；同一批关键测试串行重跑通过，仍需在真实窗口/DX12 shared
  surface session 中继续定位，不能视为全量测试已完全通过。

必须继续区分“native surface 创建成功”和“宿主已经消费 Neon UI 帧”，不能把前者当成后者。

## 15. 不可妥协的设计结论

1. 统一的是协议，不是某个引擎的 API。
2. backend 必须匹配成功后才建立 GPU session。
3. GPU texture 可以跨进程共享，但 eventd 不拥有 texture。
4. native handle 只通过本机 broker 传递，不进 JSON/event journal。
5. texture 与 fence 必须成对设计。
6. 普通 UI 和 World UI 共享 surface 模型，只差 composition metadata。
7. PNG 只允许作为调试/验收 artifact，不作为正式帧传输路径。
8. 这不是“每个引擎各写一套 Neon 插件”；各引擎只实现自己的 native resource binding。
9. `AGENTS.md` 已正式写入受控共享例外；后续实现仍不得绕过 backend/adapter matching。
