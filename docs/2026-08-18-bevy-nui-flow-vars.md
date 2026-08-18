# Bevy NUI Flow 变量桥接心得

今天确认，Bevy 接入不能只做一张 shared texture。真正干净的结构是三条通道:

```text
渲染通道: color target + ID target
控制通道: surface/camera/click RPC
变量通道: nui_flow_vars -> UiInputFrame -> UI Runtime -> eventd
```

Bevy 的 ECS 是游戏状态权威，NUI Flow 是 UI 声明和 UI 输入权威，eventd 是观测事件中心。
这三者不能互相越权。

最重要的宿主体验是:

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

结构体更新时由 adapter 自动生成 diff frame，不允许业务代码手写 JSON。UI Runtime 接受后，
声明了 `emitevent` 的变量再通过 eventd 广播出去。

当前 Bevy 最新 crates.io 版本确认是 `0.19.1`。案例位置:

```text
D:\Neon3\cases\bevy-nui-host
```

案例目标是加载 `character.glb`，让角色可行走，并在角色头顶展示头像、名字、等级、血条、
蓝条和状态文本。glTF 是 Bevy 资产，NUI 文件仍由 Neon3 UI Runtime 负责，不复制到 Bevy
自己的 UI parser。

当前实现仍是协议/ECS 桥接和案例骨架；真实 Bevy D3D12 external texture RenderGraph、
ID target GPU readback、eventd streaming 和完整 glTF asset 需要后续逐项接通。

当前已经实现的代码切片:

```text
nui_flow_vars! macro
  -> full snapshot / sparse diff
  -> ui.input.frame
  -> UiHostAdapter::apply_external_input
  -> existing emitevent publisher
```

这条变量链路和点击链路是有意分开的。点击仍然使用 `ui.host.inbound`；ECS 状态更新使用
`ui.input.frame`，避免把“状态同步”伪装成“用户交互”。

需要特别记住: Bevy 案例现在申请了 color target 和 ID target 的协议描述，但 WGPU Runtime
还没有完成第二张真实 `r32uint` shared texture 的 native 分配和绘制。当前不能宣称双目标
GPU 端到端完成。
