---
date: 2026-08-22
topic: 清理 neon-gpu 旧计算链路
type: implementation
---

## 涉及的 crate / 文件路径

- `crates/neon-gpu`
- `crates/neon-gpu-exec`
- `crates/neon-gpu-script`
- `Cargo.toml`
- `Cargo.lock`
- `docs/neon3-gpu-script.md`
- `docs/2026-08-20-neon3-session-notes.md`

## 发现的问题

旧的 GPU 数据池、脚本编译器和执行器仍注册在 workspace 中，并在锁文件和专属文档中保留引用。

## 采取的方案

删除三个 crate 的源码、测试、crate 级 AGENTS.md 及其专属文档；从 workspace 成员和 `Cargo.lock` 中移除。

## 当前状态

已完成。未创建新的通用逻辑计算模块，等待后续设计。

## 未完成事项与下一步

暂无实现事项。后续根据设计新建通用逻辑计算模块。

## 测试与验证结果

- `cargo metadata --locked --no-deps --format-version 1`：通过。
- `cargo check --workspace`：通过。
- 仅报告现有 `neon-wgpu-runtime` 的 10 条 warning，无编译失败。
- 在 `crates` 和 `docs` 中检索 `neon-gpu`、`neon-gpu-exec`、`neon-gpu-script`：无残留源码/文档引用。
