---
date: 2026-08-22
topic: 调查并修复动画交互 RPC 延迟
type: implementation
crates:
  - crates/neon-wgpu-runtime
  - crates/neon-ui-runtime
  - crates/neon-wgpu-runtime/src/bin/interaction_latency_probe.rs
---

## 发现的问题

- 窗口 WGPU RPC server 在处理 composition mutation 后，通过 `recv_timeout(5s)` 等待窗口 event loop 应用 fragment，导致控制面请求把 compositor 应用延迟暴露给调用方。
- 点击视觉反馈本身在 WGPU 进程内是本地处理并通过独立线程转发语义 RPC；动画采样使用单调时间，未发现需要等待 RPC 才推进帧的证据。
- UI runtime 的 host semantic 路径仍然是同步 RPC，这是可靠语义确认路径，不能直接当作视觉动画帧驱动。

## 采取的方案

- `neon-wgpu-runtime` composition command 投递给窗口 event loop 后立即返回 accepted，不再等待 compositor ack。
- 新增 `interaction_latency_probe`，使用现有 `neon3.rpc` 和 `UiCommand::SubmitFragment`，输出 JSONL，固定 3 个 sequence、请求状态、revision 和耗时。
- 未保留一个未经验证的 UI 客户端硬超时改动；host 语义 RPC 的异步化应另行设计 receipt/event contract。

## 当前状态

已完成本次 WGPU 控制面延迟修复；UI host 业务确认仍是同步链路，视觉反馈与其分离。

## 测试与验证结果

- `cargo test -p neon-wgpu-runtime --lib`: 121 passed, 1 ignored。
- `cargo test -p neon-ui-runtime --lib external_input_frame_applies_pending_state_motion_and_resubmits_fragment -- --nocapture`: passed。
- `cargo test -p neon-ui-runtime --lib`: 98 passed, 1 failed。已有 gallery 测试失败，错误为 `ui_program_stale_input_revision`，在回退本次 compositor 改动后仍复现。
- 实际启动 `neon-wgpu-runtime --window-server 127.0.0.1:39471` 并运行：
  `cargo run -q -p neon-wgpu-runtime --bin interaction_latency_probe -- 127.0.0.1:39471`
- 探针 JSONL 实际耗时：`1.7855ms`、`0.3551ms`、`0.4141ms`，均 `status=accepted`、`pass=true`、退出码 0。
- `cargo check -p neon-wgpu-runtime --bin interaction_latency_probe`: passed；存在既有 dead-code/unused assignment warnings，无编译失败。

## 未完成事项与下一步

- 修复 UI gallery 测试的 input revision 时序问题。
- 若要彻底消除 host 语义 RPC 的同步等待，应增加基于现有 `request_id` 的 accepted receipt 与异步 publication/event 流，不应再靠固定 sleep 或硬超时猜测完成。

## 追加工作

- WGPU 点击语义发送线程增加 250ms 有界 RPC 超时；窗口线程仍不等待。
- `pointer_delivery` 诊断现在包含 `queued`、sequence、node path、最终状态和 `elapsed_ms`。
- 实际 window-server 探针再次通过：`2.0271ms / 0.3981ms / 0.3853ms`，退出码 0。
- 聚焦验证：UI pending motion 测试通过；WGPU animation activity 测试通过；interaction trace 两项测试通过。
- 编译 warning 仍为既有 unused/dead-code 类警告，无失败。

## 追加调查：用户仍观察到约 1 秒延迟

- 用户提供的日志显示后续 render loop 稳定约 17ms，RPC/pointer latency 约 0.4-0.6ms；首段存在一次 `max=871.8ms` 的启动期 GPU/render 抖动。
- 日志中 `status-root` 出现重复 transition start，且 host intent 日志与 transition 日志顺序可疑，不能仅凭自由文本判断真实时序。
- 新增结构化上下文日志：UI motion dispatch 的 event/motion/fragment revision/input revision；external input frame 的 revision/changed slots/pending motion；WGPU transition 的单调时间和 numeric from/target。
- 聚焦测试仍通过：pending state motion、numeric transition、animation activity。
- 下一步需要用新日志确认是输入命中延迟、首帧 GPU 编译，还是旧 fragment 先启动空 transition；本轮没有宣称问题已完全解决。

## 追加修复：状态 motion 未覆盖数值控件

- 用户日志确认 `motion dispatch` 和 renderer transition start 在点击后立即发生，但 `health_bar` 没有显式 `style` 记录，因此 `apply_transition_to_fragment` 没有给它挂 transition。
- 修复为整个 presentation subtree 传播 motion；style 记录只负责覆盖声明的 style，numeric 控件也进入 renderer 的 frame-time interpolation。
- 不变 target 由 renderer 的 settled-target 判断跳过，避免旧 publication 产生可见空动画。
- 验证：pending motion 测试通过；numeric transition 测试通过；编译通过，仅有既有 unused/dead-code warnings。

## 追加调查：panel resize 时序

- 新日志显示 panel 的实际 bounds 已从 `320x160` 过渡到 `520x300`，transition 在 WGPU 单调时间 `t=14.536s` 启动；health frame 在约 `94ms` 后到达。
- 用户日志首段仍有 `924ms` render gap / `908ms` render time，属于首帧 GPU 初始化或 shader 编译候选问题。
- 增加 UI/WGPU 统一 Unix 毫秒时间戳，下一轮可直接和 Bevy 日志对齐，区分点击前等待与 transition 后首帧等待。
- 编译检查通过，warnings 仍为既有 unused/dead-code 类。

## 追加修复：host 响应阻塞 panel motion

- 统一时间戳证实：`motion dispatch unix_ms=1787340083203`，`transition start unix_ms=1787340085241`，延迟约 2038ms。
- `input frame` 在 `1787340085481` 才到达，说明 panel transition 首次提交确实发生在等待 host/domain 后，而不是 renderer 首帧阶段。
- 修复：state-machine dispatch 后立即提交 optimistic presentation fragment；host/domain 仍通过原 RPC 返回 authoritative publication，后续刷新当前动画。
- 聚焦验证：`cargo check -p neon-ui-runtime` 通过；pending motion 测试通过。

## 追加调查：输入命中失败

- 用户最新日志显示成功语义前持续出现 `press_without_semantic_hit`，成功 dispatch 到 transition 仅约 20ms，确认主要延迟来自输入命中失败，而非动画或 RPC。
- WGPU external pointer 的拒绝错误已增加 pointer 坐标、plan 数量、binding 数量和 semantic node 列表，便于区分坐标尺度、旧 plan、缺失 binding 与非交互区域。
- `cargo check -p neon-wgpu-runtime` 通过；warning 仍为既有 unused/dead-code 类。

## 追加修复：重复点击造成大量无关 numeric transitions

- 日志显示一次 panel motion 会启动大量 `hp*` numeric 节点，并造成 `render max=224ms`、`dropped=15`，解释了第一次/第二次点击体验差异。
- motion 现在只作用于 state-style 节点及其子树中的 numeric controls，避免页面其他 numeric 节点被 panel transition 牵连。
- pointer miss 诊断增加 `pointer_probe_snapshot`，下一轮可直接看到 fallback hit、modal 和 scroll 状态。
- `cargo check -p neon-wgpu-runtime` 和 pending motion 测试通过。

## 追加优化：高频 motion 去重

- 用户日志显示第一次点击后大量 `hp*` 节点被重复启动，且 render 出现 198-224ms 峰值和 dropped frames。
- renderer 现在对没有显式 style 变化、numeric from/target 相同的节点直接 settled，不进入 active transition；父 panel 继续负责结构动画，真实数值变化仍插值。
- pointer probe 已增加 semantic target bounds/clip/binding 诊断，便于继续处理动画期间的坐标命中问题。
- 验证：numeric transition、animation activity、pending motion 测试及两个 crate `cargo check` 通过；保留既有 warnings。

## 追加高强度性能优化

- 移除 transition start/complete 的逐节点同步 stderr 输出；高频动画不再把 Windows console I/O 放进 renderer 路径。
- renderer 对无显式 style 变化且 numeric from/target 相同的节点直接 settled，避免 active map 重启。
- 实际 window-server JSONL probe 通过：`3.08ms / 0.45ms / 0.42ms`，退出码 0。
- 聚焦 animation activity、numeric transition 和 pending motion 测试通过。
- 输入命中和 latest-wins 仍是进行中：当前 probe 已能显示 semantic target bounds，下一阶段继续处理动画中坐标命中与 reset 取消语义。

## 追加：边界值与 World UI 案例

- 输入 store 对 `F32Range` 做 canonicalize：clamp 到范围，并将接近 min/max 的值吸附到精确边界，避免 `99.99999` 与 `100.0` 触发重复 revision/transition。
- 新增 fixture `world-ui-panel-motion.nui`：World UI panel 点击后从 `240x48/#183445` 过渡到 `420x112/#6B3B35`，reset 使用反向 motion 恢复。
- 新增测试覆盖 fixture 解析、camera/anchor 声明、正向 toggle 和 reset 反向状态转换。
- 验证：World UI motion fixture 测试通过；pending motion 测试通过；两个 runtime check 通过。

## 追加：reset 重复 motion 修复与真实怪物面板边界

- 发现同一点击会先提交 optimistic motion，再在 host publication 返回后再次给 fragment 挂 motion，导致 reset/反向 transition 被重复重启。
- 修复为 optimistic submit 成功后，authoritative fragment 只更新目标值，不重复附加 transition；renderer 继续从当前 active sample 过渡。
- `world-ui-panel-motion.nui` 已覆盖真实 World UI contract 的 camera、anchor、放大、缩小、变色和 reset 状态转换。
- 当前 Neon3 工作区没有 Bevy 怪物面板源码，真实运行中的 `bevy-nui-host` 位于工作区外；本轮完成了 Neon3 侧 contract 和测试，尚未改外部怪物 fragment。
- 验证：world panel fixture、pending motion 测试和两个 runtime check 通过。

## 追加：真实怪物 World UI 命中定位

- 用户日志显示点击坐标如 `(583,375)`，但 renderer semantic panel 仍是 `(32,32)-(352,192)`，因此 fallback hit 必然 miss。
- 同时出现 `world_anchor_batch_rejected: InvalidWorldAnchor`，说明怪物 anchor 投影没有进入 renderer，不能先修按钮动画。
- world anchor reject response 现在包含 `sequence`、`timestamp_monotonic_ns`、`anchor_count`，用于定位外部 Bevy producer 的非法字段。
- 运行时 World UI fixture/状态机测试仍通过；真实怪物面板接入仍待外部 producer 修正 anchor batch。

## 追加：多 World UI 投影命中

- World UI 过滤路径增加兼容投影：当 producer 没有提供有效 normalized `screen_x/screen_y/view_distance`，但 anchor position 和匹配 camera frame 有效时，使用同一 Neon 投影函数计算屏幕位置；不改变 owner/transport 边界。
- anchor batch 拒绝错误增加首个 anchor 的 id、position、screen 坐标、view distance，便于精确修复外部 producer，而不是放宽所有 anchor 校验。
- 修正 `neon-world-bridge` 现有测试构造缺失的新 anchor 字段；6 个 bridge 测试通过。
- World UI motion fixture 通过；两个 runtime check 通过。

## 追加：World UI 精确命中 renderer 选择

- 最新日志证明 pointer event 一直使用 `screen_ui` renderer：World UI/monster 投影 surface 的点击却拿 `self.ui` 的 screen fragment 做 hit-test，因此只看到 `status-root`，实际 World UI 不会响应。
- `HeadlessExternalGpu::pointer_event` 现在按 `surface_id -> RenderSurfaceKind` 选择 renderer：`WorldUi` 使用独立 `world_ui` renderer，`ScreenUi` 使用 screen renderer；同时从相同 surface kind 的 filtered fragment snapshot 命中，避免多个 World UI 混在一起。
- anchor batch reject 继续保留详细首 anchor 字段；投影 fallback 使用匹配 camera frame 和 anchor position。
- 验证：world-bridge 6 tests passed；World UI motion fixture passed；两个 runtime check passed。

## 追加：未注册 surface 的 World UI 兼容命中

- 最新日志仍显示 pointer 命中的是 3 个 screen 节点，说明外部 pointer `surface_id` 没有对应已注册的 `RenderSurfaceKind`。
- 增加兼容推断：当 surface 映射缺失但当前 combined fragment 声明了 `CameraVisibility` 时，pointer 使用 WorldUi renderer 和 WorldUi split snapshot；只有没有 world declaration 时才拒绝映射。
- 已注册 surface 仍严格按自身 kind 命中，多 World UI 共享 WorldUi renderer 但使用同一 filtered fragment 集合，最终按实际 bounds/clip/binding 选择具体节点。
- 编译、world-bridge 6 tests、World UI motion fixture 均通过。

## 追加：外部 host 仍只提交 Screen UI

- 用户最新日志仍为 `plan=12 bindings=3`，只有 `status-root/status-action/status-reset`，证明外部 host 当前没有提交怪物 World UI fragment；不是 WGPU 命中算法缺失。
- Neon3 pointer server 已加入 surface 映射缺失诊断，并在 combined fragment 声明 `CameraVisibility` 时推断 WorldUi renderer；但如果 host 没有 world fragment，无法产生命中对象。
- Neon3 侧编译通过；当前真实验收被外部 host 的 fragment/anchor producer 阻塞。

## 追加：确认并修复 external World UI ID 图路径

- 调查确认：WindowedRuntime 有 R32Uint hit target/readback，但 `HeadlessExternalGpu::pointer_event` 原来只调用 `hit_id_at_pointer()` CPU 几何 fallback，World UI 点击没有经过 ID 图。
- external pointer 现在为对应 `RenderSurfaceKind` 的 renderer 渲染私有 R32Uint pointer target，并进行单点 readback；只有 Down 读取 ID，Move/Up 不重复触发 GPU readback，避免高频输入阻塞。
- ID 命中后再通过同一 renderer 的 `hit_bindings` 解析 semantic event，多个 World UI 使用各自 surface kind 的 fragment 集合。
- 编译检查通过；World UI fixture、world bridge tests 通过。真实怪物端到端仍需外部 host 提交 World UI fragment/anchor。
