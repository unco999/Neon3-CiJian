日期: 2026-08-22
主题: 修复多面板动画第二次点击失效并解除 host 阻塞
涉及的 crate / 文件路径:
  - crates/neon-ui-runtime/src/lib.rs
  - crates/neon-ui-runtime/src/bin/ui_host_animation_probe.rs
  - docs/ai-diary/2026-08-22-multicast-animation-interruption.md

发现的问题:
  - 第一个面板的 deferred host publication 完成时，completion 使用 None 覆盖了已经乐观推进的 flow_state_machine，第二个面板因此没有 transition dispatch。
  - serve_forwarder 在下一次 request 前同步执行 host RPC；慢 host 会阻塞后续点击、hover 和取消操作。
  - 多个 host completion 并发返回时，最终 fragment 可能乱序覆盖。
  - component gallery 网格测试使用空 scalar change 却期待 input revision 自动递增，和 UiInputStore 的稳定 revision 规则冲突。

采取的方案:
  - deferred completion 只有在收到新的 state machine 时才替换当前 state machine，None 不再清空状态。
  - host forward 改为单个有序后台 worker；UI 主循环只轮询完成结果，pointer/semantic request 先提交 optimistic fragment 并立即返回。
  - shutdown 使用 2 秒有界等待，保证已经 accepted 的 publication 有机会提交最终 fragment。
  - 新增 `src/bin/ui_host_animation_probe.rs`，使用固定双面板 Flow、250ms host 延迟和 JSONL 输出，验证 optimistic/final revision、面板 transition 和响应时延。
  - 更新网格测试的空 publication expected input revision 为 0。

当前状态: 已完成

未完成事项与下一步:
  - WGPU focused test `parent_transition_moves_child_panel_from_the_same_sampled_origin` 仍失败，需要单独修复现有 renderer subtree transition / offscreen expectation。
  - WGPU 全包测试曾出现 Windows `STATUS_ACCESS_VIOLATION`，需在 GPU 测试隔离后继续定位。

测试与验证结果:
  - `cargo run -q -p neon-ui-runtime --bin ui_host_animation_probe`: 通过；输出 revision `[1,2,3,4,5]`，第二次 response elapsed `8ms`，host delay `250ms`，最终 `status=passed`。
  - `cargo test -p neon-ui-runtime presentation_starts_before_host_response -- --nocapture`: 通过。
  - `cargo test -p neon-ui-runtime generic_host_route_validates_inbound_and_submits_publication_fragment -- --nocapture`: 通过。
  - `cargo test -p neon-ui-runtime motion -- --nocapture`: 9 个测试通过。
  - `cargo test -p neon-ui-runtime`: 104 个测试通过。
  - `cargo test -p neon-wgpu-runtime ui_renderer::tests:: -- --nocapture`: 55 通过，1 失败，失败项为 `parent_transition_moves_child_panel_from_the_same_sampled_origin`。
  - `cargo test -p neon-wgpu-runtime`: 未通过；测试过程中出现上述 GPU focused failure，并最终出现 Windows `STATUS_ACCESS_VIOLATION`。
  - `git diff --check`: 通过；仅报告工作区已有文件的 LF/CRLF 警告。
