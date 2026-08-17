# Neon3 事件模块（neon-eventd）设计

本文档定义独立事件中转服务 `neon-eventd`：各模块通过事件专用协议向它发布事件，
它统一校验、编号并按订阅广播。先读 `AGENTS.md`，再读本文档。

`neon-eventd` 是无窗口、无 wgpu 的独立服务。它不做领域判断、不携带业务真相、
不产生权威状态；它只负责"谁发了什么事件、谁在听"。

## 1. 定位与边界

### 1.1 为什么需要它

- NUI Flow 的 `input` 变量在 UI 操作中会被改变（slider、checkbox、drag 等）。
  变量变动是典型的**跨模块观测事件**：多个 surface、AI client、领域 runtime
  都想监听，但 UI runtime 不知道也不应该知道"谁关心"。
- 现有 RPC 是 request/response，语义是"你执行，我确认"；事件语义是"发生了什么，
  通知一下"。两者方向、速率、容错模型不同。
- 统一中转保证：事件名 schema 化、全局单调序号、单一广播源、可审计、可重放，
  而不是每对进程各写一套点对点通知。

### 1.2 与 sessiond / 其他服务的关系

| 服务 | 职责 | 是否事件模块 |
| --- | --- | --- |
| `neon-sessiond`（可选） | 服务发现、监督、capability 注册 | 否，它注册"谁活着" |
| `neon-eventd` | 事件统一中转、订阅、广播、重放 | 是 |
| 各领域 runtime | 权威业务状态 | 否，它们只发布/订阅 |

`neon-eventd` 不代替 `neon-sessiond`，也不代替服务 snapshot 发布。领域状态变化仍由
领域 runtime 通过自己的 revisioned snapshot 发布；`neon-eventd` 广播的是**通知事件**
（含 UI 变量变动），接收方不能把事件 payload 当作权威状态，需要权威值必须去拉
对应服务的 snapshot。

### 1.3 允许与禁止

允许：

- 任何客户端发布 schema 化事件、订阅事件流、按需重放。
- 事件 payload 携带结构化值（变量 key、新值、模块名、来源 surface 等）。
- 在 RPC 边界内协商共享内存 fast path 并降级回 TCP。

禁止：

- 事件 payload 携带 GPU handle、renderer hit ID、element ID、文件路径、
  内存地址、原始指针或原始文本帧。
- 任何进程把"收到事件"当作修改自己权威状态的依据；要改状态必须走 typed command。
- 事件名使用自由文本、动态拼接或未注册 schema。
- 事件模块访问项目文件、创建 wgpu 资源或执行领域逻辑。

## 2. 进程图

```text
 ui-runtime / terrain-runtime / resource-runtime / projectd / wgpu-runtime / cli / AI
        |                                                              |
        |  event.publish { name, payload }                            |  event.subscribe { filters }
        |  （TCP JSON，默认）                                          |
        v                                                              v
   +-----------------------------------------------------------------------+
   | neon-eventd                                                           |
   |   schema registry（事件名白名单）                                     |
   |   global sequence 分配（epoch + 单调序号）                            |
   |   订阅表（filter -> subscriber set）                                  |
   |   环形事件缓冲（有限 retention，支持重放）                            |
   |   fast path 协商（SPSC ring）                                        |
   +-----------------------------------------------------------------------+
        |
        |  广播（TCP push，或 fast path ring）
        v
 所有匹配订阅者
```

`neon-eventd` 是唯一广播源，也是唯一的事件序号分配者。事件到达顺序以
`neon-eventd` 分配的 sequence 为准，不以发布者本地时间为准。

## 3. 事件模型

### 3.1 事件名（稳定点分名称）

事件名必须符合 `^[a-z][a-z0-9]*(\.[a-z0-9_-]+)+$`，小写点分。V1 保留命名空间：

```text
nui.variable.changed          # NUI Flow 输入变量变动
nui.intent.issued             # UI 语义 intent 已产生（可选观测）
terrain.tool.changed          # 地形工具模式变化（通知，非权威）
terrain.preview.state         # 地形 preview/commit 状态通知
resource.import.progress      # 资源导入进度
project.opened                # 项目已打开
project.closed                # 项目已关闭
service.up                    # 某服务已上线
service.down                  # 某服务已下线/退出
camera.pose.changed           # 相机位姿变化（全局共享）
selection.changed             # 全局选择集合变化
```

命名空间前缀是 schema 边界：事件名在发布前必须已注册或属于已注册前缀。
**严格模式默认开启**：未注册事件名直接拒绝，返回 `event_unknown_name`；测试/调试
可显式开启宽松模式（只打诊断不拒绝）。宽松模式必须显式声明，不能通过配置项静默
变成全局默认。

### 3.2 事件包络（EventEnvelope）

`neon-eventd` 分发给订阅者的事件是完整包络，发布者只提供内容字段：

```json
{
  "protocol": "neon3.event",
  "version": { "major": 1, "minor": 0 },
  "event_id": "uuid",
  "name": "nui.variable.changed",
  "schema_version": 1,
  "epoch": 7,
  "sequence": 10042,
  "timestamp_unix_ms": 0,
  "publisher": { "kind": "ui_runtime", "instance_id": "uuid", "pid": 1234 },
  "payload": {
    "module": "terrain_workbench",
    "surface": "surface.editor.terrain",
    "variable_key": "brush_size",
    "kind": "i32",
    "old_value": 4,
    "new_value": 8
  }
}
```

字段规则：

- `event_id`：事件全局唯一 ID（发布者生成，用于幂等）。
- `name` / `schema_version`：稳定点分名 + payload 版本。
- `epoch`：`neon-eventd` 重启纪元；客户端看到 epoch 变化必须丢弃旧游标重新订阅。
- `sequence`：`neon-eventd` 分配的全局单调序号。
- `publisher`：复用 `ClientIdentity`，唯一例外是事件模块自己（`kind=eventd`）。
- `payload`：结构化 JSON 值，必须通过对应 schema 的校验（若该 schema 声明了
  校验规则）。

### 3.3 变量变动事件 payload（首版固定 schema）

```json
{
  "schema": "nui.variable.changed@1",
  "fields": {
    "module": "string",        // 逻辑模块名，如 terrain_workbench
    "surface": "string",       // 来源 surface 的稳定 key
    "variable_key": "string",  // 变量名，与 Flow input key 一致
    "kind": "string",          // bool|i32|u32|f32|text|enum
    "old_value": "any",        // 可选
    "new_value": "any",
    "enum_choices": "string[]" // kind=enum 时可选
  }
}
```

注意：变量变动事件是**观测通知**。UI runtime 声明变量绑定变化时发布；领域 runtime
如果关心某个变量，应订阅事件后自行发起 typed command 或拉 snapshot，
而不是直接信任 payload 里的 `new_value` 改写自身状态。

## 4. 事件专用协议（neon3.event）

事件协议与 RPC 协议分离语义，但复用同一传输栈（length-prefixed JSON over
loopback TCP）。`neon-eventd` 监听一个端口，帧内 `protocol` 字段分发：

```json
{ "protocol": "neon3.rpc", ... }    // 控制方法：service.*、event.snapshot、fastpath 协商
{ "protocol": "neon3.event", ... }  // 事件方法：event.publish / event.subscribe / ...
```

### 4.1 event.publish

发布者发送：

```json
{
  "protocol": "neon3.event",
  "version": { "major": 1, "minor": 0 },
  "kind": "publish",
  "request_id": "uuid",
  "publisher": { "kind": "ui_runtime", "instance_id": "uuid", "pid": 1234 },
  "name": "nui.variable.changed",
  "schema_version": 1,
  "payload": {
    "module": "terrain_workbench",
    "surface": "surface.editor.terrain",
    "variable_key": "brush_size",
    "kind": "i32",
    "old_value": 4,
    "new_value": 8
  },
  "idempotency_key": "uuid"
}
```

成功响应：

```json
{
  "protocol": "neon3.event",
  "kind": "publish_ack",
  "request_id": "uuid",
  "status": "accepted",
  "event_id": "uuid",
  "epoch": 7,
  "sequence": 10042
}
```

拒绝响应（错误码见第 8 节）：

```json
{
  "protocol": "neon3.event",
  "kind": "publish_ack",
  "request_id": "uuid",
  "status": "rejected",
  "error": { "code": "event_unknown_name", "message": "事件名未注册", "event_id": "uuid" }
}
```

规则：

- `event.publish` 是 fire-and-forget 语义 + 可靠确认：接受即分配 sequence 并入
  环形缓冲；不等待订阅者处理结果。
- 幂等：相同 `(publisher, idempotency_key)` 的重复发布只入缓冲一次，返回原
  `event_id`/`sequence`。
- 事件名未注册、payload schema 校验失败、payload 超过大小上限，返回稳定错误码。

### 4.2 event.subscribe

订阅者发送：

```json
{
  "protocol": "neon3.event",
  "kind": "subscribe",
  "request_id": "uuid",
  "client": { "kind": "cli", "instance_id": "uuid", "pid": 1 },
  "filters": [
    { "name_prefix": "nui.variable." },
    { "publisher_kinds": ["terrain-runtime"] },
    { "name": "project.opened" }
  ],
  "replay_from_sequence": null,
  "max_rate_hz": 0
}
```

订阅流（同一 TCP 长连接，eventd 持续 push）：

```json
{
  "protocol": "neon3.event",
  "kind": "event",
  "event": { ...EventEnvelope... }
}
```

规则：

- 过滤器是 OR 语义（匹配任意一条即投递）；单条内是精确匹配。
- `replay_from_sequence` 非空时先重放该序号之后的缓冲事件，再进入实时流。
- 订阅建立时 eventd 先返回快照，再开始流式投递。
- 客户端断开即取消订阅，不保留跨连接订阅状态。

### 4.3 event.unsubscribe / event.heartbeat

```json
{ "protocol": "neon3.event", "kind": "unsubscribe", "request_id": "uuid" }
{ "protocol": "neon3.event", "kind": "heartbeat", "request_id": "uuid" }
```

长连接空闲超过 `subscriber_idle_timeout`（默认 30s）时 eventd 发 heartbeat；
客户端无响应则断开该订阅。发布连接不受此限制。

## 5. 顺序、幂等、重连与恢复

- 全局顺序：sequence 由 `neon-eventd` 单调分配，跨发布者全局有序。
- 每订阅者顺序：投递顺序即 sequence 顺序；客户端必须按 sequence 消费，
  丢弃重复序号。
- 重连：客户端先 `event.snapshot`（RPC）拿 `{ epoch, current_sequence }`，
  再 `event.subscribe` 带 `replay_from_sequence=current_sequence`，
  补齐断线期间事件。
- epoch 变化：客户端收到新 epoch 必须丢弃旧游标、清空本地缓冲、
  重新订阅（可继续 `replay_from_sequence=0` 或按业务决定）。
- 发布者重连：事件携带发布者 identity；接收方遇到同一个 `event_id` 重复投递
  应去重（按 event_id）。

## 6. 高速接口（fast path）

默认路径是 TCP JSON，足够变量变动这类低中频事件。仅当实测 p95/p99 延迟或吞吐
不满足目标时，才启用共享内存 fast path。

### 6.1 协商

fast path 通过 RPC 协商，不在事件协议内硬编码：

```text
event.fastpath.open { publisher_identity, record_size, capacity }
  -> { ring_name, schema_version, capacity, record_size, producer_epoch, max_payload }
event.fastpath.close { ring_name }
event.fastpath.status { ring_name }
```

### 6.2 Ring 布局

- 每个发布者 -> eventd 一条 SPSC ring（writer=发布者，reader=eventd）。
- record 固定大小，`#[repr(C)]` ABI，显式字节序与对齐：

```text
event_id: u128
sequence: u64            # 由 eventd 回填？否——发布者写 0，eventd 消费时分配
request_id_lo: u64       # 预留；V1 为 0
request_id_hi: u64
name_hash: u32           # 事件名 hash（发布者计算，eventd 校验）
schema_version: u16
publisher_kind: u16
payload_size: u32
payload_inline: [u8; N]  # 固定上限，V1 N=512
reserved: [u8; M]        # 固定零填充
```

- `payload_size <= 512` 内联；更大 payload 的事件拒绝进 fast path，降级 TCP。
- 每个 ring header 含 magic、schema_version、capacity、producer_epoch、
  consumer_epoch、write/read_sequence、overflow_count、closed_flag
  （对齐 AGENTS.md 26.3 契约）。

### 6.3 溢出与降级

- 发布者写满 ring：丢弃该 record 并递增 `overflow_count`；事件**不得静默丢失**——
  发布者必须在下一次 RPC 或独立 TCP 通道补发该事件，或在 `event.publish`
  携带 `was_dropped_from_fastpath=true` 让 eventd 补录。
- 消费者（eventd）发现 sequence gap 或 overflow_count 增加，发出稳定的
  `event_fastpath_overflow` 诊断并切换到 TCP 批处理一段时间。
- ring 只是传输加速，不改变事件顺序、编号或 retention 语义。

### 6.4 适用场景

高频变量流（连续拖拽 brush_size、gizmo 参数、实时曲线编辑）适合 fast path；
低频状态通知（tool 切换、项目开关）默认走 TCP。发布者按事件名选择路径，
同一个订阅者可能同时通过 TCP 与 ring 收到事件，必须按 sequence 合并去重。

## 7. 服务方法（RPC 控制面）

`neon-eventd` 支持 AGENTS.md 第 11 节基础方法：

```text
service.health
service.describe
service.subscribe        # 现有 polling 语义，平稳迁移期保留，供不适用长连接的客户端
service.shutdown
```

事件专属控制方法：

```text
event.snapshot           # { epoch, current_sequence, registered_namespaces }
event.schema.register    # 注册/更新事件名与 payload schema
event.schema.get         # 查询事件 schema
event.schema.list
event.retention.get      # 环形缓冲大小、保留条数、TTL
event.retention.set
event.fastpath.open      # 见第 6 节
event.fastpath.close
event.fastpath.status
event.stats              # 发布/订阅/丢弃/溢出计数
```

`event.schema.register` 只在 schema 所有者（对应 runtime crate）发布时使用；
普通客户端不得随意注册事件名。

## 8. 错误码

```text
event_unknown_name        事件名未注册或不属于任何已注册前缀
event_schema_mismatch     schema_version 与已注册版本不一致
event_schema_invalid      payload 未通过 schema 校验
event_payload_too_large   payload 超过上限（默认 64 KiB）
event_duplicate_ignored   相同 (publisher, idempotency_key) 已接受过
event_subscribe_invalid   过滤器为空或非法
event_replay_unavailable  请求的 sequence 早于环形缓冲起点
event_fastpath_unavailable  fast path 未开启/容量不足/版本不匹配
event_epoch_changed       客户端使用了过期 epoch
```

错误响应必须同时包含稳定 `code`、可读 `message`、`request_id` 与相关
`event_id`/`sequence`。禁止 silent no-op。

## 9. 变量变动标准流程

```text
用户拖动 slider / 勾选 checkbox
  -> wgpu-runtime 本地 hit-test 与即时视觉反馈（Layer 1，不跨进程）

控件提交语义 intent（如 settings.brush_size.commit）
  -> ui-runtime 校验并更新本地 UI 变量 brush_size=8
  -> ui-runtime -> eventd: event.publish { nui.variable.changed,
       payload { module, surface, variable_key=brush_size, kind=i32,
                 old_value=4, new_value=8 } }
  -> eventd 分配 sequence，广播给匹配订阅者

订阅者（如 terrain-runtime / AI client / 其他 surface）收到事件：
  -> 如需权威值，拉取对应服务 snapshot 或发起 typed command
  -> 不得直接信任 payload 改写权威状态
```

关键约束：**变量变动事件是观测通知，不是业务命令**。UI runtime 不通过事件模块
模拟任何领域 mutation；领域 runtime 不把事件当权威输入。这一条与 AGENTS.md
第 4、5 节边界一致。

## 10. 验收标准

分层验收沿用 AGENTS.md 第 21 节：

```text
contract-ready
  事件协议 schema round-trip 测试
  未知事件名 / schema 不匹配 / payload 过大 被稳定拒绝
  publish 幂等测试（同 idempotency_key 只入一次）
  订阅过滤、replay、断线重连补齐测试
  fast path ABI layout、wraparound、overflow、epoch mismatch 测试

service-ready
  headless 场景：ui-runtime 发布变量变动 -> eventd 编号 ->
  订阅者按 sequence 收到；事件名、payload、sequence 可断言

gpu-ready / composition-ready / wgpu-rendered
  不适用（eventd 不触碰渲染）

interactive-accepted
  人类在真实工作流中确认：拖拽 slider 时其他模块能实时收到变量变动
```

建议 fixture：

```text
tests/fixtures/events/
  variable-change-request.json
  variable-change-ack.json
  subscribe-filter.json
  replay.json
```

## 11. 实施步骤

1. `neon-protocol` 增加事件协议类型：`EventEnvelope`、`EventPublish`、
   `EventSubscribe`、`EventAck`、`EventError`、`EventSnapshot`、过滤器结构，
   与 RPC 类型并列但独立枚举。
2. `neon-ipc` 增加帧分发：连接首帧按 `protocol` 字段路由到 RPC 或事件处理器。
3. 新建 `neon-eventd` crate：事件注册表、全局 sequence、订阅表、环形缓冲、
   RPC 控制面与事件服务端。
4. `neon-ui-runtime` 在变量绑定变更处调用事件发布（先走 TCP）。
5. fast path 按第 6 节实现并完成 ABI/overflow/降级测试。
6. `neon-cli` 支持 `neon event subscribe --name nui.variable.*` 等调试命令。

## 12. 已确认决策

以下决策已在设计评审中定案：

| 决策点 | 结论 |
| --- | --- |
| 事件名严格模式 | **默认开启**，未注册事件名直接拒绝（`event_unknown_name`）；宽松模式需显式声明 |
| 环形缓冲默认保留 | **4096 条**，内存受限时可调小，`event.retention.get/set` 可查询与调整 |
| 旧 RPC polling 订阅 | **保留**，`service.subscribe` 平稳迁移期继续提供，事件长连接可用后仍不删除 |
| 订阅端 fast path | **首版不走订阅端 ring**，订阅统一由 eventd fan-out 后走 TCP；订阅端 ring 留到 V2 |
| `service.subscribe` 与事件流关系 | 两条路径并存期间，两者投递同一事件必须保证 sequence 一致，客户端按 sequence 去重合并 |

实施时以本表为准；如需变更必须回到设计评审重新确认，不能静默调整。
