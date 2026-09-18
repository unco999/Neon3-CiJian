# Neon3 CLI Debug 能力总览

状态：2026-09-18 已实现能力。

本文是 SDK 使用者的当前入口，描述已经验证的 CLI 协议能力。CLI 不是第二个业务状态所有者；它只通过 `neon3.rpc` / `neon3.event` 查询服务、发送 typed command，并返回结构化 JSON。

## 1. 已完成能力

### 通用 RPC

```powershell
neon-cli rpc <method> --endpoint <host:port> `
  [--service <service>] `
  [--params-json '{...}'] `
  [--idempotency-key <key>]
```

自动根据 method 选择 envelope `target`，也可以用 `--service` 显式覆盖。

已验证路由包括：

```text
ui.*                    -> ui-runtime
wgpu.* / render.*       -> wgpu-runtime
event.*                 -> eventd
editor.*                -> editor-runtime
project.* / asset.*     -> neon-projectd
terrain.*               -> neon-terrain-runtime
resource.*              -> neon-resource-runtime
debug.trace/command.*  -> ui-runtime
```

### Manifest 聚合观察

```powershell
neon-cli debug snapshot
neon-cli debug snapshot --manifest .neon/manifest.json
neon-cli debug snapshot --manifest .neon/manifest.json --service ui-runtime
```

每个服务包含：

- endpoint、PID、manifest epoch
- `service.health`
- `service.describe`
- 适配的 snapshot method
- request ID、revision、result、error

顶层 `status` 只有所有已配置服务的 health、describe 和 snapshot 都成功时才是 `passed`。服务断线或方法失败会返回 `failed`，不会静默忽略。

manifest 支持：

```json
{
  "services": {
    "ui-runtime": { "endpoint": "127.0.0.1:39102", "epoch": 1 },
    "wgpu-runtime": { "endpoint": "127.0.0.1:39103", "epoch": 1 }
  }
}
```

也兼容 `ui_endpoint`、`wgpu_endpoint`、`eventd_endpoint` 等旧字段。

### Snapshot diff

```powershell
neon-cli debug snapshot 127.0.0.1:39103 --diff before.json
```

输出 `changed_paths` 和每个字段的 `from` / `to`。比较在 CLI 侧执行，不修改服务状态。

### Revision wait

```powershell
neon-cli debug wait --ep 127.0.0.1:39103 --revision +1 --timeout 2s
neon-cli debug wait --ep 127.0.0.1:39103 --revision 6 --timeout 500ms
```

这是 AI/SDK 替代固定 `sleep` 的标准接口：

- 条件满足：退出码 `0`
- 有界超时：输出 `timeout: true`，退出码 `2`
- transport/protocol failure：退出码 `1`

### 请求结果与 trace 查询

```powershell
neon-cli debug command get 127.0.0.1:39103 <request-id>
neon-cli debug trace query 127.0.0.1:39103 '{"request_id":"<request-id>"}'
```

这两个接口用于沿着 `request_id -> command receipt -> trace` 定位一次操作，不需要解析人类日志。

### 语义输入激活

```powershell
neon-cli debug input activate 127.0.0.1:39103 root/footer/save_btn
```

它发送现有的 `debug.window.input.activate_target`，参数是语义节点路径，不是坐标。服务返回的结果中可以包含 `hit_target`、accepted 状态和 request ID。CLI 不负责猜测坐标，也不绕过 WGPU runtime 的输入所有权。

### 既有专用命令

```powershell
neon-cli debug interaction get <endpoint> <interaction-id>
neon-cli debug interaction query <endpoint> '{"limit":10}'
neon-cli debug render capture <endpoint> output.png
neon-cli debug world-ui capture <endpoint> output.png
neon-cli event snapshot <eventd-endpoint>
neon-cli event subscribe <eventd-endpoint> nui.variable.
```

## 2. 推荐 SDK 调试流程

```text
1. 读取 manifest
2. debug snapshot --manifest manifest.json
3. rpc <typed-method> ...
4. 保存响应里的 request_id
5. debug wait --revision +1 --timeout 2s
6. debug snapshot <endpoint> --diff before.json
7. debug command get / debug trace query --request_id <id>
```

所有中间结果都应作为 JSON 保存；不要依赖屏幕像素、日志文本或固定等待时间。

## 3. 已验证入口

```powershell
cargo test -p neon-cli
cargo run -p neon-cli --bin rpc_route_probe
cargo run -p neon-cli --bin snapshot_aggregate_probe
cargo run -p neon-cli --bin ui_observation_probe
cargo run -p neon-cli --bin debug_wrapper_probe
cargo run -p neon-cli -- scenario ui.static-fragment.submit.v1 --headless
```

探针使用现有 length-prefixed JSON RPC，并输出 JSONL，包括 sequence、method、target、request ID、revision、frame pairing 和 pass/fail。

## 4. 暂缓能力

以下功能当前不阻塞 SDK 查询和确定性 debug 闭环：

- `debug trace follow` 无限长连接包装。SDK 目前使用 bounded `debug.trace.query`，避免 CLI 引入第二套事件传输。
- `debug input` 的坐标 click/hover/key 语法糖。当前优先使用语义节点激活，符合 Neon3 输入所有权约束。
- OCR、`--contains-text` 和 PNG golden image assert。UI 文本优先从 fragment snapshot 读取。
- 无限 `snapshot --watch` 和 capture `--on-change`。SDK 可自行控制轮询生命周期，避免 CLI 进程无法有界退出。
- `dev up/status/logs/restart/down/ps`。下一阶段实现统一 supervisor 和 manifest 持久化写入。
- YAML scenario runner、Python/Node/Rust SDK thin wrapper。它们属于发布和自动化层，不影响当前公开 RPC 查询能力。

## 5. 当前验收层级

```text
contract-ready: 通过
service-ready: 通过
gpu-ready: 已有 headless scenario 证据
composition-ready: 现有 headless render graph 证据
wgpu-rendered: 本轮未宣称最终窗口像素验收
interactive-accepted: 未宣称
```
