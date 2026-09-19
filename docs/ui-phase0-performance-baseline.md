# Phase 0 UI 更新性能基线（B0-5）

本文件记录 Neon3 UI 底层优化计划 Phase 0 的实际测量基线与性能预算。
所有数字均来自可重复执行的公开协议 probe，禁止用估算值替换；后续阶段的
收益必须与本文件基线做机器比较。对应计划：
[`neon3-ui-bottom-layer-optimization-plan.md`](./neon3-ui-bottom-layer-optimization-plan.md) §2。

## 1. 测量入口

| 侧 | Probe | 命令 |
|---|---|---|
| 生产者（ui-runtime，跨进程 RPC） | `crates/neon-ui-runtime/src/bin/ui_patch_baseline_probe.rs` | `cargo run -p neon-ui-runtime --bin ui_patch_baseline_probe --release` |
| 消费者（wgpu retained renderer，offscreen） | `crates/neon-wgpu-runtime/src/bin/ui_reconcile_baseline_probe.rs` | `cargo run -p neon-wgpu-runtime --bin ui_reconcile_baseline_probe --release` |

两个 probe 都输出 JSONL 到 stdout，最后一行是
`{"probe":...,"final":true,"status":"passed|failed","failures":N,"pass":bool}`，
任一 record 检查失败即以退出码 1 结束。测量环境：Windows 10 (10.0.26200)，
consumer 侧使用 DX12 真实 adapter（`adapter.limits()`），debug 与 release
两套 profile 均已采集并分别标注。

固定场景（A-H 与生产者/消费者完全一致）：

```text
A 100 节点 1 个文本 SetProperty
B 100 节点 50 个事务状态 Set（单 patch 批量）
C 100 节点 task 列表 insert/update/remove（3 个连续 patch）
D 500 节点仅选中态 1 个 Set
E 100/1000 节点插入 1 个节点
F 100 节点删除 1 个节点
G 100 节点批量 move 重排 10 个
H 1000 节点 100 个 Set 批量
```

## 2. JSONL 机器比较格式（规范）

每条 record 以 `(probe, case)` 为主键，数值字段可直接逐项对比。

生产者 record（`probe = "ui_patch_baseline.v1"`）：

```json
{
  "probe": "ui_patch_baseline.v1",
  "case": "A_property_set_1/patch_1",
  "input": {"node_count": 100, "operation_count": 1},
  "producer": {"base_revision": 3, "patch_sequence": 1},
  "consumer": {"status": "accepted", "revision": 2, "error": null},
  "timing_ms": {"patch_apply": 0.09, "compile": 0.41, "fragment": 0.44, "forward": 2.81, "total": 4.14, "parse": 0.0, "source_read": 0.0},
  "pass": true
}
```

消费者 record（`probe = "ui_reconcile_baseline.v1"`）：

```json
{
  "probe": "ui_reconcile_baseline.v1",
  "case": "A_property_set_1/set",
  "input": {"node_count": 100, "operation_count": 1},
  "producer": {"patch_sequence": 1, "base_revision": 3, "ir_revision": 4},
  "consumer": {"fragment_revision": 4, "draw_sequence": 2},
  "timing_ms": {"draw": 15.65, "refresh_plan": 4.89},
  "retained": {"retained": 101, "created": 0, "removed": 0, "updated": 1, "moved": 0},
  "pass": true
}
```

字段约定：

- `case` 形如 `<场景>/<步骤>`；`/base` 为该场景首帧（全量创建）。
- `timing_ms` 各项为非负浮点毫秒；生产者 `total` 覆盖 patch_apply/compile/fragment/forward 全链。
- `retained.*` 为 B0-3 计数器：一次全量 plan reconcile 内 keyed diff 的结果；
  走 plan 复用 early-return 的帧不更新计数器。
- 比较方法（示例）：`jq -s 'group_by(.case) | map({case: .[0].case, best: (map(.timing_ms.total) | min)})'`；
  回归判定 = 同 profile 下任一 `timing_ms` 中位数劣化超预算或 `retained` 不变量被破坏。

## 3. 基线数据

### 3.1 生产者（ui-runtime → wgpu，release，本机实测）

| case | nodes | ops | patch_apply | compile | fragment | forward | total |
|---|---|---|---|---|---|---|---|
| A/submit | 100 | 0 | - | 0.66 | 0.63 | 2.60 | 5.16 |
| A/patch_1 | 100 | 1 | 0.09 | 0.41 | 0.44 | 2.81 | 4.14 |
| B/patch_1 | 100 | 50 | 0.12 | 0.33 | 0.31 | 2.38 | 3.49 |
| C/patch_1..3 | 100 | 1 each | 0.05-0.17 | 0.46-0.95 | 0.39-0.80 | 1.59-2.86 | 2.91-4.89 |
| D/submit | 500 | 0 | - | 1.87 | 3.54 | 9.34 | 18.22 |
| D/patch_1 | 500 | 1 | 0.58 | 2.40 | 2.52 | 8.05 | 14.69 |
| E/patch_1 | 100 | 1 | 0.08 | 0.30 | 0.28 | 1.42 | 2.31 |
| F/patch_1 | 100 | 1 | 0.21 | 1.37 | 0.45 | 1.51 | 3.82 |
| G/patch_1 | 100 | 10 | 0.15 | 0.77 | 0.39 | 2.24 | 3.94 |
| H/submit | 1000 | 0 | - | 5.07 | 12.07 | 18.81 | 43.23 |
| H/patch_1 | 1000 | 100 | 0.82 | 3.67 | 9.34 | 15.42 | 31.94 |

### 3.2 生产者（debug，用于 profile 换算参考）

单 op patch total：100 节点 ≈ 14.9 ms，500 节点 ≈ 67.7 ms，1000 节点（100 ops）≈ 131.6 ms；
全量 submit total：100 ≈ 19.4 ms，500 ≈ 70.3 ms，1000 ≈ 135.4 ms。

### 3.3 消费者（retained renderer，debug）

首次 draw 的 `timing_ms.draw`（2.7-3.9 s）是一次性 shader/pipeline 编译预热，
不计入基线；以下均为 reconcile 帧。

| case | retained | created | removed | updated | moved | refresh_plan ms |
|---|---|---|---|---|---|---|
| A/set | 101 | 0 | 0 | 1 | 0 | 4.89 |
| B/batch50 | 101 | 0 | 0 | 50 | 0 | 7.36 |
| C/insert | 101 | 1 | 0 | 0 | 0 | 5.88 |
| C/update | 102 | 0 | 0 | 1 | 0 | 4.69 |
| C/remove | 101 | 0 | 1 | 0 | 0 | 5.59 |
| D/set (500n) | 501 | 0 | 0 | 1 | 0 | 35.05 |
| E/insert (100n) | 101 | 1 | 0 | 0 | 0 | 6.50 |
| E/insert (1000n) | 1001 | 1 | 0 | 0 | 0 | 59.84 |
| F/remove (100n) | 100 | 0 | 1 | 57 | 0 | 6.38 |
| G/move10 | 101 | 0 | 0 | 100 | 100 | 6.78 |
| H/batch100 (1000n) | 1001 | 0 | 0 | 100 | 0 | 68.52 |

### 3.4 消费者（release，本机实测）

每个 case 使用全新 renderer，`/base` 的 `timing_ms.draw`（0.4-0.6 s）是
一次性 shader/pipeline 编译预热，不计入基线；下表为 reconcile 帧。

| case | retained | created | removed | updated | moved | refresh_plan ms | draw ms |
|---|---|---|---|---|---|---|---|
| A/set | 101 | 0 | 0 | 1 | 0 | 0.56 | 3.14 |
| B/batch50 | 101 | 0 | 0 | 50 | 0 | 0.69 | 3.43 |
| C/insert | 101 | 1 | 0 | 0 | 0 | 0.43 | 2.12 |
| C/update | 102 | 0 | 0 | 1 | 0 | 0.44 | 2.14 |
| C/remove | 101 | 0 | 1 | 0 | 0 | 0.41 | 2.40 |
| D/set (500n) | 501 | 0 | 0 | 1 | 0 | 4.62 | 16.83 |
| E/insert (100n) | 101 | 1 | 0 | 0 | 0 | 1.00 | 6.25 |
| E/insert (1000n) | 1001 | 1 | 0 | 0 | 0 | 13.30 | 38.30 |
| F/remove (100n) | 100 | 0 | 1 | 57 | 0 | 0.73 | 4.86 |
| G/move10 | 101 | 0 | 0 | 100 | 100 | 0.91 | 7.55 |
| H/batch100 (1000n) | 1001 | 0 | 0 | 100 | 0 | 12.26 | 37.90 |

## 4. 关键结论（Phase 0 证据）

1. **patch 没有增量收益**：1 个 op 的 patch 与全量 submit 几乎同价
   （release：A 4.14 vs 5.16 ms；H 31.9 vs 43.2 ms），每个 op 都触发
   全量 recompile + 全量 fragment 传输 + 全量 plan reconcile。证实计划 P0-2。
2. **forward 占生产者链路约一半**，且随节点数线性增长（100n ≈ 2.4 ms → 1000n ≈ 15.4 ms）：
   整棵 fragment JSON 序列化 + RPC 是主要跨进程成本。
3. **consumer refresh_plan 随节点数线性**：release 下 100n 1-op 改动也要 ≈ 0.6 ms，
   500n ≈ 4.6 ms，1000n ≈ 12-13 ms（debug：5 → 35 → 60-70 ms）。
   reconcile 不是 O(changes)。
4. **无 key 布局涟漪**：删除中间 1 个节点使 57 个未触碰节点 visual 判脏；
   重排 10 个节点使全部 100 个 retained 节点 moved=100、updated=100。
   这是 Phase 1（stable key）与 Phase 2（keyed diff）要消除的核心浪费。
5. **retained 不变量已由 B0-3 计数器可证**：纯属性 patch created==0/removed==0；
   单节点 insert created==1；remove removed==1。后续阶段必须保持。

## 5. 性能预算（release profile，后续阶段验收线）

依据计划 §8/§9 的交互目标（tick 内 <16 ms；enqueue→accepted 本地 <50 ms）：

| 指标 | 预算 | 当前 release 基线 |
|---|---|---|
| 1-op patch 生产者 total（≤100 节点） | ≤ 2 ms | 4.14 ms |
| 50-op 批量 patch 生产者 total（100 节点） | ≤ 3 ms | 3.49 ms |
| 1-op patch 生产者 total（500 节点） | ≤ 5 ms | 14.69 ms |
| 1-op patch 生产者 total（1000 节点） | ≤ 8 ms | 31.94 ms（100 ops 批量；单 op 未测，按 H submit 43.23 ms 上界） |
| consumer refresh_plan（1-op，100 节点） | ≤ 0.5 ms | 0.56 ms |
| consumer refresh_plan（1-op，1000 节点） | ≤ 5 ms | 13.30 ms |
| 1000 节点单帧 draw（release） | ≤ 33 ms | 37.90-38.30 ms |
| 单属性 patch 的 updated 数（Phase 2 后） | updated ≤ op 数（无涟漪） | F: 57/1，G: 100/10 |
| 事件→可见帧端到端（case A，60Hz） | ≤ 16.7 ms | 未测（Phase 7 收口；当前分段和 ≈ 0.6+3.1+4.1 ≈ 7.8 ms 见结论） |

预算由本文件 §2 的 JSONL 做机器比较；任何阶段不得以主观感受替代。

## 6. 验收层级

- 生产者 probe：`service-ready`（公开 `neon3.rpc` 边界，含 accepted revision 校验）。
- 消费者 probe：`gpu-ready`（真实 DX12 device + retained renderer + offscreen render pass）。
- 本阶段不包含 `interactive-accepted`；端到端事件延迟在 Phase 7 收口。
