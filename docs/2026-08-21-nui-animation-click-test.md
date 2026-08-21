# NUI 状态动画点击测试问题分析与现状

日期: 2026-08-21
状态: 修复中

## 一、背景

`plan/neon3-nui-state-animation.md` 定义的 NUI 状态动画功能(M0-M3)已经实现并在单元测试中通过。目标是在 `D:\bevy-nui-host` 中能够通过点击 UI 按钮实测动画效果。

## 二、测试基线(修复前)

| 套件 | 结果 |
| --- | --- |
| `neon-ui-runtime` | 97 passed |
| `neon-wgpu-runtime` transition | 5 passed |
| `bevy-nui-host` | 7 passed |

单元测试全部通过,但端到端点击测试发现动画链路存在实际断裂。

## 三、问题根因

`UiRuntime::forward_host_request`(<code>crates/neon-ui-runtime/src/lib.rs</code>) 中:

```rust
let mut selected_motion = None;   // 方法内局部变量
```

`selected_motion` 只在语义事件处理期间存活(状态机 idle→hit 转换时被设置)。流程如下:

1. **用户点击** "Toggle status" → 指针事件到达 UI runtime
2. **状态机转换** idle→hit → 捕获 `selected_motion` (health-change, 320ms ease_out)
3. **Fragment 刷新** — 但此时 health 值仍是 82(Bevy 尚未响应语义意图)
4. **Motion 应用** → fragment 提交到 WGPU(health=82→82,无视觉效果)
5. **下一帧** Bevy 收到语义意图 → health=100 → 发送 `ui.input.frame`
6. **`handle_external_input_frame`** 更新 adapter 输入值 — **但 fragment 不会重新刷新提交**
7. 渲染器始终看到 health=82 的 fragment

**最终表现: health 条直接从 82 跳到 100,没有插值动画。**

## 四、修复方案

### 4.1 结构体变更

`UiRuntime` 新增字段:

```rust
/// Motion selected by the last state-machine transition that has not yet
/// been applied to a fragment carrying the updated authoritative inputs.
pending_motion: Option<UiTransition>,
/// WGPU renderer endpoint for fragment re-submission during input frames.
wgpu_endpoint: Option<SocketAddr>,
```

### 4.2 方法变更

- `forward_host_request`: `selected_motion` 的副本同时存入 `self.pending_motion`
- 新增 `apply_motion_to_current_fragment(motion)`:
  1. 用当前 adapter 快照刷新 cached fragment
  2. 对 fragment 的每个节点应用 `enter_transition`
  3. 通过 `forward_fragment` 重新提交到 WGPU
- `handle_external_input_frame`: 成功应用输入帧后,若 `pending_motion` 存在则消费它并调用上方法

### 4.3 时序(修复后)

1. 点击 → 状态机转换 → `pending_motion` 持久化 → fragment 提交一次(旧值+transition)
2. Bevy 发 `ui.input.frame` (health=100) → adapter 更新
3. `handle_external_input_frame` 检测到 `pending_motion` → 刷新 fragment(新值)+ 应用 transition → 重新提交
4. WGPU 渲染器从当前显示值(82)向目标(100)插值 → **动画可见**

## 五、当前验证状态

新增端到端测试:
<code>external_input_frame_applies_pending_state_motion_and_resubmits_fragment</code>

覆盖链路: flow 提交 → 语义事件 → 状态机转换 → pending_motion 持久化 → 输入帧 → fragment 重提交(含 enter_transition + 新值)。

**已知问题(未完成)**: 测试中 `handle_external_input_frame` 返回 Rejected(ui_program_stale_input_revision)。因为 fallback publication 将输入 revision 从 0 推进到 1,而测试构造的输入帧仍使用旧的 revision 0。已修改测试代码改为在 `forward_host_request` 之后捕获 revision,待验证。

## 六、后续行动

1. 修正测试的 revision 捕获,使端到端测试通过
2. 运行 neon-ui-runtime 全量测试确认无回归
3. 在 bevy-nui-host 中运行 `cargo run` 实际点击验证动画
4. 观察 WGPU 渲染器的 `active_transition_debug_snapshot` 确认动画活动