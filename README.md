# Neon3

<p align="center">
  <a href="README.md"><strong>中文</strong></a> ·
  <a href="README.en.md">English</a>
</p>

<p align="center"><strong>独立 UI 渲染 Runtime、进程架构与公共协议</strong><br />
窗口与 GPU 由 Neon3 Runtime 统一负责；应用可以使用任意语言通过公共协议提交 UI、状态和交互。</p>

<p align="center"><img src="readme.png" width="1120" alt="Neon3 Runtime 与协议总览" /></p>

## 从案例开始

| 案例 | 用途 | 启动 |
| --- | --- | --- |
| [Neon3 背包案例](https://github.com/unco999/Neon3-example) | Python / Node.js：背包、拖拽、容量切换、真实 runtime probe | [查看并运行](https://github.com/unco999/Neon3-example#快速开始) |
| `component-gallery` | Rust：完整控件、DataGrid、Tooltip、拖拽、下拉框和世界 UI smoke test | `cargo run -p neon-dev -- case component-gallery --show-logs` |

```powershell
cargo build -p neon-projectd -p neon-eventd -p neon-ui-runtime -p neon-wgpu-runtime -p neon-dev --bins
cargo run -p neon-dev -- case component-gallery --show-logs
```

![component-gallery](docs/media/component-gallery/component-gallery.gif)

## SDK 与客户端

| 客户端 | 状态 | 链接 |
| --- | --- | --- |
| Python SDK | 已发布 | [PyPI: neon3-sdk](https://pypi.org/project/neon3-sdk/) · [源码](https://github.com/unco999/Neon3Sdk) |
| Node.js SDK | 已发布 | [npm: @neon3/sdk](https://www.npmjs.com/package/@neon3/sdk) · [源码](https://github.com/unco999/Neon3Sdk) |
| Rust Client SDK | 开发中 | [Neon3 SDK](https://github.com/unco999/Neon3Sdk) |
| C / C++ DLL SDK | 开发中 | [Neon3 SDK](https://github.com/unco999/Neon3Sdk) |

Python 和 Node.js SDK 默认解析并下载 GitHub Releases 的最新 Windows runtime。安装 SDK 后直接运行案例即可；需要复现指定版本时再设置 `NEON3_RUNTIME_VERSION`。

## 仓库

- [Neon3 Runtime](https://github.com/unco999/Neon3-CiJian)
- [Neon3 SDK](https://github.com/unco999/Neon3Sdk)
- [Bevy NUI Plugins](https://github.com/unco999/bevy-nui-plugins)
- [Neon3 Examples](https://github.com/unco999/Neon3-example)

## 构建

```powershell
cargo build
cargo test --workspace
```

要求：Rust `1.85+`，当前 release bundle 面向 Windows x86_64。

## 许可证

MIT 或 Apache-2.0。
