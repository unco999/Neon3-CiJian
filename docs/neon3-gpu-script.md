# neon3-gpu-script：GPU 脚本语言与固定 Kernel 调度设计

状态：设计定稿（未实现，施工前按本文件验收）
关联：`AGENTS.md`、`crates/neon-gpu`、`docs/neon3-ui-ir.md`

## 1. 背景与目标

Neon3 的 GPU 计算需要一个可组合的算法层：地形生成、侵蚀模拟、
水位场、战斗数值等算法若逐个手写 shader 会失控。本设计定义一层
**面向过程的脚本语言**，编译为数据流图（DAG），经拓扑分层后交给
一批**预编译的固定算法 pipeline（kernel 库）**执行，并解决中间量
的存储、复用与搬运问题，最终产出可缓存、可并行、可 trace 的
执行计划（plan）。

核心承诺：

- 脚本是**声明式资产**，不是运行时黑魔法；可被 AI / CLI / 人类共享。
- 所有 GPU 资源仍归 `neon-wgpu-runtime` 唯一持有；脚本编译器与执行器
  是其中的库层（或独立 crate，依赖方向同 neon-gpu）。
- 每个 plan 都是 headless 可测、可读回断言、可 trace 的。
- 本设计只覆盖计算数据面，不涉及渲染合成。

## 2. 总体架构（自下而上）

```text
脚本语言（薄的语法糖）
  -> IR：数据流图（编译器本体，重点）
    -> 拓扑分层 + 并行规划
      -> 内存规划（liveness -> scratch 分配）
        -> 执行 plan（wave 序列 + 搬运插入 + bind）
          -> 固定 kernel 库（Codelet 注册表）
            -> neon-gpu pool / heap / layout（已就绪）
```

每一层职责单一：

| 层 | 职责 |
| --- | --- |
| 脚本语言 | 只负责表达意图，无调度/内存语义 |
| IR | 正确性：无环 DAG、类型、依赖 |
| 拓扑分层 | 并行与关键路径：wave、融合机会、重叠机会 |
| 内存规划 | liveness -> scratch 分配、alias 零拷贝、搬运插入 |
| 执行 plan | 具体 dispatch 序列 + 绑定 + 缓存 |
| 固定 kernel 库 | 预编译 pipeline + 布局规格（Codelet） |
| neon-gpu | 寻址、池、紧凑绑定 |

## 3. 术语表（施工时统一用语）

| 术语 | 含义 |
| --- | --- |
| 脚本 / scene | 一个编译单元（`.neoncomp` 资产），内部是 SSA 语句列表，含 input/output 契约 |
| 值 / 场 (field) | 脚本中的变量：一张 grid/场，或标量/mask；每格一个数据单元 |
| SSA | Static Single Assignment：每个变量只赋值一次，数据流图天然无环 |
| 节点 | IR 中的一个 kernel 调用（`let v = kernel(args)`） |
| 边 / 依赖 | 值从产生节点流向消费节点 |
| DAG | 有向无环图，脚本的编译产物 |
| 拓扑分层 | `level(v) = max(level(preds)) + 1`；同层节点无依赖 |
| wave | 同一层的节点集合；plan 中表现为一次或几次 dispatch |
| critical path | 最长依赖链，其长度 = 理论最小 dispatch 数 |
| 融合 (fusion) | 同层多个 kernel 合并进单次 dispatch，减少提交与中间内存 |
| liveness | 值的活跃区间 `[def_wave, last_use_wave]`，决定 scratch 复用 |
| scratch | plan 私有的瞬态存储 buffer，中间值按 liveness 分配区间 |
| resident pool | 跨脚本、跨帧驻留的 GpuPool（Handle + generation），状态格/实体/资产 |
| Handle | `(slot, generation)` 稳定身份，见 `crates/neon-gpu` |
| alias | 前后 kernel 布局一致时直接复用同一内存区间，零拷贝 |
| 搬运 kernel | 布局不匹配时插入的 layout 转换 kernel（transpose/tile/interleave） |
| Codelet | 固定 kernel 注册表条目：id + 布局规格 + 预编译 pipeline |
| 参数烘焙 | 常量参数（seed/radius/steps）编译期写死进 pipeline 实例 |
| plan 缓存 | 规范化 IR 的 hash 到完整执行计划的缓存，脚本不变则零重编译 |

## 4. 语言规范

### 4.1 SSA 不可变变量（强制）

每个变量只能被赋值一次。不允许对已定义名字二次赋值。

```text
let v1 = noise(seed=42)      # ok
let v2 = add(v1, x)          # ok
v1 = add(v1, x)              # 非法：SSA 禁止改写
let v1 = add(x, y)           # 非法：名字重复
```

原因：唯一产生点 -> DAG 天然无环 -> 拓扑排序无条件安全 -> 无别名，
liveness 与内存规划始终准确。

### 4.2 纯数据流，无控制流（第一版）

脚本**没有 if / while / for 语句**：

- 循环 -> 固定次数 unroll（`steps=32`）或 kernel 内部参数化。
- 条件 -> `select(a, b, mask)`：两个分支都计算，按 mask 选择。
- 没有副作用语句，只有 `let` 绑定和 `export` 声明。

控制流后续版本再议；本设计所有调度决策在编译期完成，依赖此约束。

### 4.3 场（field）抽象

第一版变量以**场**为主：一张 grid（尺寸、元素类型、布局由推导或
显式标注决定）。地形脚本几乎全是场运算。支持：

- `input` 场、中间场、`export` 场。
- 标量值作为 kernel 常量参数或窄数据（mask）出现。
- 显式布局标注仅在推导失败或需要控制搬运时出现：
  `let x: field<f32, layout=planar> = ...`

### 4.4 scene 契约：input / output

脚本对外的内存接触面是**显式契约**，不是"随便引用就能改"。

```text
scene crit_combo = {
    input: player_stats, target_stats, frame
    output: target.hp
}
```

- `input` 默认**只读**：kernel 只能采样，无法改外部 buffer。
- 写回外部内存只允许通过 `output` 声明的目标。
- **同一 buffer 同一帧只能有一个 writer**：规划层做全局冲突检测；
  两个 scene 同时 export 同一 buffer -> 编译报错，逼显式排序或合并。
- 写回是显式事务：输出值在 plan 末尾统一写回，不存在"运行中偷偷
  改外部状态"的不可见效应。
- `input` / `output` 解析到 **resident handle**（见第 6 节）。

### 4.5 命名空间：三层模型（已定）

脚本与外部世界之间是**三层命名空间**，不用扁平全局名。

```text
层 0：scene 私有符号（let 变量、input 别名）
      —— 只在脚本内部可见，不导出就没人知道

层 1：世界资源名（限定名）  domain.name
      —— 全局唯一，由"世界资源表"权威注册，脚本只能引用不能发明

层 2：运行时身份（Handle）和 GPU binding
      —— 由资源表在运行期解析，脚本永不接触
```

**语法：限定名 + 别名**

```text
# 世界资源表（运行时/领域层维护，不是脚本定义）
#   target.hp      : field<f32, [1]>
#   target.def     : field<f32, [1]>
#   target.stats   : field<f32, [8]>
#   frame.frame    : field<u32, [1]>    每帧递增
#   terrain.water  : field<f32, [64,64]>

scene crit_combo = {
    input:
        target.stats as player_stats,   # 限定名 -> 局部别名
        target.def   as def,
        frame.frame  as frame
    output:
        target.hp                        # 写回目标必须是已注册的世界名
}

let dmg  = damage_formula(player_stats, def, kind="physical")
...
export target.hp = hp
```

规则：

- input/output 里的名字一律是 `domain.name` 限定名，必须已在世界
  资源表注册，否则编译期报错（未知名字）。
- 脚本内部引用用短别名（`player_stats`），别名是 scene 私有层 0
  符号，随意取不冲突。
- `let` 变量不许与 input 别名重名（场景内符号表唯一，保持 SSA 干净）。
- 写回目标 = 世界名：`export target.hp = hp`。同一世界名同一帧只
  允许一个 writer。
- 限定名两段式：`domain.name`。不用更深层级（那是普通模块系统）。
- 不用扁平全局名：碰撞与隐式耦合，违背显式契约哲学，规划器无法
  做 writer 冲突检测与跨 scene 分析。

**解析流程（编译期 + 运行期）**

```text
脚本文本中的限定名 target.hp
  └─ 编译期：查世界资源表 -> 得到类型/布局/只读标志（不符则报错）
      └─ 生成 plan：plan 里存「名字 -> binding 索引」的槽位，不存死 Handle
          └─ 运行期：执行前查表拿到当前 Handle -> resolve -> bind
```

plan 缓存只依赖名字和类型，不依赖 Handle 数值。资源重建导致 Handle
变化时，plan 不用重编译，只需重新解析绑定（对应 AGENTS.md 的
epoch / resident_handle 语义）。

**跨 scene 连接：显式接线**

两个 scene 之间不靠"碰巧同名"，而是显式声明（plan 规划器据此合并
DAG）：

```text
connect terrain_gen.result -> erosion.input_height
```

`connect` 两端也必须先在世界资源表注册（或一端是 scene 的 export
别名），名字解析与冲突检测与 input/output 一致。

### 4.6 控制流：GPU 直跑模式（已定）

**原则：脚本一旦提交进 GPU 就持续运行到结束，中间零读回、零 CPU
同步**。所有控制流在编译期被"静态展开"，把循环和分支从脚本里
彻底消掉，产物仍是纯数据流 dispatch 序列。CPU 只在帧边界读回。

| 控制流 | 语法 | 解析结果 | CPU 参与 |
| --- | --- | --- | --- |
| 分支 | `select(a, b, mask)`（数据选择） | 两路都算，按格选择；mask 是 kernel 算出的场 | 零 |
| 定次循环 | `for round in 1..3`（语法糖） | **unroll** 成固定份 SSA 语句，变量改名保持 SSA | 零 |
| 变次收敛 | `until_converge, tol=…, max_iters=N` | **首选：kernel 内部自循环**（workgroupBarrier + 全局收敛计数器，收敛后空转跳车）；跨多个 kernel 时固定展开 N 份 + 收敛掩码空转 | 零 |
| 大规模循环体剪枝 | 可选优化 | plan 分割（子 plan + 条件读回），CPU 决定是否提前终止——**降级为可选**，仅在 kernel 内循环塞不下且节省量可观时使用 | 一次读回 |

关键点：

- `branch` 不是语法——它是数据：`let mask = above(rain, 0.3)`，
  `let v = select(a, b, mask)`。没有 CPU 决策点。
- `for` 定次循环 unroll 后是纯数据流链，共享子表达式（如
  `wet_rate` 被多轮使用）由 DAG/liveness 处理。
- 变次循环的收敛判断在 GPU 内部完成，`max_iters` 是硬上限
  （对齐 AGENTS.md：禁止无限运行），超限由 kernel 报告
  `loop_iteration_budget_exceeded`。
- 执行器只做一件事：按 wave 顺序把 dispatch 一次性提交进 queue，
  然后返回。唯一同步点在帧边界（下一帧要拿结果做决策/显示）。

**示例解析（侵蚀模拟，3 轮循环 + 区域分支 + 变次收敛）**

```text
scene erosion_sim = {
    input:
        terrain.height as h0,
        terrain.rain    as rain,
        frame.frame     as frame
    output:
        terrain.height
}

let wet_mask = above(rain, 0.3)              # 场：0/1
let wet_rate = select(0.5, 0.05, wet_mask)   # 数据分支

for round in 1..3:
    let e = erode(h, wet_rate, strength=0.2)
    let h = clamp(e, 0.0, 64.0)

let out = relax(h, until_converge, tol=0.001, max_iters=16)
export terrain.height = out
```

步骤 1 语法糖展开（unroll + 改名保持 SSA）：

```text
let wet_mask = above(rain, 0.3)
let wet_rate = select(0.5, 0.05, wet_mask)
let e1 = erode(h0, wet_rate, 0.2)
let h1 = clamp(e1, 0.0, 64.0)
let e2 = erode(h1, wet_rate, 0.2)
let h2 = clamp(e2, 0.0, 64.0)
let e3 = erode(h2, wet_rate, 0.2)
let h3 = clamp(e3, 0.0, 64.0)
let out = relax(h3, tol=0.001, max_iters=16)
```

展开后仍是纯数据流。IR DAG、拓扑分层、执行如下：

```text
rain ─> above ─> select ─> wet_rate ─┐
                                     ├─> erode ─> clamp ─> erode ─> clamp ─> erode ─> clamp ─> relax ─> out
h0 (input) ──────────────────────────┘

wave 0: above          wave 4: erode(2)      wave 8: relax（kernel 内自循环）
wave 1: select         wave 5: clamp(2)
wave 2: erode(1)       wave 6: erode(3)
wave 3: clamp(1)       wave 7: clamp(3)

CPU 提交：queue.submit([above][select][erode][clamp][erode][clamp][erode][clamp][relax])，零中间同步
```

`relax` 变次循环的两种纯 GPU 实现：

1. kernel 内自循环（首选）：`while(!converged && iter<max_iters)`，
   workgroupBarrier + 全局收敛计数器，收敛后空转跳车。
2. 固定展开 + 收敛掩码：循环体跨多个 kernel 时展开 N 份，每轮末尾
   收敛检测 kernel 更新收敛场，后续 kernel 按 mask 空转。

**plan 分割（原 v2 CPU 分支）降级为可选剪枝优化**：只有循环体过大
塞不进单个 kernel、且提前终止的收益明显大于一次读回延迟时才启用。
默认机制永远是静态展开 GPU 直跑。

### 4.5 编译产物约定

- 脚本文件头带 `schema_version`（版本化，跨版本变更走迁移）。
- `trace <value>` 语法可把任意中间值标记为可读回导出，供测试与调试。
- 脚本即声明式验收场景的原料：fixture 输入 -> plan -> 执行 -> 读回断言。

## 5. 语言示例

### 5.1 地形生成 + 水流侵蚀

```text
# terrain.gen —— 程序化地形 + 水流侵蚀
schema_version: 1

scene terrain_gen = {
    input: heightfield(fixture://hills_64x64)   # 来自项目资产的 AssetRef
}

let base    = noise(seed=42, octaves=5, lacunarity=2.0, gain=0.5, scale=0.01)
let height  = add(base, terrain_gen.input)
let smooth  = blur(height, radius=3, mode=gaussian)
let slope   = slope_map(smooth)                 # 坡度场
let wet     = rain_map(slope, rain=0.8, seed=7) # 降雨 -> 湿度场
let flow    = flow_sim(smooth, wet, steps=32)   # 水流（循环在 kernel 内部）
let eroded  = erode(smooth, flow, strength=0.2)
let water   = clamp(flow, 0.0, 1.0)
let mask    = water_above(water, 0.5)
let out     = select(eroded, water, mask)       # 条件 = 数据选择

export terrain_gen.result = out
```

编译产物（DAG）：

```text
noise ──────────────────────┐
                            ├─> add ─> blur ─┬─> slope_map ─> rain_map ─┐
input (fixture) ────────────┘                │                         ├─> flow_sim ─┬─> erode ─┐
                                             └─────────────────────────┘             ├─> clamp ─> water_above ─┘
                                                                                     └───────────────────────> select ─> out
```

拓扑分层（同层无依赖，可并行 dispatch 或融合）：

```text
wave 0: noise
wave 1: add
wave 2: blur
wave 3: slope_map
wave 4: rain_map
wave 5: flow_sim
wave 6: erode + clamp          # 无依赖：可并行 dispatch 或融合
wave 7: water_above
wave 8: select                 # 关键路径 = 9 次 dispatch
```

### 5.2 游戏战斗 / 技能（每帧重跑）

```text
# crit_combo.neoncomp —— 每帧重跑：plan 缓存跨帧复用，只有输入变化
schema_version: 1

scene crit_combo = {
    input: player_stats, target_stats, frame
    output: target.hp
}

let dmg  = damage_formula(player_stats.atk, target_stats.def, kind=physical)
let crit = rng_chance(seed=frame, chance=0.25)
let hit  = select(dmg, mul(dmg, 2.0), crit)    # 暴击 = 数据选择
let hp   = apply_damage(target_stats.hp, hit)  # 返回新格值，plan 末尾写回
let fx   = spawn_particles(hit, count=32, kind=spark)

export target.hp = hp
export fx = fx
```

`rng_chance` 依赖 `frame` 输入，使该 scene 具备"每帧可重跑"的确定性
（同 frame 同输入 -> 同输出，可复现、可测试）。

## 6. 内存模型：scratch 与 resident 的分工

脚本内存分两层，这是本设计的关键：

| 数据类别 | 存储 | 是否用 Handle | 生命周期 |
| --- | --- | --- | --- |
| 中间值（slope、flow） | plan scratch 区间，静态 offset | 否 | plan 执行期（liveness 决定） |
| 状态格（水位场、侵蚀状态、hp） | resident GpuPool | 是 `(slot,generation)` | 跨帧、跨脚本 |
| 动态实体（粒子、临时对象） | resident GpuPool，每帧 alloc/free | 是 | 运行期动态 |
| 资产驻留（fixture 高度图） | resident GpuPool | 是 | 加载后驻留 |

**为什么中间值不用 Handle**：它们由 liveness 静态规划，直接在 scratch
buffer 上拿 offset，不需要身份、不需要动态分配。

**Handle/pool 是"内存身份层"**（对应 AGENTS.md §9 `resident_handle=[slot,generation]`）：

```text
AssetRef ──> neon-wgpu-runtime 把数据放进 GpuPool ──> Handle { slot, generation }
                                                          │
脚本编译期：input 引用 ──resolve──> GPU binding
中间值 ──> scratch 区间（无 handle）
```

- `input: player_stats` 在编译期解析成一个 resident handle；
  plan 生成时 resolve 成 binding 绑进 kernel。
- 动态实体依赖 pool 的 generation：陈旧 Handle 被拒绝，防越界读旧数据。
- 同一 GpuPool 可被多个 plan 只读共享（read_only 池），写池受 writer 冲突
  检测约束。

## 7. 拓扑拆解与 GPU"并行"语义

GPU 上没有 CPU 式多线程 kernel 调度器：同一 queue 的 dispatch 天然串行。
因此分层拆解的收益不是"多线程并行"，而是：

1. **融合**：同层无依赖节点合并为单 dispatch（kernel 两两可组合时）。
2. **重叠**：无依赖 wave 可分配到多 queue（wgpu 多 queue）流水线执行。
3. **显式化**：critical path 长度 = 理论最小 dispatch 数；依赖关系一目了然。

全局拆解 = 多 scene DAG 合并为一个大 DAG（scene 间以 export/import 连边），
统一分层，跨 scene 融合与重叠。合并前做：

- 常量折叠（constant folding）
- 公共子表达式消除（CSE）
- 规范化（节点排序），其结果作为 plan 缓存 hash 的输入

## 8. 中间量规划：liveness + alias + 搬运

### 8.1 分配

- 每个值记录 `[def_wave, last_use_wave]`。
- 线性扫描 / 图着色把活跃区间分配到统一 scratch storage buffer 的区间。
- scratch 总大小是 plan 的一部分，可提前一次性分配。

### 8.2 Alias 零拷贝

布局相同的前后 kernel 直接复用同一内存区间，中间不产生任何操作。
**多数"移动变量"根本不需要移动**——这是最大性能来源，也是规划器
第一优先：先尝试 alias，失败才插搬运。

### 8.3 "模拟栈"是特例，不是模型

用户直觉（栈式生命周期）在**表达式树**下成立：每个值单消费者、按序
生成 -> LIFO 天然复用。作为快速路径特例保留。
但一般 DAG 含共享子表达式（一个值多个消费者），栈会失败，必须退回
通用 liveness 分配。规划器对树形子图走栈路径，其余走池路径。

### 8.4 搬运 kernel（layout 转换）

搬运 compute 阶段**只做布局转换**：transpose / tile / interleave /
stride 变化 / format 转换。触发条件：

- kernel A 输出布局 != kernel B 输入布局。
- 需要改变 tiling 以匹配 kernel 的 workgroup 形状。

惰性插入：先尝试 alias，失败才插搬运；搬运本身也是 Codelet，参与
分层与 trace。

## 9. 固定 Kernel 库（Codelet 注册表）

数据驱动，不做宏生成：

```rust
struct Codelet {
    id: &'static str,          // "map.add", "scan.sum", "layout.tile"
    inputs: Vec<LayoutSpec>,   // 输入布局约束
    output: LayoutSpec,        // 输出布局
    pipeline: ComputePipeline, // 预编译缓存，plan 编译期绑定
}
```

- **参数烘焙**：常量参数（seed/radius/steps/mode）编译期烘焙进 uniform /
  push constant；同 kernel 不同参数 = 不同 pipeline 实例，全部预编译缓存。
- **数据参数**走 pool bindings（neon-gpu PoolHeap 紧凑绑定）。
- 分类：

| 类别 | 例子 |
| --- | --- |
| elementwise（map 族） | add / mul / lerp / clamp / abs / max / min |
| reduce / scan | sum / min / max / prefix / histogram |
| gather / scatter（邻居采样，地形核心） | blur / flow / 邻域和 / slope / erode |
| layout | transpose / tile / interleave / reshape |
| glue | select / mask / move / push / pop / swap（"模拟栈"） |

- **领域有界性是前提**：地形算法集合为几十个 kernel 量级，固定库成立。
  若未来脚本需任意算法，可切换运行时 codegen——IR 层不变，只换 kernel
  来源（Codelet trait 抽象出 `pipeline` 的获取方式）。

## 10. 执行器与 Plan 缓存

- **编译期（CPU，一次性）**：脚本 -> IR -> DAG -> 分层 -> liveness ->
  搬运插入 -> pipeline 实例绑定 -> 完整 plan。
- **plan 缓存**：规范化 IR hash -> plan。脚本不变 -> 零重编译。
- **执行器**：遍历 waves，对每个 wave 绑定 bind group + dispatch；
  输入 Handle 变化时仅重绑 bind group，plan 结构不变。
- **scratch 复用**：scratch 按 plan 预分配，跨帧复用（同一 plan 的
  scratch 尺寸不变）。
- 每帧重跑的 scene（如 crit_combo）在 plan 缓存命中后，仅刷新输入
  Handle 与绑定，重新 dispatch。

## 11. 可观测性与验收（对齐 AGENTS.md）

- **headless 可测**：固定 fixture 脚本 -> plan -> 执行 -> 读回断言。
- **结构化 trace**，每个 record 至少含：plan id、wave 序号、dispatch
  序号、kernel id、输入/输出 Handle、scratch 区间、耗时。
- 统计指标：wave 数、dispatch 耗时、scratch 用量、alias 命中率、
  fusion 命中率、plan 缓存命中率、搬运插入数。
- 禁止"等待固定毫秒数"猜测 Ready；一切以 job/status 与读回为准。
- 脚本即声明式验收场景：AI/CLI 可提交执行、读机器可读结果。
- 生产路径不允许把 GPU 内存写权开放给任意脚本；writer 冲突必须在
  编译期报错。

## 12. 实施顺序

1. **Phase 1（验证切片，不写语言）**：手写 3 个固定 kernel
   （elementwise + scan + copy）+ 一个手写 plan 串起来，验证
   pool -> kernel -> pool 链路、alias 零拷贝、读回断言。
2. **Phase 2**：IR + 数据流解析（SSA 脚本 -> DAG），含类型与布局推导。
   验证场景：crit_combo 暴击脚本（§5.2）——解析 -> SSA 检查 ->
   DAG -> 分层，全部 headless 测试。
3. **Phase 3**：拓扑分层 + liveness 规划 + 搬运自动插入。
4. **Phase 4**：plan 缓存 + 执行器 + trace 与统计。
5. **Phase 5**：多 scene 全局合并统一规划（跨脚本融合/重叠）。

每个 Phase 完成标准：该层 headless 可测、trace 可查、产出机器可读
结果，并在验收记录中标注达到的层级（contract-ready / service-ready /
gpu-ready）。

## 13. 依赖与边界

- 依赖方向：脚本编译器/执行器 -> neon-gpu（layout/pool/heap/hal_map），
  不反向。
- 不引入：运行时 shader 编译（第一版）、控制流、远程执行。
- 与 `neon-wgpu-runtime` 的关系：执行器运行在其进程内；生产环境 GPU
  资源归它唯一持有。
- 与 `neon-projectd`：脚本与 fixture 是项目资产，经 AssetRef 引入，
  不直接读文件系统。

## 14. 待拍板决策点

1. SSA 不可变变量是否接受（决定 DAG 安全性）。——当前建议：接受
2. 控制流形态：已定为"静态展开 + kernel 内循环"的 GPU 直跑模式
   （§4.6），plan 分割降级为可选剪枝优化。——已定，实施 Phase 2 验证
3. 同 wave 内 kernel 融合第一版是否做。——当前建议：Phase 1 只测
   顺序 dispatch，融合留 Phase 3 视数据再定
4. 固定 kernel 库第一批覆盖哪批地形算法。——待定（需领域清单）
5. 场（field）作为唯一第一版值类型，标量/结构体混合是否延后。
6. scene 粒度与跨 scene 合并规则（export/import 命名空间）。