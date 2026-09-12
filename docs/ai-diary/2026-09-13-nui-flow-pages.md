---
date: 2026-09-13
type: implementation
topic: 建立 NUI Flow GitHub Pages 文档站
status: completed
---

# NUI Flow GitHub Pages

## 日期

2026-09-13

## 主题

审阅当前 NUI Flow V1 的 grammar、schema、runtime、fixture 与 probe，并建立一个独立的 GitHub Pages 文档站。

## 涉及的 crate / 文件路径

- `crates/neon-ui-runtime/src/nui_flow.rs`
- `crates/neon-ui-runtime/src/nui_state_machine.rs`
- `crates/neon-ui-runtime/src/lib.rs`
- `crates/neon-ui-runtime/src/host_adapter.rs`
- `crates/neon-ui-runtime/src/bin/neon3_authoring_probe.rs`
- `crates/neon-ui-runtime/src/bin/image_resource_probe.rs`
- `crates/neon-ui-runtime/tests/fixtures/ui/*.nui`
- `crates/neon-ui-schema/src/lib.rs`
- `tests/fixtures/ui/*.nui`
- `docs/nui-flow/index.html`
- `docs/nui-flow/styles.css`
- `docs/nui-flow/app.js`
- `.github/workflows/nui-flow-pages.yml`
- `README.md`

## 发现的问题

- 初次运行 NUI runtime 测试时，`UiProgram` 的两个 host adapter 测试构造器和 `UiIrDocument` 的一个 compiler 测试构造器没有初始化新加入的 `context_menu_records` 字段，导致 runtime test profile 无法编译。
- NUI 文档、设计稿和当前 parser 存在时间差：当前 parser 已接受 `vec2`、`vec4`、`color`、`asset_handle`、struct、array、简单 derived bool、skin、geometry、material、context menu、tree view 等；部分能力的 compiler、host 或 live renderer 集成仍需单独验证。
- `format_nui_flow` 是保守的语义 formatter，不是完整源码 whitespace/comment/全部高级声明的 round-trip formatter。
- struct/array 字段路径和 derived input 在 parser/schema 层有测试，但复杂 runtime publication 路径不能只凭 parser 成功来判断稳定。

## 采取的方案

- 只补充缺失字段的 `BTreeMap::new()` 初始化，不修改协议或运行逻辑，使现有 NUI runtime 测试和 authoring probe 可以执行。
- 新增 `docs/nui-flow/` 静态站点，无第三方构建依赖，包含：快速开始、所有权模型、grammar、input/binding、布局、组件词典、statechart/motion、交互、DataGrid、资源、patch、案例索引、调试/验收和能力矩阵。
- 站点使用现有 `docs/media/nui-guide/*.png` 真实 headless 渲染图，链接仓库 fixture、源码和 probe；页面状态明确区分稳定、已覆盖、部分和设计中。
- 新增原生 JavaScript 交互：主题切换、移动端目录抽屉、章节搜索、代码复制、能力状态过滤、目录高亮和图片失败提示。
- 新增 `.github/workflows/nui-flow-pages.yml`，把 `docs/` 作为 Pages artifact，页面路径为 `/nui-flow/`；README 增加直达链接。

## 当前状态

已完成。未执行 git commit 或 push；GitHub Actions 的云端部署需在仓库推送后由 GitHub 执行。

## 未完成事项与下一步

- 当前环境没有安装 Playwright，未做浏览器截图级自动化验收；已完成本地 HTTP、HTML 结构、资源路径和 JavaScript 语法检查。
- Pages workflow 的云端 job 尚未执行，需在 GitHub 仓库启用 Pages / Actions 后观察实际 deployment URL。
- 页面状态矩阵应随 NUI runtime 能力变化同步维护，尤其是 struct/array、custom shader live compile/bind、world panel host 数据面和完整 formatter。

## 测试与验证结果

### 初次失败

- `cargo test -p neon-ui-runtime nui_flow::tests::`：编译失败，3 处 `missing field context_menu_records`。
- `cargo test -p neon-ui-runtime nui_state_machine::tests::`：同一编译阻断。
- `cargo test -p neon-ui-runtime --lib`：同一编译阻断。

### 修复后通过

- `cargo test -p neon-ui-runtime nui_flow::tests::`：83 passed，0 failed。
- `cargo test -p neon-ui-runtime nui_state_machine::tests::`：8 passed，0 failed。
- `cargo test -p neon-ui-schema`：38 passed，0 failed；有 1 个既有 `unused_mut` warning。
- `cargo test -p neon-ui-runtime --lib`：151 passed，0 failed。
- `cargo test -p neon-ui-runtime nui_flow::tests::rich_text_spans_are_typed_data_not_separate_text_nodes`：1 passed，0 failed。
- `'{"request_id":"guide-1",...}' | cargo run -q -p neon-ui-runtime --bin neon3_authoring_probe`：输出 `status: "passed"`、`stage: "compiled"`，包含 input schema、canonical IR、2 个节点、1 个 binding、1 个 event 和 layout hash。
- 使用 Node UTF-8 驱动同一个 `target/debug/neon3_authoring_probe.exe` 批量检查 `tests/fixtures/ui/*.nui`：4 个 fixture compiled，4 个 fixture 按真实 diagnostics 失败；失败为 asset/gallery clip budget、terrain 缺 render resource、stress fixture 第 320 行缩进错误。第一次 PowerShell 批量尝试因字符串被包装成 `{value: ...}` 而产生 `invalid_probe_request`，已定位并排除，不作为 NUI 结果。
- `node --check docs/nui-flow/app.js`：通过。
- Node HTML tag-stack 检查：`{"status":"passed","unclosed":[],"mismatched":[]}`。
- 页面内容计数检查：16 个 section、25 个代码块、10 个案例、7 张渲染图，`status: "passed"`。
- 相对链接检查：66 个链接，`status=passed`。
- 本地 `python -m http.server`：`/nui-flow/`、`styles.css`、`app.js`、`01-minimal.png`、`07-workbench.png` 均返回 HTTP 200。
- `git diff --check`：通过；Git 只提示现有文件的 LF/CRLF 转换 warning，没有 whitespace failure。
