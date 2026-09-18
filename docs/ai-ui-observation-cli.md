# Neon3 AI 观察 UI 变化的 CLI 能力设计

> 状态：分阶段实施中。`debug snapshot` 聚合、`debug snapshot --diff`、`debug wait --revision`、`debug command get`、`debug trace query` 和语义 input activate 已实施并通过 loopback JSONL probe。当前功能总览见 `docs/neon-cli-debug-capabilities.md`。
> 目标：让 AI 能通过 CLI 
>
> **准确、确定性地**
>
> 观察 UI 状态变化，不靠 sleep、不靠视觉模型、不靠手 diff JSON。
> 关联：
>
> `docs/neon-cli-sdk-debug-refactor.md`
>
> （重构计划）、
>
> `AGENTS.md`
>
>  第 17/18/22 节。



***

## 1. 问题：现在 AI 怎么 debug

当前 AI 调试一次 UI 交互的实际循环：



```
1\. neon-cli debug snapshot \<wgpu-endpoint>        → 读渲染态

2\. neon-cli rpc ui.host.inbound --params-json ...  → 发 intent

3\. sleep(500ms)                                    → 猜 UI 更新完

4\. neon-cli debug snapshot \<wgpu-endpoint>        → 再读一次

5\. AI 自己 diff 两个 JSON                          → 手工作业

6\. neon-cli debug render capture \<ep> out.png      → 截图

7\. AI 看图判断                                     → 视觉模型或人眼
```

痛点：



* **第 3 步靠 sleep**：AGENTS.md 第 22 节明确禁止 "等待固定毫秒数猜测 Ready"，但 CLI 没给别的选择。

* **第 5 步靠 AI 手 diff**：两次 snapshot 都是大 JSON，AI 要自己找 "哪个字段变了"。

* **第 7 步靠看图**：没有像素断言，只能视觉模型猜。

* **看不到 UI 语义态**：ui-runtime 的 snapshot 只返回 revision/capabilities，当前 fragment 内容要靠 wgpu-runtime 的渲染态反推。

* **事件流薄**：eventd 只发 4 种事件（shader.event/file\_drop /click\_blank/document\_commit），不发 UI 状态变化。



***

## 2. 设计目标

AI 用 CLI 完成一次 UI debug，应该长这样：



```
1\. neon-cli debug snapshot \<ep> --json > before.json

2\. neon-cli rpc ui.host.inbound --params-json ...

3\. neon-cli debug wait --ep \<ep> --revision +1 --timeout 2s

4\. neon-cli debug snapshot \<ep> --diff before.json

&#x20;  → 输出结构化 diff：哪个 surface、哪个 node、哪个字段变了

5\. neon-cli debug trace follow --request \<id>

&#x20;  → 实时看 command 从接收到完成的全过程

6\. neon-cli debug capture \<ep> out.png

7\. neon-cli debug assert out.png --region 100,100,200,50 --contains-text "Save"
```

关键性质：



* **确定性**：不用 sleep。`wait` 基于 revision / 事件 / 稳定条件返回。

* **结构化 diff**：不用 AI 手比较 JSON，CLI 输出字段级变化。

* **语义态可见**：ui-runtime snapshot 返回当前 fragment /host 状态。

* **可组合**：每个子命令独立可用，输出 JSONL 方便管道。



***

## 3. AI Debug 循环模型



```
&#x20;       ┌─────────────────────────────────────────┐

&#x20;       │            AI Debug Loop               │

&#x20;       └─────────────────────────────────────────┘

&#x20;                    │

&#x20;  ┌─────────────────┼─────────────────────┐

&#x20;  ▼                 ▼                     ▼

&#x20;observe()         act()               observe()

&#x20;读状态快照        发 typed command     等条件满足后读新快照

&#x20;before.json       intent/input/patch   after.json

&#x20;  │                 │                     │

&#x20;  └─────────────────┼─────────────────────┘

&#x20;                    ▼

&#x20;                diff(before, after)

&#x20;                结构化变化列表

&#x20;                    │

&#x20;                    ▼

&#x20;                trace follow

&#x20;                关联 request\_id
```

CLI 要提供的工具正好对应这五步：



| 步骤            | 子命令                                 |
| ------------- | ----------------------------------- |
| observe       | `debug snapshot`                    |
| act           | `rpc` / `ui intent` / `input click` |
| wait          | `debug wait`                        |
| observe again | `debug snapshot --diff`             |
| correlate     | `trace follow --request`            |



***

## 4. CLI 子命令设计

### 4.1 `neon-cli debug snapshot`（增强现有）



```
neon-cli debug snapshot \<endpoint> \[--service \<name>] \[--json] \[--compact]

&#x20;                                  \[--watch] \[--interval-ms 100]

&#x20;                                  \[--diff \<before-file>]
```

**行为：**



* 不带 `--service` 时：


  * 如果连的是 wgpu-runtime，返回渲染态 snapshot（现有行为）

  * 如果连的是 ui-runtime，返回 UI 语义态（需 P2 补全：fragment 列表 + host 状态）

  * 新增：`--all` 聚合 manifest 里所有服务，一次返回合并 snapshot

* `--watch`：每秒（或 `--interval-ms`）输出一行 JSON snapshot 到 stdout，直到 Ctrl+C。每行是完整 snapshot，不是增量。

* `--diff <file>`：读 before.json，比较当前 snapshot，输出结构化 diff：



```
{

&#x20; "diff": {

&#x20;   "ui\_runtime.revision": { "from": 5, "to": 6 },

&#x20;   "fragments": \[

&#x20;     {

&#x20;       "surface\_id": "surface.calculator",

&#x20;       "changes": \[

&#x20;         { "node": "root/display", "property": "text", "from": "0", "to": "3" }

&#x20;       ]

&#x20;     }

&#x20;   ]

&#x20; }

}
```

**为什么需要&#x20;**`--watch`**：** AI 订阅变化不用自己轮询。CLI 内部循环调 snapshot，stdout 输出 JSONL，AI 逐行读。

**为什么需要&#x20;**`--diff`**：** AI 不用自己写 JSON 比较。CLI 知道 snapshot schema，能做语义 diff（比如 "button.enabled 从 false 变 true" 比 "JSON 字符串不同" 有用）。

### 4.2 `neon-cli debug wait`（新增，最关键）



```
neon-cli debug wait --ep \<endpoint> \[--service \<name>]

&#x20;                   (--revision \<N> | --revision +\<delta> | --event \<name> | --stable-ms \<N> | --element-visible \<node-path>)

&#x20;                   \[--timeout \<secs>] \[--json]
```

**条件（四选一，可组合）：**



| 条件                         | 含义                                    | 实现方式                                   |
| -------------------------- | ------------------------------------- | -------------------------------------- |
| `--revision <N>`           | 等到某服务 revision 等于 N                   | 轮询 debug.snapshot.get 直到 revision 字段匹配 |
| `--revision +<delta>`      | 等到 revision 比当前增加 delta               | 先读当前，再轮询                               |
| `--event <name>`           | 等到 eventd 收到指定事件                      | event subscribe 阻塞                     |
| `--stable-ms <N>`          | 连续 N ms 内 snapshot 无变化                | 每 50ms 采样，N ms 无 diff 则返回              |
| `--element-visible <path>` | 等到指定 node 出现在 fragment 且 visible=true | 读 fragment snapshot，grep node path     |

**超时：** `--timeout 5s`（默认 10s），超时返回 exit code 2。

**输出：**



```
{

&#x20; "wait": "revision",

&#x20; "matched\_revision": 6,

&#x20; "elapsed\_ms": 42,

&#x20; "timeout": false,

&#x20; "final\_snapshot": { ... }

}
```

**为什么这是最关键的：** 它替代了所有 `sleep`。AI 发完 intent 后不用猜 "等多久"，直接 `--revision +1` 等到 UI 更新完。AGENTS.md 第 22 节的 "不允许 sleep" 有了落地工具。

### 4.3 `neon-cli debug input`（坐标 / 键盘操作封装）



```
neon-cli debug input click \<x> \<y> \[--ep \<endpoint>]

neon-cli debug input hover \<x> \<y> \[--ep \<endpoint>]

neon-cli debug input key \<key> \[--modifiers ctrl|shift|alt] \[--ep \<endpoint>]
```

**实现：** 封装现有 `debug.window.input.activate_target` / `hover_target` / `ui.host.keyboard_event`。

**输出：**



```
{

&#x20; "input": "click",

&#x20; "position": \[100, 200],

&#x20; "hit\_target": "root/button/save",

&#x20; "request\_id": "...",

&#x20; "accepted": true

}
```

**关键：** 返回 `hit_target`（点中了哪个语义节点）。AI 不用自己猜坐标对应哪个按钮。

### 4.4 `neon-cli debug trace follow`（实时 trace）



```
neon-cli debug trace follow --ep \<endpoint> \[--request \<request-id>]

&#x20;                          \[--service \<name>] \[--level info|warn|error]

&#x20;                          \[--event-prefix \<prefix>]
```

**行为：** 调 `debug.trace.subscribe`，实时输出 JSONL：



```
{"sequence":42,"service":"ui-runtime","event":"ui.host.inbound.received","request\_id":"...","revision\_before":5}

{"sequence":43,"service":"ui-runtime","event":"ui.flow.patched","request\_id":"...","revision\_after":6}

{"sequence":44,"service":"wgpu-runtime","event":"ui.fragment.committed","request\_id":"...","frame":1234}
```

`--request <id>`**：** 只输出指定 request\_id 的 trace，AI 发完 command 后拿着 request\_id 看全过程。

### 4.5 `neon-cli debug capture`（增强现有）

现有：



```
neon-cli debug render capture \<ep> out.png
```

新增：



```
neon-cli debug capture \<ep> out.png \[--wait-idle] \[--on-change]
```



* `--wait-idle`：等到 `--stable-ms 200` 后再截。避免截到动画中间帧。

* `--on-change`：watch snapshot，变化后截一张。

### 4.6 `neon-cli debug assert`（像素 / 语义断言）



```
neon-cli debug assert \<png> --region \<x,y,w,h> --color "#rrggbb" \[--tolerance 0.05]

neon-cli debug assert \<png> --contains-text "Save" \[--region ...]

neon-cli debug assert \<png> --not-blank \[--region ...]
```

**行为：**



* `--color`：读 PNG，检查 region 平均色是否匹配（需要 CLI 带个 PNG 解码依赖，比如 `image` crate）

* `--contains-text`：OCR（这个重，第一版可以不做，用 wgpu-runtime 的 fragment snapshot 代替 —— 直接查节点 text 字段）

* `--not-blank`：检查 region 不是纯色（防黑屏）

**输出：** exit code 0 = passed，1 = failed。

### 4.7 `neon-cli debug ui`（UI 语义操作）



```
neon-cli debug ui snapshot \<ep>                    # ui-runtime 语义态

neon-cli debug ui fragment \<ep> \[--surface \<id>]   # 当前 fragment 内容

neon-cli debug ui host \<ep>                        # host 状态

neon-cli debug ui intent \<intent> \[--source \<key>] \[--payload-json ...]
```

这些是 `rpc` 逃生舱的语法糖，让 AI 不用拼 JSON envelope。



***

## 5. 一次完整的 AI debug 会话示例

场景：点 "Save" 按钮，预期保存成功，snapshot 里 saved=true。



```
EP\_UI=127.0.0.1:52342

EP\_WGPU=127.0.0.1:52343

\# 1. 读初始状态

neon-cli debug snapshot \$EP\_UI --json > before.json

\# 2. 找 Save 按钮位置（从 fragment snapshot 里查 node path）

neon-cli debug ui fragment \$EP\_UI | jq '.nodes\[] | select(.kind=="button" and .text=="Save") | .path'

\# → "root/footer/save\_btn"

\# 3. 点击（CLI 自动查 node 的屏幕坐标）

neon-cli debug input click --ep \$EP\_WGPU --node "root/footer/save\_btn"

\# 4. 等 UI 更新（不靠 sleep）

neon-cli debug wait --ep \$EP\_UI --revision +1 --timeout 2s

\# 5. 读新状态，和 before.json diff

neon-cli debug snapshot \$EP\_UI --diff before.json

\# → {"diff": {"nodes.root/footer/save\_btn.saved": {"from":false,"to":true}}}

\# 6. 看这次操作的 trace

neon-cli debug trace follow --ep \$EP\_UI --request \<request\_id-from-step-3>

\# 7. 截图存证

neon-cli debug capture \$EP\_WGPU after.png --wait-idle

\# 8. 断言不是黑屏

neon-cli debug assert after.png --not-blank
```



***

## 6. 与现有 RPC method 的映射



| CLI 子命令                        | 调用的 RPC method                                      | 现状             |
| ------------------------------ | --------------------------------------------------- | -------------- |
| `debug snapshot`               | `debug.snapshot.get`                                | ✅ 已有           |
| `debug snapshot --watch`       | 轮询 `debug.snapshot.get`                             | 纯 CLI 侧，服务端不用改 |
| `debug snapshot --diff`        | 两次 `debug.snapshot.get` + CLI 侧 diff                | 纯 CLI 侧        |
| `debug wait --revision`        | 轮询 `debug.snapshot.get`                             | 纯 CLI 侧        |
| `debug wait --event`           | `event.subscribe`                                   | ✅ 已有           |
| `debug wait --stable-ms`       | 轮询 snapshot + diff                                  | 纯 CLI 侧        |
| `debug wait --element-visible` | `wgpu.ui.fragment.snapshot`                         | ✅ 已有           |
| `debug input click/hover`      | `debug.window.input.activate_target/hover_target`   | 服务端已有，CLI 没封装  |
| `debug input key`              | `ui.host.keyboard_event`                            | 服务端已有，CLI 没封装  |
| `debug trace follow`           | `debug.trace.subscribe`                             | 服务端已有，CLI 没封装  |
| `debug capture --wait-idle`    | `debug.snapshot.get` + `wgpu.render.target.capture` | 组合现有           |
| `debug assert --color`         | PNG 解码（CLI 侧）                                       | 新 CLI 依赖       |
| `debug assert --not-blank`     | PNG 解码（CLI 侧）                                       | 新 CLI 依赖       |
| `debug ui intent`              | `ui.host.inbound`                                   | 语法糖            |

**关键观察：** 大部分工具**不需要改服务端**。`watch` / `diff` / `wait --revision` / `--stable-ms` / `--wait-idle` 都是 CLI 侧组合现有 method。只有 `debug.trace.subscribe` 和 `debug.window.input.*` 需要服务端已存在（已存在）。



***

## 7. 数据契约

### 7.1 snapshot JSON schema（v1）

所有服务的 `debug.snapshot.get` 响应应该包含：



```
{

&#x20; "service": "ui-runtime",

&#x20; "epoch": 1,

&#x20; "revision": 6,

&#x20; "health": "healthy",

&#x20; "capabilities": \[...],

&#x20; "fragments": \[

&#x20;   {

&#x20;     "surface\_id": "surface.calculator",

&#x20;     "revision": 3,

&#x20;     "sequence": 42,

&#x20;     "program\_revision": 7,

&#x20;     "nodes": \[

&#x20;       { "path": "root/display", "kind": "text", "text": "3", "visible": true, "enabled": true }

&#x20;     ]

&#x20;   }

&#x20; ],

&#x20; "host": {

&#x20;   "input\_revision": 3,

&#x20;   "active\_interaction": null,

&#x20;   "bound\_nodes": {...}

&#x20; }

}
```

**注意：** ui-runtime 现在的 snapshot **没有&#x20;**`fragments`**&#x20;和&#x20;**`host`**&#x20;字段**。这是 P2 任务，必须补。

### 7.2 diff JSON schema（v1）

`debug snapshot --diff before.json` 输出：



```
{

&#x20; "diff": {

&#x20;   "revision": { "from": 5, "to": 6 },

&#x20;   "changed\_paths": \[

&#x20;     "fragments\[surface.calculator].nodes\[root/display].text"

&#x20;   ],

&#x20;   "nodes": \[

&#x20;     {

&#x20;       "path": "root/display",

&#x20;       "changes": \[

&#x20;         { "property": "text", "from": "0", "to": "3" }

&#x20;       ]

&#x20;     }

&#x20;   ],

&#x20;   "added\_nodes": \[...],

&#x20;   "removed\_nodes": \[...]

&#x20; }

}
```

CLI 做 diff 的算法：



1. 递归比较两个 snapshot JSON

2. 叶子值变化记录 in `nodes[].changes`

3. 数组（nodes）按 path 索引，新增 / 删除分别记录

4. 不 diff `timestamp` / `request_id` 这类每次都变的字段（维护一个 ignore list）

### 7.3 wait 超时 JSON



```
{

&#x20; "wait": "revision",

&#x20; "condition": "revision >= 6",

&#x20; "matched": false,

&#x20; "elapsed\_ms": 10000,

&#x20; "timeout": true,

&#x20; "last\_revision": 5,

&#x20; "hint": "revision never reached 6; check request trace with neon-cli debug trace query"

}
```



***

## 8. 实施优先级



| 顺序 | 子命令                                  | 工作量               | 依赖                 |
| -- | ------------------------------------ | ----------------- | ------------------ |
| 1  | `debug wait --revision / --timeout`  | \~150 行           | 无（纯轮询）             |
| 2  | `debug snapshot --diff`              | \~200 行           | 无（纯 JSON diff）     |
| 3  | `debug input click/hover/key`        | \~150 行           | 无（封装现有 method）     |
| 4  | `debug trace follow`                 | \~100 行           | 无（封装现有 method）     |
| 5  | ui-runtime snapshot 补 fragments/host | 改 neon-ui-runtime | P2                 |
| 6  | `debug snapshot --watch`             | \~100 行           | 依赖 5               |
| 7  | `debug capture --wait-idle`          | \~50 行            | 依赖 1               |
| 8  | `debug assert --not-blank / --color` | \~200 行           | 加 `image` crate 依赖 |
| 9  | `debug wait --element-visible`       | \~100 行           | 依赖 5               |
| 10 | `debug ui intent / fragment / host`  | \~150 行           | 语法糖                |

当前已完成：

- `debug snapshot [--manifest] [--service]`：按 manifest 聚合 health、describe 和服务 snapshot。
- `debug snapshot <endpoint> --diff <before.json>`：输出 changed paths 和 from/to 值。
- `debug wait --ep <endpoint> --revision <N|+delta> --timeout <Nms|Ns>`：按 revision 轮询，匹配返回 0，超时返回 2。
- `src/bin/ui_observation_probe.rs`：验证 snapshot producer/consumer frame pairing、diff 和 revision transition。
- `debug command get`、`debug trace query`、`debug input activate`：封装现有 command receipt、trace query 和语义节点激活 RPC。

当前下一步只保留对 SDK 查询闭环有直接价值的功能：

1. `debug ui snapshot|fragment|host` 语义查询入口，前提是 ui-runtime snapshot 补齐 fragment/host。
2. `debug trace follow` 的有限 bounded 查询；长连接订阅暂缓，避免 CLI 自己成为第二套 event transport。

暂缓：OCR、PNG `--contains-text`、golden image assert、`--watch` 无限循环、capture `--on-change`、world-ui lab 语法糖。这些不阻塞 SDK 通过结构化协议观察页面状态。



***

## 9. 不做的事



* **不做 OCR**（`--contains-text`）：太重，而且 fragment snapshot 里本来就有 text 字段，不需要 OCR。

* **不做截图像素级 AI 判断**：那是视觉模型的事，CLI 只做确定性的颜色 / 黑屏断言。

* **不做 snapshot 持久化**：AI 自己管 before.json，CLI 不存历史。

* **不做录制回放**：那是 `debug.replay.export`（AGENTS.md 第 19 节），另一个功能。

* **不让 CLI 模拟鼠标坐标**：AGENTS.md 第 17 节禁止 AI 模拟 UI 点击。CLI 的 `input click` 是**语义点击**—— 它查 node path 对应的屏幕坐标，不是 AI 自己猜坐标。

* **不做跨进程 diff**：每个服务的 snapshot 独立 diff，CLI 不合并跨服务状态。



***

## 10. 验收标准

每个子命令必须通过：



```
\# wait 能在条件满足时返回，不靠 sleep

cargo run -p neon-cli -- wait --ep \<ep> --revision +1 --timeout 2s

\# 期望：42ms 内返回，不是 2000ms

\# diff 能准确指出变化

cargo run -p neon-cli -- snapshot \<ep> --diff before.json

\# 期望：输出 nodes.root/display.text from "0" to "3"

\# trace follow 能实时输出

cargo run -p neon-cli -- trace follow --ep \<ep> --request \<id>

\# 期望：command received → validated → accepted → completed 四行 JSONL

\# input click 返回 hit\_target

cargo run -p neon-cli -- input click 100 200 --ep \<ep>

\# 期望：hit\_target = "root/footer/save\_btn"
```

最终验收：AI 用这套工具完成一次 "点击按钮 → 验证状态变化" 的 scenario，全程不出现 `sleep` 命令。

当前已达成的自动验收：

```powershell
cargo test -p neon-cli
cargo run -p neon-cli --bin ui_observation_probe
```

probe 输出 `diff_completed.pass=true` 与 `wait_completed.pass=true`，并带有 producer sequence、consumer frame pairing、revision 和 changed paths。真实服务场景仍需在 `neon-cli dev up` 产出 manifest 后完成完整点击闭环。
