# Neon3 今日心得与决策记录

日期:2026-08-18

## 今天确认的方向

Neon3 不应该为 Godot、Unity 或某一个游戏引擎单独做一套接口。外部引擎应该都是同一种
角色：**宿主协议 client + GPU surface consumer**。

真正要解决的不是“如何把一张 PNG 送给 Godot”，而是:

```text
宿主后端能力
  -> Neon 后端匹配
  -> GPU adapter 匹配
  -> shared texture + shared fence
  -> Neon UI / World UI 渲染
  -> 宿主直接采样
```

这不是为了把 Neon 变成 Godot 插件，也不是让每个引擎拥有一套 Neon 私有 API。我们要做的
是一个“宿主接入层”: 宿主报告它已经在使用的 renderer 和 adapter，Neon 选择并确认匹配，
然后 Neon 把自己渲染出的 surface 以宿主能直接消费的原生 GPU 资源交出去。

## 对之前方案的修正

之前提出 PNG/CPU frame 作为外部测试路径，只适合临时截图验收，不符合我们要验证真实 GPU
World UI 的目标。正式路径必须是原生 GPU texture 共享，PNG 只能保留为 capture artifact。

之前提出由 eventd 管纹理也不准确。eventd 是事件和 session 中心，不应拥有纹理、GPU handle
或 render pass。正确分工是:

```text
eventd                 session、事件、序号、生命周期、订阅
neon-wgpu-runtime      backend、adapter、texture、fence、最终合成
neon-ui-runtime        UiProgram、UiFragment、emitevent、语义输入
外部引擎               协议 client、纹理 consumer、World UI 场景宿主
```

## 今天看到的现有基础

仓库已经有:

- `wgpu.ui.render_surface.v1` capability。
- WGPU Runtime 内部 `register_render_surface` / `ensure_render_surface`。
- `world_ui_pipeline.rs`，可将 UI texture 投影到 3D 世界并进行深度测试。
- `wgpu.render.target.capture`。
- `UiProgram`、`UiFragment` 和 `render_surface` IR 组件。
- 独立 `neon-eventd`，已接入 UI Runtime 的 `emitevent` 统一能力。

目前缺的是公开的外部 GPU interop contract，而不是重新发明 UI surface。

## 最终技术选择

首个适配目标:

```text
Windows
D3D12
Godot D3D12 renderer
native GDExtension adapter
```

原因不是要绑定 Godot，而是 D3D12 的 shared resource/shared fence 能最快证明真实零拷贝
跨进程纹理链路。协议里不会出现 Godot 专用方法。

今天的关键词是“先匹配，再共享，再消费”:

```text
先匹配: backend + adapter LUID + format/features
再共享: texture + fence + broker token + generation
再消费: host wait fence -> sample texture -> report frame sequence
```

不能把这三步压成“给一个纹理地址就完事”。纹理地址、Windows HANDLE、Godot RID 都不是
协议；它们只是各自平台/引擎内部的实现细节。

## 必须记住的难点

1. `wgpu` 的 backend 选择和原生 texture import/export 不是一回事。
2. 选择 DX12 后，还要确认 Neon 与宿主使用同一物理 adapter/LUID。
3. shared texture 必须带 shared fence，否则宿主不知道何时可以采样。
4. Windows HANDLE 不能放进 JSON；需要本机 handle broker 和 `DuplicateHandle`。
5. resize、device lost、surface replacement 必须有 generation。
6. eventd 可以发布 `frame.ready`，但不能保存 texture handle。
7. Godot 当前若使用 Vulkan，不能假装兼容 D3D12，必须协商失败或以后增加 Vulkan transport。
8. shared texture 是 Neon 拥有、宿主消费，不是双方随便读写的公共内存。
9. 单张纹理可能造成读写竞争，正式 surface 要为双缓冲/三缓冲和每 buffer fence 留出协议位置。
10. resize/device lost 产生新 generation，不能复用旧 handle。
11. `AGENTS.md` 仍保留旧的“禁止跨进程共享纹理”条款，正式实现前必须先把今天的受控共享
    例外写入架构约定。

## 统一的两个 UI 形态

普通 UI:

```text
UiFragment -> Neon UI renderer -> shared screen texture -> host screen composition
```

World UI:

```text
UiFragment -> Neon UI renderer -> shared texture
           -> anchor/transform/billboard/depth metadata
           -> Neon world_ui_pipeline 或 host world composition
```

两者使用同一个 surface/session/texture/frame 协议，不再拆成 Godot UI、Godot World UI、Unity
UI 等多套 API。

## 今天的工程原则

> 协议统一，后端协商，GPU 资源归 WGPU Runtime，事件归 eventd，外部引擎只实现 adapter。

接下来所有实现都应围绕 `plan/neon3-host-engine-gpu-interop.md`，先把 D3D12 shared texture+
fence 的真实链路打通，再扩展其他后端。不要用 PNG 验证来冒充 GPU 互操作完成。

## 给未来自己的提醒

如果以后有人提出“先用 PNG 证明一下就算接通”或“让 eventd 直接保存 GPU handle”，要回到
今天的结论检查:

```text
PNG = capture/debug artifact，不是正式 frame transport
eventd = session/event coordinator，不是 GPU resource owner
Godot = 第一个 adapter，不是核心协议的特殊分支
wgpu-runtime = 唯一 Neon GPU owner
```

真正的完成标准是: 宿主后端和 adapter 匹配成功，Neon 创建的 D3D12 resource/fence 被
Godot 原生打开，Godot 等待 fence 后直接消费 Neon 帧，并且 screen UI 与 World UI 都走通。

## 已开始实现的部分

今天已经从设计进入代码:

```text
neon-protocol
  -> backend / adapter / surface / fence schema

neon-wgpu-runtime
  -> render.backend.negotiate
  -> render.surface.open
  -> render.surface.acquire

DX12 HAL
  -> ID3D12Device
  -> shared texture
  -> shared fence
  -> DuplicateHandle(target PID)
```

已验证 `cargo test -p neon-protocol`、Runtime 协商测试和 `cargo check --workspace` 通过。

当前必须准确描述为“共享资源创建与 handle broker 控制面已接通”，而不是“Godot 已经显示 Neon
UI”。下一段工作是把实际 UI/world composition 写入 shared surface，signal fence 并发布
`render.surface.frame.ready`，然后再写 Godot consumer。

补充: 实际 UI/world composition 已经开始写入 shared surface，DX12 queue 也已经安排 fence
signal；尚未完成的是 eventd 正式发布 `frame.ready`、宿主 consume 回执和 Godot GDExtension
绑定。Windows 并发 GPU 全量测试有一次 access violation，关键测试串行通过，后续要用真实
window + shared surface session 继续定位。
