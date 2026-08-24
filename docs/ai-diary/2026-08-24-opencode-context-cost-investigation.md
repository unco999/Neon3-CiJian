---
date: 2026-08-24
topic: 调查 OpenCode 单次上下文异常膨胀与费用保护
type: research-and-config
---

## 日期

2026-08-24

## 主题

调查 OpenCode Desktop 出现约 700K 上下文并增加项目级限制。

## 涉及的 crate / 文件路径

- `opencode.json`
- `docs/ai-diary/2026-08-24-opencode-context-cost-investigation.md`
- `C:\Users\10540\.local\share\opencode\log\opencode.log`
- `C:\Users\10540\.local\share\opencode\opencode.db`
- `C:\Users\10540\.local\share\opencode\tool-output\`

## 发现的问题

- 项目原有 `opencode.json` 没有上下文、压缩或工具输出限制。
- 根目录 `AGENTS.md` 约 32,548 bytes，属于固定输入开销，但不足以单独解释 700K。
- OpenCode Desktop 日志显示版本 `1.18.18`，当前模型为 `openai/gpt-5.6-terra`，同一 Neon3 session `ses_fd3962...` 连续运行到至少 `step=21`，并创建了多个 explore 子 session。
- OpenCode 本地数据库约 7.6 GB；工具输出目录包含多份约 1.0-2.6 MB 的输出文件，说明长工具结果和会话历史会持续累积。
- 日志在 2026-08-23 22:32 明确记录 `AI_APICallError: insufficient balance`。日志没有记录可核对的每次 `input_tokens` 数值，因此不能把“700K”直接等同于已计费 input token；但会话膨胀和多步重复发送是明确风险。

## 采取的方案

在项目级 `opencode.json` 中：

- 将 `openai/gpt-5.6-terra` 和 `yunquzhilian/gpt-5.6-terra` 的识别上下文上限设为 100,000，输出上限设为 16,000。
- 开启自动压缩和旧工具输出裁剪，保留 8 个最近 turn，并将最近内容预算设为 80,000 token。
- 将单个工具输出限制为 200 行或 12,000 bytes。
- 不限制 build/plan/general/explore agent 的单轮步数，避免截断正常长任务；上下文保护由模型 context、自动压缩和工具输出裁剪负责。

## 当前状态

已完成配置修改，待退出并重启 OpenCode Desktop 后生效。

## 未完成事项与下一步

- 尚未读取全局 `C:\Users\10540\.config\opencode\opencode.json`，因为当前项目外部目录权限拒绝访问；如全局配置覆盖模型，应在全局配置中同步相同保护。
- 尚未获得 provider 账单接口的真实 `input_tokens` / `cached_input_tokens` 明细；需要在 provider 控制台或 OpenCode 请求 usage 中核对。
- 不删除数据库、日志或历史会话；清理需要用户明确授权并先备份。
- Image 功能改造已完成本轮实现，后续可继续接入真实外部引擎具体编码格式或压缩图片格式；当前 probe 使用固定 RGBA8 输入。

## 测试与验证结果

- `webfetch https://opencode.ai/config.json`: 成功；确认 `provider.models.*.limit.context`、`compaction`、`tool_output` 和 agent `steps` 均为受支持字段。
- `webfetch https://opencode.ai/docs/config/`: 成功；确认 `compaction.prune` 和 `reserved` 的语义。
- `Get-Item AGENTS.md` / `Get-Content AGENTS.md`: 成功；输出 `bytes=32548`，PowerShell 行数统计为 `698`。
- OpenCode 日志检索：成功；确认 `version=1.18.18`、模型与 provider、session 多步循环和 `insufficient balance`。
- JSON 解析：成功；确认 `opencode.json` 可解析，两个 provider 的 context 均为 `100000`，工具输出上限为 `12000` bytes，未设置 agent 步数限制。
- `git diff --check`: 通过；仅报告现有工作区文件的 LF/CRLF 转换警告，没有新增空白错误。

## Image 改造验证补充

- `cargo check --workspace`: 通过；存在既有 unused/dead-code warnings，无编译失败。
- `cargo test -p neon-ui-schema`: 28 passed。
- `cargo test -p neon-ui-runtime --lib`: 106 passed。
- `cargo test -p neon-ui-schema external_image_binding_allows_image_without_project_asset_ref -- --nocapture`: 1 passed。
- `cargo test -p neon-wgpu-runtime ui_renderer::tests::external_image_binding_uploads_and_renders_without_asset_ref -- --nocapture`: 1 passed；固定 RGBA8 图片通过 WGPU offscreen 像素检查。
- `cargo build -p neon-ui-runtime -p neon-wgpu-runtime --bins`: 通过，保留既有 warnings。
- `cargo run -p neon-ui-runtime --bin image_resource_probe`: 通过，实际启动 `neon-wgpu-runtime --window-server` 与 `neon-ui-runtime --forward-server`，JSONL 步骤 `wgpu.health`、`ui.health`、`ui.image.upload`、`ui.image.binding`、`wgpu.image.inspect` 全部 `pass=true`；实际结果包含 `gpu_owner=neon-wgpu-runtime-window`、`texture_index=0`、`generation=1`、`region={x:1,y:1,width:2,height:2}` 与 `uv`。
- 后续 Flow probe 同样通过：上传后提交完整 `ui.flow.submit`，返回 `surface_id=external-image-flow`；WGPU consumer diagnostics 报告 `resident=true`、`texture_index=0`、`generation=1`、`region={x:1,y:1,width:2,height:2}`，证明 Flow Image 不再依赖 projectd 的占位 AssetRef。
- `cargo test -p neon-ui-runtime nui_flow::tests::image_resource -- --nocapture`: 3 passed，覆盖旧 AssetRef 兼容路径、未绑定 snapshot 的外部 ImageBinding 路径和缺失 snapshot 错误。
- `cargo test -p neon-ui-schema`: 29 passed。

## Bevy 最终图片检查

- 用户要求检查 `D:\bevy-nui-host` 中 `city-sign` 的最终 Bevy composite 图片，并明确授权读取和启动该外部项目。
- 实际阻塞：当前工具的 `external_directory` 策略仍拒绝 `D:\bevy-nui-host` 的 glob/read/shell workdir 请求，因此未能读取 host 配置、启动 `neon3-bevy-nui-host.exe` 或取得最终 composite 截图。
- Neon3 本地证据：`IMAGE_SHADER` 使用 atlas `textureLoad`，image draw pass 在上传 instance buffer 后绑定 atlas bind group 并调用 `image_pipeline` draw；真实 window probe 已确认 external image `resident=true`、有效 `uv`、`texture_index=0`、`region={x:1,y:1,width:2,height:2}`。这些证明 source/atlas/WGPU image draw 输入就绪，但不证明 Bevy 最终 composite 已显示像素。
- 当前状态：进行中（被外部目录权限阻塞）。下一步是在允许实际访问 `D:\bevy-nui-host` 的运行环境中启动 release host，采集 host final target screenshot，并对 `city-sign` screen bounds 进行非背景像素断言。
