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

## Android 运行

Neon3 提供可安装的 Android Host：APK 内的 `Neon3HostService` 以后台前台服务
运行，无窗口、无黑屏，在 `127.0.0.1:43100` 暴露同一个 `neon3.rpc/1` 控制端点。
Host 内部链接唯一的 `neon-wgpu-runtime`，因此 Android 与 Windows 使用同一套
渲染与共享表面纹理协议。

- 示例工程：[examples/android-runtime](examples/android-runtime/)
- Rust host crate：[crates/neon-android-host](crates/neon-android-host/)

```powershell
# 构建 Rust 库并安装（需要 Android NDK）
$env:ANDROID_HOME = "E:\AndroidSdk"
$env:ANDROID_NDK_HOME = "E:\AndroidSdk\ndk\30.0.16138531"
cargo ndk -t arm64-v8a -o examples/android-runtime/app/src/main/jniLibs build --release -p neon-android-host

# 安装 APK 并启动 Host
& "E:\AndroidSdk\platform-tools\adb.exe" install -r examples/android-runtime/app/build/outputs/apk/debug/app-debug.apk
& "E:\AndroidSdk\platform-tools\adb.exe" shell monkey -p com.neon3.androidruntime 1

# SDK 通过 adb forward 连接单端点
& "E:\AndroidSdk\platform-tools\adb.exe" forward tcp:43100 tcp:43100
```

Host 启动后日志输出一条 JSONL 记录（`adb logcat -s Neon3Probe:I`）：

```json
{"probe":"android-host-service","state":"started","result":0,"endpoint":"127.0.0.1:43100"}
```

SDK 以 Android transport 连接时不再启动本地桌面进程，全部协议（`ui.*`、
`wgpu.*`、`render.*`、`service.*`）由该单端点回答：

```python
from neon3_sdk import NeonApp

app = NeonApp.start(transport="android")             # 自动 adb 发现 + forward
app.ui.mount_flow_file("hello.nui")
app.stop()
```

Node.js 的写法等价：

```ts
import { NeonApp } from "@neon3/sdk";

const app = await NeonApp.start({ transport: "android" });  // 自动 adb 发现 + forward
await app.ui.mountFlowFile("hello.nui");
await app.stop();
```

## 共享表面纹理与图片测试

SDK 可以在不启动窗口的情况下申请跨进程共享表面纹理（Windows 为 D3D12 共享
纹理，Android 为 Vulkan/离屏 wgpu 纹理），并把最新帧保存为 PNG 用于自动化
验收。

### 申请表面纹理

```python
from neon3_sdk import RenderClient, SurfaceOpen, SurfaceSize, SurfaceKind

renderer = RenderClient(client)                # client = NeonClient 实例
surface = renderer.open_surface(SurfaceOpen(
    session_id="demo",
    surface_id="hello",
    kind=SurfaceKind.SCREEN_UI,
    size=SurfaceSize(width=1280, height=720),
    buffer_count=2,
))
```

```ts
import { RenderClient } from "@neon3/sdk";

const renderer = new RenderClient(client);      // client = NeonClient 实例
const surface = await renderer.openSurface({
  sessionId: "demo",
  surfaceId: "hello",
  kind: "screen_ui",
  width: 1280,
  height: 720,
  bufferCount: 2,
});
```

注意：申请尺寸与渲染逻辑尺寸（默认 1280x720）越接近，画面缩放越小。若使用
很小的表面（如 320x200），UI 会被等比缩小。

### 保存图片（PNG 测试）

```python
await surface.save_png("captures/surface.png")   # Python
await surface.savePng("captures/surface.png")    # Node.js
```

`render.surface.capture_png` 由 `neon-wgpu-runtime` 读回共享表面并写出 PNG，
原生 GPU 句柄不会进入 JSON。自动验收可对产物做像素/签名断言：

```python
import pathlib
png = pathlib.Path("captures/surface.png").read_bytes()
assert png[:8] == b"PNG


"          # 有效 PNG
```

在 Windows 上，这一步同时验证 D3D12 共享纹理的 readback 链路；在 Android 上
则验证 Vulkan/离屏纹理的 readback 链路。没有 GPU 导出路径的 Host 会返回稳定
错误码 `backend_not_available`，而不是 `unsupported_method`。

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
