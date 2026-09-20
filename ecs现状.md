## neon-gpu-ecs 现状

**一句话**：一个 IR 驱动、compute-only 的 GPU ECS 运行时，已独立完成并通过 66 个测试，但**尚未真正接入 `neon-wgpu-runtime`**——README 自己点出了接入阻塞点。

### 规模与结构

- 5,618 行 Rust，25 个文件，三层：
  - `ir/`（~1,900 行）：`EcsIr` 全部数据结构 + `validate()`，**零 wgpu 依赖**，只靠 serde。
  - `generator/`（~2,400 行）：IR → 单个多入口点 WGSL 文本。
  - `runtime/`（~3,700 行，其中 `runtime/mod.rs` 单文件 33KB）：`GpuEcsCtx` 缓冲分配、管线缓存、`run_frame`、结构变更回放、回读。
- 公共出口只有两个：`EcsIr` 和 `GpuEcsCtx`，加一个 5 变体带稳定 `code()` 的 `EcsError`。

### 测试现状（刚跑完）

`cargo test -p neon-gpu-ecs` 全绿，**66/66 通过**：

表格

| 测试文件 | 数量 | 层级 |
| --- | --- | --- |
| `ir_validate.rs` | 18 | contract-ready |
| `control_flow.rs` | 11 | contract-ready（含 4 个 headless 编译） |
| `generator_snap.rs` | 11 | contract-ready（含 headless `create_shader_module`） |
| `execution.rs` | 7 | gpu-ready |
| `gpu_integration.rs` | 4 | gpu-ready |
| `structural.rs` | 4 | gpu-ready |
| `render_data.rs` | 3 | gpu-ready |
| 其余 | 8 | 单元 |

按 AGENTS.md §21 的分层，M1–M3 是 `contract-ready`，M4–M7 是 `gpu-ready`（headless 真实设备）。**README 明确说没有宣称 `wgpu-rendered` / `interactive-accepted`**—— 这是诚实的。

### 接入阻塞点

README §17 自己写了：`neon-wgpu-runtime` 当前请求 `Limits::default()`（每阶段 8 个 storage buffer），而本 crate 需要 `8 + 3 × 组件数`。`GpuEcsCtx::new` 会在构造期检查并报 `ecs_limits_insufficient`。也就是说现在这个 crate 是**独立验证库**，还没有被主 runtime 真正实例化过。

---

## 设计原理

核心思路一句话：**把 ECS 世界蓝图编译成一个 WGSL shader module，CPU 只发命令，不遍历实体**。

### 1. 语言无关 IR → 单 ShaderModule 多入口点

前端（任意语言）生成 serde 可序列化的 `EcsIr`（组件、资源、原型、查询、系统、调度），校验后一次性编译成**一个** WGSL 模块，里面：

- 3 个分拣内核（`system_prep_count/scan/fill`）
- 每个系统一个 `@compute fn system_<name>` 入口点
- 所有入口共享同一套 bind group 布局

切换系统只是切换 entry point，**不产生 PSO 爆炸**—— 这是和典型 "每系统一个 compute pipeline" 路线最大的区别。

### 2. 版本号即存在性（没有位掩码）

每个组件在 GPU 上占 3 个 storage 缓冲：

- 数据 SoA
- 当前版本号 `array<atomic<u32>>`
- 基线版本号

版本 `0` = 实体没有该组件；任何 `Store` 使版本 ≥1 并 `atomicAdd`。组件种类数量不受位掩码限制。

### 3. 三段式 GPU 分拣

每帧：

1. **count**：每实体一线程，对每个查询跑谓词，原子累加计数。
2. **scan**：单线程前缀和，写出每查询 `{start, count}`，导出 `indirectArgs = ceil(count/64)`。
3. **fill**：再跑一次谓词，原子游标散射写 `compactedEntityIds`。

然后 `dispatch_workgroups_indirect(&indirect_exec, 16*query_id)`——**系统入口点拿到的就是压实后的实体切片，CPU 完全不参与实体遍历**。

### 4. Changed / Added 免费做

帧 N 分拣比较 `current` 和 `baseline`（= 帧 N-1 快照），分拣后立刻 `copy_buffer_to_buffer(current → baseline)`：

- `Changed(c)`：`baseline != 0 && current != baseline`
- `Added(c)`：`baseline == 0 && current != 0`

这是典型的 "双缓冲版本号" 技巧，不需要 CPU 维护脏标记。

### 5. TAC + 程序计数器状态机降控制流

系统函数体是三地址码（Load/Store/Const/BinaryOp/If/Jump/...）。没有控制流的系统体**完全平铺成直线 WGSL**（零开销）；有 if/loop 的才降译成 `var ecs_pc + loop { switch(ecs_pc) {...} }` 状态机。生成期做类型推断（局部槽首次赋值定型）。

### 6. 双环命令缓冲做结构变更

GPU 端 `SpawnEntity / DeleteEntity / AddComponent / RemoveComponent` 不直接改实体表，而是 `atomicAdd(&commandCount)` 往命令环里追加 16 字节 `{kind, a, b}`。两个环 ping-pong：

- 系统永远写当前相位环
- 下一帧开头 CPU 回读另一相位环，逐条执行 spawn/delete/add/remove，然后清零

帧 N 的结构变更在帧 N+1 生效，避免了 GPU 写实体表的同步问题。

### 7. 严格的所有权边界

compute-only，不创建 `wgpu::Instance/Adapter/Device`，由 `neon-wgpu-runtime` 注入 device/queue 克隆。这正好落实 AGENTS.md §1"业务进程不持有 GPU 资源" 的约束 —— 虽然现在它和 wgpu-runtime 同进程，但边界是按设计画好的。

---

## 它目前最拿得出手的功能

按 "设计真正有想法、且已被测试验证" 排序：

### 1. **单 ShaderModule 多入口点 + 间接调度**

这是最核心的差异化。一个 world 一次编译，N 个系统只是 N 个 `@compute` entry，共享 bind group 布局。切换系统、加系统都不触发重新建 pipeline。配合三段分拣产出的 `indirectArgs`，空查询自动 `[0,1,1]` 调度 0 个 workgroup，零开销。

### 2. **版本号即存在性 + 双缓冲做 Changed/Added**

没有位掩码、没有 Archetype 位图、没有 CPU 脏标记。组件存在性、Changed、Added 三个问题全靠 "当前版本 vs 基线版本" 两个 atomic 缓冲回答。物理积分器跑起来后 "写者连续写 3 帧后停止 → Changed 命中 3 帧后自然停摆" 这种语义都有 `execution.rs` 钉死。

### 3. **GPU 端结构变更 + CPU 双环回放**

`SpawnEntity` 这种在 GPU ECS 里很棘手的操作，这里用 "GPU 追加命令、下一帧 CPU 回放空闲槽分配" 解决。双环 ping-pong 保证被回放的环永远不是正在被写的环，没有写后读竞争。`structural.rs` 4 个测试覆盖了双环交换、回放幂等、Delete 回收。

### 4. **CPU 参考实现逐一对账**

`runtime/init.rs` 是纯 Rust 函数（`prototype_entity_ranges`、`initial_query_match`、`initial_entity_active`...），既是运行时播种用的，也是 GPU 分拣的**黄金参考**。`gpu_integration.rs` 直接把 GPU 分拣结果和 CPU 手算结果对账 —— 这比只看 "测试通过" 更有说服力，是真正的 cross-check。

### 5. **一次性收集全部 IR 错误**

`validate()` 不是 fail-fast，而是把所有问题分号拼接成一个 `IrInvalid` 返回。生成期还有类型推断、保留入口点冲突、跳转目标必须落在基本块起点等规则。这套校验让 "前端写错 IR" 的代价从 GPU 跑飞变成构造期一行报错。

### 6. **控制流零开销降译**

无 if/loop 的系统体完全平铺，有控制流的才付 PC 状态机的代价。`control_flow.rs` 11 个测试覆盖 if/else、计数循环、fallthrough、嵌套 if。

---

## 还没做的（README §17 自己列的）

- `renderInstances` 接 `DrawIndexedIndirect`（现在只有数据，没接绘制 pass）。
- `ecs.*` RPC 协议面（还没有对外服务接口）。
- 命令环溢出的诊断事件（现在是静默丢弃）。
- **真正被 `neon-wgpu-runtime` 实例化**—— 需要先把那个 crate 的 `max_storage_buffers_per_shader_stage` 从 8 提到 `8+3n`。

总的来说，这个 crate 是**设计完成度和测试密度都相当高的一块独立零件**，但现在还在 "零件已验收、等装机" 的状态。

今天 19:35

如果说它是  gpu-ecs 那他的  组件怎么注册  怎么用  系统逻辑怎么写？   查询什么怎么写

梳理用户使用疑问

我直接看代码里的真实用法，不凭 README 复述。先看公共夹具 `physics_world()`—— 它是所有测试共用的完整世界，最能说明 "怎么写"。

梳理回答框架

下面全部用仓库里真实代码讲，不抽象。

## 1. 组件怎么注册

组件就是往 `EcsIr.components` 里塞一个 `ComponentDef`，**`id` 必须等于它在数组里的下标**（这是硬约束，校验会查）。

```
use neon_gpu_ecs::ir::*;

ComponentDef {
    id: 0,
    name: "Transform".into(),        // 会变成 WGSL 标识符 ecs_c0 / ecs_cv0
    ty: ComponentType::Vec3F,        // 见下表
    default_value: vec![0u8; 12],    // 必须 == ty.byte_size()
},
```

`ComponentType` 只有 8 种，没有自定义 struct：

表格

| 类型 | 字节 | 说明 |
| --- | --- | --- |
| `F32` / `U32` / `I32` | 4 | 标量；U32/I32 自动变 atomic 数组 |
| `Bool` | 4 | 存 u32 (0/1)，局部变量是真 bool |
| `Vec2F` | 8 |  |
| `Vec3F` | 12 | **GPU stride 16**（自动 pad） |
| `Vec4F` | 16 |  |
| `Mat4F` | 64 |  |

注册完还得告诉运行时 "初始世界里有哪些实体"：

```
initial_entities: vec![
    EntityPrototype {
        component_ids: vec![0, 1, 2],   // 这批实体带 Transform+Velocity+Health
        count: 10,                      // 10 个实体，连续 ID
        initial_values: None,           // None = 全用 default_value
        // 或者显式给：每个 component 一份 Vec<u8>，作用于整批
        // initial_values: Some(vec![pos_bytes, vel_bytes, health_bytes]),
    },
],
```

**组件存在性靠版本号**：版本 0 = 没有这个组件；任何 `Store` 使版本 ≥1 并原子 +1。没有位掩码。

---

## 2. 组件怎么用（在系统里读写）

组件读写都是**三地址码**，局部变量槽是 `v0..v{local_var_count-1}`，首次赋值定型。

物理积分器的完整系统（`tests_support.rs` 原文）：

```
SystemDef {
    id: 0,
    name: "physics_update".into(),
    query_id: 0,                       // 这个系统跑哪些实体，见第 4 节
    resource_refs: vec![
        ResourceRef { resource_id: 0, access_type: AccessType::Read },
    ],
    local_var_count: 3,                // v0, v1, v2 三个局部槽
    instructions: vec![
        // v0 = Transform[entity]
        Instr::Load { dest: 0, component_id: 0, access: AccessType::ReadWrite },
        // v1 = Velocity[entity]
        Instr::Load { dest: 1, component_id: 1, access: AccessType::ReadWrite },
        // v2 = DeltaTime (resource)
        Instr::LoadResource { dest: 2, resource_id: 0 },

        // v1 = v1 * v2   (vel *= dt)
        Instr::BinaryOp { dest: 1, lhs: 1, rhs: 2, op: BinaryOpCode::Mul },
        // v0 = v0 + v1   (pos += vel)
        Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },

        // Transform[entity] = v0   ← 写版本号自动 +1
        Instr::Store { src: 0, component_id: 0 },
        // Velocity[entity] = v1
        Instr::Store { src: 1, component_id: 1 },
        Instr::Return,
    ],
}
```

注意几个要点：

- **没有 `this` / `entity` 概念**。每个系统入口点被 dispatch 到一个实体线程，你写的代码默认就是 "当前实体"。
- `Load` 的 `access` 必须和查询里的 `with` 声明匹配；`LoadResource` 的资源必须在 `resource_refs` 里声明。
- `Store` 不只是写数据，**还把版本号原子 +1**—— 这是 `Changed` 检测的基础。没有 "静默写"。
- 立即数用 `Const`：`Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 3.0f32.to_le_bytes().to_vec() }`。

---

## 3. 系统逻辑怎么写（含控制流）

系统体就是上面那串 `Vec<Instr>`，平铺在指令数组里。**没有控制流时完全是直线**，有 if/loop 时用 `If` / `Jump` 按指令索引跳转。

`heater` 系统的真实例子（`tests/execution.rs`）：当 Health < 3 时加 1：

```
local_var_count: 3,
instructions: vec![
    // [0] v0 = Health[entity]
    Instr::Load { dest: 0, component_id: 0, access: AccessType::ReadWrite },
    // [1] v1 = 3.0
    Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 3.0f32.to_le_bytes().to_vec() },
    // [2] v2 = (v0 < v1)
    Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Less },
    // [3] if v2 真 -> 跳到 [4], 假 -> 跳到 [7]
    Instr::If { cond: 2, true_block: 4, false_block: 7 },
    // [4] v1 = 1.0
    Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 1.0f32.to_le_bytes().to_vec() },
    // [5] v0 = v0 + v1
    Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
    // [6] Health[entity] = v0
    Instr::Store { src: 0, component_id: 0 },
    // [7] return
    Instr::Return,
],
```

写成伪代码就是：

```
let v0 = Health[e];
let v1 = 3.0;
if v0 < v1 {
    v0 = v0 + 1.0;
    Health[e] = v0;
}
return;
```

**关键**：`If` 的 `true_block` / `false_block` 是**指令数组下标**（不是标签）。生成器会把它降译成 PC 状态机（`var ecs_pc; loop { switch(ecs_pc) {...} }`）。循环靠 `Jump` 往回跳自己写。

内建函数（数学 + 结构变更）：

```
Instr::CallBuiltin { dest: 0, func: BuiltinFunc::Sin, args: vec![1] }
// 数学：Sin, Cos, Normalize, Length, Dot, Cross

// 结构变更：追加命令到 GPU 命令环，下一帧生效
Instr::CallBuiltin { dest: 1, func: BuiltinFunc::SpawnEntity, args: vec![0] }
//                                               DeleteEntity, AddComponent, RemoveComponent
```

`SpawnEntity(1)` 的含义是 "追加一条 spawn 命令，原型索引 1"—— 不是立即 spawn，是往命令环里写一条记录，**下一帧开头 CPU 回放时才真的分配空闲槽**。

---

## 4. 查询怎么写

查询是 `QueryDef`，和系统分离。一个系统绑定一个 `query_id`。

```
queries: vec![
    // 查询 0：所有同时有 Transform 和 Velocity 的实体
    QueryDef {
        id: 0,
        with: vec![
            ComponentAccess { component_id: 0, access_type: AccessType::ReadWrite },
            ComponentAccess { component_id: 1, access_type: AccessType::ReadWrite },
        ],
        without: vec![],
        filters: vec![],
    },
],
```

三个维度：

### `with` — 必须有的组件

每个组件都要声明访问模式（`Read` / `ReadWrite`）。系统里 `Load` 的组件必须被 `with` 覆盖。

### `without` — 必须没有的组件

```
without: vec![3],   // 不能有 component_id=3 的实体
```

### `filters` — 三个可选过滤器

```
use neon_gpu_ecs::ir::QueryFilter;

filters: vec![QueryFilter::Changed(0)],   // 上一分拣点以来 component 0 被写过
filters: vec![QueryFilter::Added(0)],     // 上一分拣点以来 component 0 首次出现
filters: vec![QueryFilter::RenderData],  // 所有活跃实体无条件通过（渲染用）
```

`Changed` 的真实语义（`execution.rs` 钉死的行为）：

- 帧 N 分拣：比较 `current` 版本和 `baseline`（= 帧 N-1 的快照）
- 写完后立刻 `baseline ← current`
- 所以 "写者连续写 3 帧后停手" → `Changed` 命中 3 帧，第 4 帧起自然归 0

### 调度把系统和阶段绑起来

```
schedule: ScheduleDef {
    stages: vec![
        Stage { id: 0, name: "Heat".into(),   system_ids: vec![0] },
        Stage { id: 1, name: "Detect".into(),  system_ids: vec![1] },
    ],
}
```

- **阶段之间有隐式屏障**（不同 compute pass）。
- **同一阶段内两个系统写同一组件会被 `validate()` 直接拒绝**（`ScheduleConflict`）—— 所以同阶段内可以乱序，跨阶段必须按顺序。
- 每个系统必须恰好出现在一个阶段。

---

## 5. 跑起来的完整骨架

```
let (device, queue) = /* neon-wgpu-runtime 注入的 device/queue 克隆 */;
let ir = physics_world();                       // 上面那棵 EcsIr
let ctx = GpuEcsCtx::new(device, queue, ir,
    /* max_entities */ 1024,
    /* command_capacity */ 256)?;

ctx.seed_initial();                             // 上传原型种群 + 版本种子
ctx.set_resource(0, &dt.to_le_bytes());         // 每帧上传 DeltaTime

loop {
    ctx.set_resource(0, &dt.to_le_bytes());
    ctx.run_frame();                            // 回放→分拣→快照→间接调度
    // 可选回读：
    // ctx.read_component_data(0)               // SoA 字节
    // ctx.read_frame_prep()                    // 每查询 {start, count}
}
```

---

## 一句话总结使用模型

- **组件** = `ComponentDef { id == 下标, name, ty, default_value }`，纯数据，没有函数。
- **查询** = `QueryDef { with, without, filters }`，声明 "我关心哪些实体"。
- **系统** = `SystemDef { query_id, instructions: Vec<Instr> }`，instructions 是三地址码，在压实后的实体切片上每实体跑一个线程。
- **资源** = `ResourceDef`，uniform，v1 只读，每帧 `set_resource` 上传。
- **结构变更** = 调 `SpawnEntity`/`DeleteEntity` 内建，往命令环追加记录，下一帧开头 CPU 回放。

这不是传统 ECS 那种 "实体有 ID、组件有 struct" 的 Rust 写法，而是**把世界描述成一份可序列化的 IR，然后编译成 GPU shader**。你写的是数据，不是 trait impl。
