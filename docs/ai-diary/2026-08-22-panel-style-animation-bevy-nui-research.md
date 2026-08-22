日期: 2026-08-22
主题: 调研 panel 样式/动画差距 + bevy-nui-host 是否全功能 NUI
涉及的 crate / 文件路径:
  - crates/neon-ui-schema/src/lib.rs（UiStyle/UiTransition/UiTransitionState）
  - crates/neon-ui-runtime/src/nui_flow.rs（Flow 语法、style 解析）
  - crates/neon-ui-runtime/src/lib.rs（PendingStateMotion、多播）
  - crates/neon-wgpu-runtime/src/ui_renderer.rs（插值、GPU 推进、容量上限）
  - crates/neon-ui-runtime/tests/fixtures/ui/imgui-component-gallery.nui（全功能展示）
  - plan/neon3-world-ui-unified.md、plan/phase2-bevy-nui-host-work-requirement.md
  - docs/nui-flow-reference-and-bevy-status-debug.md
  - .workbuddy/memory/2026-08-18.md ~ 2026-08-21.md
  - docs/panel-style-animation-and-bevy-nui-capability-report.md（本次产物）

发现的问题:
  - panel 样式只有背景/边框/圆角/透明度/边框宽 5 项，Flow 语法层只能写 fill/line。
  - 动画只有 state machine 驱动的单段 from->to，二次 easing，无 transform/spring/keyframe/stagger/repeat。
  - bevy-nui-host 是 host/consumer，不是 NUI 实现者；全功能 NUI 由 Neon3 侧支撑。
  - bevy 当前只闭环极简 status 面板；组件 gallery 的交互闭环远未全部接入。
  - MAX_WORLD_UI_QUADS=256 硬上限 assert、GPU 锁无限重试、serve_until 串行 handler 是横向扩展最大阻碍。

采取的方案:
  - 纯调研，无代码改动。
  - 产出 docs/panel-style-animation-and-bevy-nui-capability-report.md，含样式/动画差距、bevy 架构判定、gallery 逐项支持度、P0/P1/P2 施工优先级。

当前状态: 已完成

未完成事项与下一步:
  - 如需百分百源码级确认 bevy-nui-host，需在环境中授予 D:\bevy-nui-host 读取权限后补一轮。
  - P0 施工：MAX_WORLD_UI_QUADS 去 assert、GPU 锁有界等待、RPC 长任务 job 化、hover CPU broad-phase。
  - P1 施工：样式/动画按报告优先级逐项落地。

测试与验证结果:
  - 本次为调研，没有改动代码，未运行测试。
  - 已实际枚举 D:\bevy-nui-host 目录结构（src/lib.rs 136KB、dx12_consumer.rs、gpu_readback.rs、phase2_latest_probe.rs、assets/ui/*.nui 三个文件）。
  - D:\bevy-nui-host 文件内容读取被环境权限规则拦截（external_directory deny），bevy 侧结论来自工作区既有调研文档。