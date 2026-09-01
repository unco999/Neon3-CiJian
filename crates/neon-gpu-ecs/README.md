# neon-gpu-ecs

> **案例链接（Examples）：<https://github.com/unco999/Neon3-example>**

IR 驱动、多入口点单 ShaderModule、间接调度的 GPU ECS 运行时（Rust + wgpu 30 / WGSL）。

本 crate 消费一份语言无关、可序列化（serde）的 `EcsIr` 世界蓝图，把它编译成**一个包含
N 个 `@compute` 入口点的 WGSL 模块**，然后在 GPU 上完成数据分拣（sorting）与按
`dispatch_workgroups_indirect` 的系统执行。CPU 只负责编码命令，不参与实体遍历。

- **Compute only**：不创建 `wgpu::Instance` / Adapter / Device，不开窗口，不说网络协议。
  唯一调用方是 `neon-wgpu-runtime`（Neon3 唯一 GPU owner），由它注入 device/queue 克隆。
- **语言无关**：`EcsIr` 可由任意前端（C#/Python/Lua/JSON）生成。
- **单次编译**：所有系统逻辑编译进同一个 `ShaderModule`，共享一套绑定组布局，
  切换系统只是切换入口点，避免 PSO 爆炸。

---

## 目录

1. [架构总览](#1-架构总览)
2. [快速开始](#2-快速开始)
3. [IR 规范（`EcsIr`）](#3-ir-规范ecsir)
4. [TAC 指令集（系统函数体）](#4-tac-指令集系统函数体)
5. [类型系统](#5-类型系统)
6. [校验规则](#6-校验规则)
7. [代码生成（WGSL）](#7-代码生成wgsl)
8. [绑定组布局](#8-绑定组布局)
9. [缓冲清单与尺寸](#9-缓冲清单与尺寸)
10. [运行时接口（`GpuEcsCtx`）](#10-运行时接口gpuecsctx)
11. [帧生命周期](#11-帧生命周期)
12. [分拣流水线](#12-分拣流水线)
13. [Changed / Added 变更检测](#13-changed--added-变更检测)
14. [结构变更（Spawn/Delete/AddComponent/RemoveComponent）](#14-结构变更)
15. [RenderData 实例缓冲](#15-renderdata-实例缓冲)
16. [错误码](#16-错误码)
17. [设备要求与集成约束](#17-设备要求与集成约束)
18. [已知坑与注意事项](#18-已知坑与注意事项)
19. [测试地图](#19-测试地图)
20. [License](#20-license)

---

## 1. 架构总览

```
前端（任意语言）                    neon-gpu-ecs
─────────────────     JSON/MsgPack   ┌─────────────────────────────────────┐
 生成 EcsIr  ───────────────────────▶│ ir/        结构 + validate()         │
                                     │ generator/ IR → 单个多入口点 WGSL    │
                                     │ runtime/   缓冲/管线/帧循环/回放     │
                                     └──────────────┬──────────────────────┘
                                                    │ 注入 device/queue
                                     neon-wgpu-runtime（唯一 GPU owner）
```

分层：

| 模块 | 职责 | 依赖 |
| --- | --- | --- |
| `ir/` | `EcsIr` 全部数据结构 + `validate()` | 仅 serde，无 wgpu |
| `generator/` | WGSL 文本生成：绑定声明、查询谓词、分拣内核、系统入口点、控制流降译 | `ir` |
| `runtime/` | `GpuEcsCtx`：缓冲分配、管线缓存、`run_frame`、结构变更回放、回读 | `generator` + wgpu |

单帧数据流：

```
CPU 上传资源 (set_resource)
    │
    ▼
┌─────────────── run_frame ───────────────────────────────┐
│ ① 回放上一帧命令环（回读 → CPU 执行 Spawn/Delete → 清零）  │
│ ② 分拣：count → scan → fill（产出 framePrepBuffer /      │
│    compactedEntityIds / indirectArgs）                   │
│ ③ 基线快照：version_current → version_baseline           │
│ ④ indirectArgs → indirectExec（专用间接缓冲）            │
│ ⑤ 按 Schedule 逐阶段 dispatch_workgroups_indirect        │
└─────────────────────────────────────────────────────────┘
```

---

## 2. 快速开始

```rust
use neon_gpu_ecs::GpuEcsCtx;
use neon_gpu_ecs::tests_support::physics_world; // 示例世界：Transform+Velocity+Health

// device/queue 由宿主（neon-wgpu-runtime）注入；测试里可自建高 limits 设备：
//   required_limits: wgpu::Limits {
//       max_storage_buffers_per_shader_stage: 32,
//       ..wgpu::Limits::default()
//   }
let (device, queue) = my_headless_device();

let ir = physics_world();                       // EcsIr，也可从 JSON 反序列化
let ctx = GpuEcsCtx::new(device, queue, ir,
    /* max_entities */ 1024,
    /* command_capacity */ 256)?;               // Result<Self, EcsError>

ctx.seed_initial();                             // 上传原型种群 + 版本种子 + 资源默认值
ctx.set_resource(0, &dt.to_le_bytes());         // 每帧上传 DeltaTime 等 uniform

loop {
    ctx.run_frame();                            // 分拣 → 快照 → 间接调度执行
    // 可选回读验证：
    // let ranges = ctx.read_frame_prep();      // 每查询 {start, count}
    // let pos = ctx.read_component_data(0);    // SoA 数据（含 16 字节 stride 填充）
}
```

`GpuEcsCtx::new` 在构造期完成：`ir.validate()` → 同阶段写冲突检查 →
设备限额检查 → `generate_wgsl` → `create_shader_module`（naga 校验）→
全部管线（每入口点一个 `ComputePipeline`）→ 全部缓冲与两个绑定组。任何一步失败都返回
`EcsError`（不会半初始化）。

---

## 3. IR 规范（`EcsIr`）

```rust
pub struct EcsIr {
    pub version: u32,                          // 必须为 1
    pub components: Vec<ComponentDef>,         // 索引 == id
    pub resources: Vec<ResourceDef>,           // 索引 == id
    pub initial_entities: Vec<EntityPrototype>,
    pub queries: Vec<QueryDef>,                // 索引 == id
    pub systems: Vec<SystemDef>,               // 索引 == id
    pub schedule: ScheduleDef,                 // 宏观阶段
}
```

### 3.1 ComponentDef（组件注册）

```rust
pub struct ComponentDef {
    pub id: u32,
    pub name: String,            // 合法 WGSL 标识符，全局唯一
    pub ty: ComponentType,       // 显式类型（见第 5 节）
    pub default_value: Vec<u8>,  // 小端，长度必须 == ty.byte_size()
}
```

每个组件在 GPU 上占 **3 个 storage 缓冲**：数据（SoA）、当前版本号、基线版本号。

**组件存在性 = 版本号语义**：版本 `0` 表示实体没有该组件；任何写入使其 ≥1 并递增。
没有位掩码，组件种类数量无上限。

### 3.2 ResourceDef（固定资源 / 单例）

```rust
pub struct ResourceDef {
    pub id: u32,
    pub name: String,
    pub ty: ComponentType,        // 必须 uniform 安全（见 5.2）
    pub binding_slot: u32,        // group(1) 槽位，唯一且 < 30
    pub default_value: Vec<u8>,   // 首帧前上传
}
```

资源绑定为 `var<uniform>`，**v1 只读**（校验拒绝系统写资源）。每帧通过
`GpuEcsCtx::set_resource` 上传。

### 3.3 EntityPrototype（初始种群）

```rust
pub struct EntityPrototype {
    pub component_ids: Vec<u32>,
    pub count: u32,                            // ≥ 1
    pub initial_values: Option<Vec<Vec<u8>>>,  // 与 component_ids 对齐；
                                               // None/缺省用 ComponentDef::default_value
}
```

注意：`initial_values` 作用于该种群的**全部**实体（不是逐实体）。多个原型按声明顺序
占用连续实体 ID。

### 3.4 QueryDef（查询）

```rust
pub struct QueryDef {
    pub id: u32,
    pub with: Vec<ComponentAccess>,   // 必须有组件；含访问模式
    pub without: Vec<u32>,            // 必须没有的组件
    pub filters: Vec<QueryFilter>,    // Changed / Added / RenderData
}

pub enum QueryFilter {
    Changed(u32),   // 基线≠0 且 当前≠基线（上一分拣点以来被写过）
    Added(u32),     // 基线==0 且 当前≠0（上一分拣点以来首次出现）
    RenderData,     // 渲染种群：所有活跃实体无条件通过（见第 15 节）
}
```

查询在分拣阶段被压实为 `compactedEntityIds` 中的一段 `{start, count}`（`framePrepBuffer`
按查询槽位索引），系统入口点据此做间接调度。

### 3.5 ScheduleDef（调度）

```rust
pub struct ScheduleDef { pub stages: Vec<Stage> }
pub struct Stage {
    pub id: u32,
    pub name: String,
    pub system_ids: Vec<u32>,   // 阶段内顺序执行
}
```

- 阶段间隐式屏障（不同 compute pass / 顺序提交）。
- 阶段内**禁止写同一组件**（`ScheduleConflict`）——这保证了阶段内多系统无序依赖。
- 每个系统必须恰好出现在一个阶段。

---

## 4. TAC 指令集（系统函数体）

系统逻辑是三地址码（TAC）平铺指令流。局部变量槽 `v0..v{local_var_count-1}` 在生成期
做类型推断（首次赋值定型）。

| 指令 | 语义 |
| --- | --- |
| `Load { dest, component_id, access }` | `v_dest = comp[entity]`（Bool → `!= 0u`；U32/I32 走 `atomicLoad`） |
| `Store { src, component_id }` | `comp[entity] = v_src`，**同时版本号 +1**（`atomicAdd`） |
| `Const { dest, ty, bytes }` | 立即数加载；小端字节；不支持 `Mat4F` |
| `LoadEntityId { dest }` | `v_dest = 当前实体 ID`（u32） |
| `LoadResource { dest, resource_id }` | `v_dest = ecs_r{id}`；资源必须在 `resource_refs` 声明 |
| `BinaryOp { dest, lhs, rhs, op }` | `+ - * / % & | ^`；仅 `Mul` 允许 `vecNf × f32` 标量提升 |
| `UnaryOp { dest, src, op }` | `Neg` / `Not`（`Not` 仅 bool） |
| `Compare { dest, lhs, rhs, cond }` | `== != < <= > >=`，结果为 `bool` |
| `If { cond, true_block, false_block }` | 条件跳转（指令索引） |
| `Jump { target }` | 无条件跳转（指令索引） |
| `Return` | 结束当前实体线程 |
| `CallBuiltin { dest, func, args }` | 内建调用（下表） |
| `AtomicOp { component_id, op, value }` | `atomicAdd/Sub/Exchange`；v1 不支持 `CompareExchange` |
| `StoreRender { src, field }` | 写实例缓冲字段（`Transform`/`Color`，`vec4f`）；仅 `RenderData` 查询的系统可用 |

### 4.1 内建函数与参数个数

| 函数 | 参数数 | 结果类型 | 说明 |
| --- | --- | --- | --- |
| `Sin` / `Cos` / `Normalize` | 1 | 同参数 | 数学内建 |
| `Length` / `Dot` | 1 / 2 | `f32` | 向量长度 / 点积 |
| `Cross` | 2 | `vec3f` | 叉积 |
| `SpawnEntity` | 1（原型索引） | 丢弃 | 追加生成命令 |
| `DeleteEntity` | 1（实体 ID） | 丢弃 | 追加删除命令 |
| `AddComponent` | 2（实体, 组件） | 丢弃 | 追加添加组件命令 |
| `RemoveComponent` | 2（实体, 组件） | 丢弃 | 追加移除组件命令 |

结构变更调用把命令追加进当前帧的命令环（见第 14 节），**下一帧开头生效**。

### 4.2 控制流降译

- 无 `If`/`Jump` 的系统体保持**平铺直线**形式（零开销）。
- 含控制流的系统体降译为 `var ecs_pc + loop { switch(ecs_pc) { case N: … } }`
  程序计数器状态机：
  - 基本块起点 = 指令 0、所有跳转目标、每个终结符（If/Jump/Return）的下一条；
  - 无终结符的块直通（fallthrough）下一块；最后一块隐式 `return`；
  - 每个 `switch` 带 `default: return` 防御分支。

---

## 5. 类型系统

### 5.1 ComponentType

| 变体 | 字节 | 对齐 | WGSL 存储类型 | 局部类型 | 数据缓冲 |
| --- | --- | --- | --- | --- | --- |
| `F32` | 4 | 4 | `f32` | `f32` | `array<f32>` |
| `Vec2F` | 8 | 8 | `vec2f` | `vec2f` | `array<vec2f>` |
| `Vec3F` | 12 | 16 | `vec3f` | `vec3f` | `array<vec3f>`（**stride 16**） |
| `Vec4F` | 16 | 16 | `vec4f` | `vec4f` | `array<vec4f>` |
| `Mat4F` | 64 | 16 | `mat4x4f` | `mat4x4f` | `array<mat4x4f>` |
| `U32` | 4 | 4 | `u32` | `u32` | `array<atomic<u32>>` |
| `I32` | 4 | 4 | `i32` | `i32` | `array<atomic<i32>>` |
| `Bool` | 4 | 4 | `u32`（0/1） | `bool` | `array<u32>` |

- `U32`/`I32` 组件因支持 `AtomicOp` 一律声明为原子数组，`Load`/`Store` 生成
  `atomicLoad`/`atomicStore`。
- `Bool` 在存储上是 `u32`，局部变量是真 `bool`，边界处用 `!= 0u` / `select(0u, 1u, …)` 转换。

### 5.2 uniform 安全类型

`ResourceDef.ty` 只允许：`F32 / Vec2F / Vec4F / Mat4F / U32 / I32`。
`Vec3F`（uniform 占 16 字节但 `byte_size()` 是 12，会失配）和 `Bool`（不可共享）被拒绝。

### 5.3 二元运算规则

- 两操作数必须同类型；
- **唯一例外**：`Mul` 允许 `vecNf × f32` / `f32 × vecNf`（结果为向量），
  设计笔记中的 `vel = vel * dt` 依赖此规则；
- 其余混合类型（含 `vec + scalar`）在生成期报 `WgslInvalid`。

---

## 6. 校验规则

`EcsIr::validate()` 收集**全部**问题一次性返回（`EcsError::IrInvalid`，分号分隔）：

| 类别 | 规则 |
| --- | --- |
| 通用 | `version == 1`；所有 `id` 必须等于所在数组的索引 |
| 组件 | 名字非空、是合法 WGSL 标识符、全局唯一；`default_value` 字节数 == 类型字节数 |
| 资源 | 名字唯一且合法；`binding_slot` 唯一且 < 30；类型 uniform 安全；v1 禁止系统写资源 |
| 查询 | `with` 非空；引用组件存在；`with`/`without` 不重叠、不重复；过滤器组件存在 |
| 系统 | 名字唯一且合法；查询存在；局部槽不越界；跳转目标不越界；内建参数个数正确；`Load/Store` 的组件必须被查询覆盖；`LoadResource` 必须在 `resource_refs` 声明；`StoreRender` 只能出现在 `RenderData` 查询的系统；`Const` 字节数与类型匹配 |
| 调度 | 阶段 `id` 与索引一致；系统引用存在；每系统恰好调度一次；**同阶段写冲突拒绝** |
| 原型 | `count ≥ 1`；组件存在且不重复；`initial_values` 长度与字节数校验 |
| 查询唯一性 | 至多一个查询携带 `RenderData` 过滤器 |

生成器另有两条：系统名生成的入口点不得与保留名（`system_prep_count` /
`system_prep_scan` / `system_prep_fill`）冲突；设备限额不足报 `EcsError::Limits`。

---

## 7. 代码生成（WGSL）

`generator::generate_wgsl(&ir) -> Result<String, EcsError>` 产出单个模块，结构：

```
// Generated by neon-gpu-ecs. Do not edit.
struct QueryRange { start: u32, count: u32 }
struct StructuralCommand { kind: u32, a: u32, b: u32, reserved: u32 }
struct RenderInstance { transform: vec4f, color: vec4f }

// group(0)：全部 storage，read_write（布局见第 8 节）
// group(1)：uniform 资源 + renderInstances

fn ecs_pass_0(e: u32) -> bool { … }     // 每查询一个谓词函数
fn ecs_pass_1(e: u32) -> bool { … }

@compute @workgroup_size(64) fn system_prep_count(…)   // 分拣①
@compute @workgroup_size(1)  fn system_prep_scan()     // 分拣②
@compute @workgroup_size(64) fn system_prep_fill(…)    // 分拣③

@compute @workgroup_size(64) fn system_<name>(…)       // 每系统一个入口点
```

系统入口点固定骨架（压实迭代契约）：

```wgsl
let ecs_cmd   = framePrepBuffer[SLOT];          // SLOT = 系统的 query_id
let ecs_index = ecs_gid.x;
if (ecs_index >= ecs_cmd.count) { return; }
let ecs_entity = compactedEntityIds[ecs_cmd.start + ecs_index];
let ecs_slot   = ecs_index;                      // RenderData 实例槽
```

同一实体可能被多个查询命中，因此 `compactedEntityIds` 中同一实体可出现多次；
`Store` 的版本号递增是原子的，多查询并发写安全。

---

## 8. 绑定组布局

### group(0)（全部 `storage, read_write`，共 `8 + 3n` 个）

| 槽位 | 名字 | 内容 |
| --- | --- | --- |
| 0 | `entityActive` | `array<u32>`，1=存活 |
| 1 | `queryCounts` | `array<atomic<u32>>`，每查询计数（scan 后自动清零） |
| 2 | `queryCursors` | `array<atomic<u32>>`，fill 散射游标 |
| 3 | `framePrepBuffer` | `array<QueryRange>`，每查询 `{start, count}` |
| 4 | `compactedEntityIds` | 压实后的实体 ID 串 |
| 5 | `indirectArgs` | `array<vec3u>`，间接调度参数（**16 字节/项**） |
| 6 | `commandBuffer` | `array<StructuralCommand>`，结构变更命令环 |
| 7 | `commandCount` | `atomic<u32>`，环内命令数 |
| 8+3c | `ecs_c{c}` | 组件 c 的数据（SoA） |
| 9+3c | `ecs_cv{c}` | 组件 c 的当前版本 `array<atomic<u32>>` |
| 10+3c | `ecs_cb{c}` | 组件 c 的基线版本 `array<atomic<u32>>` |

### group(1)

| 槽位 | 内容 |
| --- | --- |
| `ResourceDef.binding_slot`（< 30） | `var<uniform>` 资源 `ecs_r{id}` |
| 30（固定，保留） | `renderInstances : array<RenderInstance>`，`read_write` |

命令环采用 **ping-pong 双缓冲**：两个环各建一套 group(0)，运行时按相位选择，
系统永远写入当前相位环，回放永远读另一相位环。

---

## 9. 缓冲清单与尺寸

`n` = 组件数，`q` = 查询数，`M` = `max_entities`，`C` = `command_capacity`。

| 缓冲 | 尺寸 | 用途标志 |
| --- | --- | --- |
| `entityActive` | `M × 4` | STORAGE/COPY_SRC/COPY_DST |
| `queryCounts` / `queryCursors` | `q × 4` | 同上 |
| `framePrepBuffer` | `q × 8` | 同上 |
| `compactedEntityIds` | `M × 4 × max(q,1)`（**查询重叠**） | 同上 |
| `indirectArgs` | `q × 16`（vec3u stride） | 同上 |
| `indirectExec` | `q × 16` | **INDIRECT + COPY_DST**（专用） |
| `commandBuffer` ×2 | `C × 16` | STORAGE/COPY_SRC/COPY_DST |
| `commandCount` ×2 | `4` | 同上 |
| 组件数据 ×n | `M × stride`（vec3f 为 16） | 同上 |
| 版本/基线 ×2n | `M × 4` | 同上 |
| 资源 ×r | `ty.byte_size()` | UNIFORM + COPY_DST |
| `renderInstances` | `M × 32` | STORAGE + COPY_SRC |

所有缓冲最小 4 字节（WebGPU 禁止零尺寸）。

---

## 10. 运行时接口（`GpuEcsCtx`）

### 构造与初始化

```rust
GpuEcsCtx::new(
    device: wgpu::Device,       // 宿主注入的克隆
    queue: wgpu::Queue,
    ir: EcsIr,                  // 移动进上下文，ctx.ir 只读可查
    max_entities: u32,          // 实体表容量（所有缓冲按此分配）
    command_capacity: u32,      // 每帧结构变更命令上限
) -> Result<Self, EcsError>     // 校验/限额/编译失败立即返回

ctx.seed_initial();             // 上传原型种群、版本种子（基线=当前=1 或 0）、资源默认值；
                                // 同时初始化 CPU 侧活跃表与空闲槽队列
```

### 每帧

```rust
ctx.set_resource(id: u32, bytes: &[u8]);  // 上传 uniform 资源（字节数必须精确）
ctx.run_frame();                          // 完整帧：回放→分拣→快照→执行→换相位
ctx.flush();                              // 提交空批次，冲刷 write_buffer 暂存（可选）
ctx.run_sort();                           // 仅分拣（调试/测试用）
```

### 回读（阻塞，自带 staging + map_async + poll）

```rust
ctx.read_frame_prep() -> Vec<QueryRange>        // 每查询 {start, count}
ctx.read_compacted_ids() -> Vec<u32>            // 全部压实段（按 start/count 切片）
ctx.read_indirect_args() -> Vec<[u32; 3]>       // 每查询 workgroup 三元组
ctx.read_component_data(cid) -> Vec<u8>         // SoA 原始字节（含 stride 填充）
ctx.read_component_versions(cid) -> Vec<u32>    // 当前版本号
ctx.read_buffer_blocking(&buffer, size) -> Vec<u8>  // 任意缓冲通用回读
```

### 诊断与直接访问

```rust
ctx.shader_module() -> &wgpu::ShaderModule
ctx.bind_group_layouts() -> &[wgpu::BindGroupLayout; 2]
// 公开字段（高级用法）：
//   device, queue, ir, max_entities, command_capacity,
//   bind_group0: [BindGroup; 2], bind_group1,
//   entity_active, query_counts, query_cursors, frame_prep,
//   compacted_ids, indirect_args, indirect_exec,
//   component_buffers: Vec<(data, current_ver, baseline)>,
//   resource_buffers, render_instances
```

### 生成器独立入口（不建运行时也能用）

```rust
generator::generate_wgsl(&ir) -> Result<String, EcsError>
generator::check_schedule_conflicts(&ir) -> Result<(), EcsError>
generator::check_limits(&ir, max_storage_buffers) -> Result<(), EcsError>
generator::required_group0_bindings(&ir) -> u32        // 8 + 3n
```

---

## 11. 帧生命周期

`run_frame` 的精确顺序（单个提交）：

1. **回放**（第 2 帧起）：同步回读上一相位环的 `commandCount` 与命令，逐条在 CPU 执行
   （见第 14 节），然后清零该环计数。
2. **分拣**：`count → scan → fill` 三个 dispatch 在同一 compute pass 内（隐式屏障）。
   分拣比较的是**当前版本**与**上一帧留下的基线**。
3. **基线快照**：`copy_buffer_to_buffer(current → baseline)` 逐组件。
4. **间接参数搬运**：`indirectArgs → indirectExec`（规避用途冲突，见第 18 节）。
5. **系统执行**：按 `schedule.stages` 顺序，每系统一个 compute pass，
   `dispatch_workgroups_indirect(&indirect_exec, 16 * query_id)`。
6. **相位交换**：`cmd_phase` 翻转，帧计数 +1。

语义要点：

- 第 N 帧发出的结构变更命令在**第 N+1 帧开头**（分拣之前）生效。
- 空查询的间接调度参数是 `[0, 1, 1]`，调度 0 个 workgroup，安全无副作用。
- 命令环容量耗尽时，GPU 端多余命令被丢弃（`if (ci < arrayLength)` 保护）。

---

## 12. 分拣流水线

| 入口点 | 线程模型 | 职责 |
| --- | --- | --- |
| `system_prep_count` | 每实体一线程 | 活跃实体逐查询求谓词，`atomicAdd(&queryCounts[q])` |
| `system_prep_scan` | 单线程 | 独占前缀和 → 写 `framePrepBuffer`/`queryCursors`，清零 `queryCounts`，导出 `indirectArgs = ceil(count/64)` |
| `system_prep_fill` | 每实体一线程 | 再求同一谓词，`atomicAdd(&queryCursors[q])` 占槽，写 `compactedEntityIds` |

- 查询谓词 `ecs_pass_q`：`with`（当前版本≠0）→ `without`（==0）→ 过滤器（见第 13 节）；
  `RenderData` 过滤器不加条件。
- `fill` 用原子游标散射，**查询内实体顺序不确定**；跨查询段按扫描顺序连续排布。
- `scan` 把计数累积在**局部数组**再写 `indirectArgs`，避免同一调用内对同一
  storage 缓冲"写后读"在部分驱动上的不可靠行为。
- 分拣结果与 `runtime::init` 的纯 CPU 参考实现逐一对账（见 `gpu_integration.rs`）。

CPU 参考实现（`runtime::init`，纯函数，也被运行时用来播种）：

```rust
init::prototype_entity_ranges(&ir)      // 每原型实体 ID 区间
init::prototype_entity_total(&ir)       // 总实体数
init::initial_query_match(&ir, q)       // 初始种群下查询命中集（Changed/Added 恒不通过）
init::initial_entity_active(&ir, M)     // entityActive 字节
init::initial_component_bytes(&ir, c, M)// SoA 字节（含 16 字节 stride）
init::initial_version_bytes(&ir, c, M)  // 版本字节（1=持有）
```

---

## 13. Changed / Added 变更检测

版本缓冲是变更检测的唯一事实来源：

```
帧 N 分拣：比较 current 与 baseline（= 帧 N-1 快照）
帧 N 快照：baseline ← current
系统 Store：atomicAdd(current, 1)
```

| 过滤器 | 谓词 | 含义 |
| --- | --- | --- |
| `Changed(c)` | `baseline[c] != 0 && current[c] != baseline[c]` | 上一分拣点以来被写过 |
| `Added(c)` | `baseline[c] == 0 && current[c] != 0` | 上一分拣点以来首次出现（含 AddComponent/Spawn） |

行为示例（`execution.rs` 已验证）：写者连续写 3 帧后停止 → `Changed` 查询命中 3 帧，
之后计数归 0 并保持（检测器自然"停摆"）。首帧因播种时 `baseline == current`，
`Changed`/`Added` 一律不命中。

---

## 14. 结构变更

### 14.1 命令记录（`StructuralCommand`，16 字节）

```
{ kind: u32, a: u32, b: u32, reserved: u32 }
```

| kind | 常量 | a | b |
| --- | --- | --- | --- |
| 0 | `COMMAND_KIND_SPAWN` | 原型索引 | 0 |
| 1 | `COMMAND_KIND_DELETE` | 实体 ID | 0 |
| 2 | `COMMAND_KIND_ADD_COMPONENT` | 实体 ID | 组件 ID |
| 3 | `COMMAND_KIND_REMOVE_COMPONENT` | 实体 ID | 组件 ID |

GPU 端通过 `CallBuiltin` 追加：`atomicAdd(&commandCount)` 占槽后写入环。

### 14.2 CPU 回放语义（下一帧开头）

- **Spawn**：从空闲槽队列取**最小**空闲槽 → `entityActive=1` → 按原型写初始值
  （`initial_values` 或 `default_value`）→ 涉及组件版本置 1（基线同为 1，故新实体
  不会被误判 `Changed`）。**无空闲槽时丢弃**（世界已满）。
- **Delete**：`entityActive=0` → 全部组件当前/基线版本清零（防止僵尸匹配与
  重生后误判 `Changed`）→ 槽位归还。
- **AddComponent**：写组件默认值 + 当前/基线版本置 1。
- **RemoveComponent**：当前/基线版本置 0（数据保留，版本为 0 即视为无该组件）。
- 回放对非活跃实体、越界实体/组件直接忽略（防御式）。

### 14.3 双环保证

系统写环 `phase`，回放读环 `1-phase`，帧末翻转——被回放的环永远不是正在被写入的环，
回读与写入无重叠；每环回放后计数清零（槽内容受计数门控，无需擦除）。

---

## 15. RenderData 实例缓冲

- 恰好一个查询携带 `QueryFilter::RenderData`；该查询**所有活跃实体**无条件通过，
  实体的实例槽 `ecs_slot = ecs_index`（压实段内连续），因此 `renderInstances` 前
  `count` 项始终稠密。
- 该查询上的系统用 `StoreRender { src: v_slot, field }` 写入
  `RenderInstance.transform` 或 `.color`（均为 `vec4f`）。
- `framePrepBuffer[slot].count` 即实例数，直接可作下游 `DrawIndexedIndirect` 的
  `InstanceCount`（渲染管线接入是后续工作）。
- `RenderInstance` 布局：`{ transform: [f32;4], color: [f32;4] }`，32 字节，`bytemuck::Pod`。

---

## 16. 错误码

`EcsError`（稳定机器码见 `code()`）：

| 变体 | `code()` | 典型来源 |
| --- | --- | --- |
| `IrInvalid` | `ecs_ir_invalid` | `validate()` 收集的全部问题 |
| `ScheduleConflict` | `ecs_schedule_conflict` | 同阶段两系统写同一组件 |
| `WgslInvalid` | `ecs_wgsl_invalid` | 类型推断失败、混合类型运算、读未赋值局部、保留入口点名、跳转目标非块起点 |
| `Limits` | `ecs_limits_insufficient` | 设备存储缓冲数 < 8+3n（错误信息含所需数量） |
| `Gpu` | `ecs_gpu_error` | GPU 侧失败（预留） |

---

## 17. 设备要求与集成约束

1. **唯一调用方**：本 crate 是 compute-only，宿主（`neon-wgpu-runtime`）注入
   device/queue 克隆；这是 AGENTS.md"唯一 GPU owner"边界的组成部分。
2. **限额**：设备必须满足
   `max_storage_buffers_per_shader_stage ≥ 8 + 3 × 组件数`。
   ⚠️ `neon-wgpu-runtime` 当前请求 `Limits::default()`（每阶段 8 个），**接入前必须提高**。
   `GpuEcsCtx::new` 会在构造期检查并给出带具体数字的错误。
3. **无需额外 features**：全部功能仅用 WebGPU 基线（原子、间接调度、多入口点）。
4. **容量参数**：`max_entities` 决定所有缓冲尺寸；`command_capacity` 决定每帧结构变更
   上限；两者在构造后不可变。
5. **后续集成路线**（未实现）：`renderInstances` 接 `DrawIndexedIndirect`（Stage 4 绘制）；
   `ecs.*` RPC 协议面；命令环溢出的诊断事件。

---

## 18. 已知坑与注意事项

都是实际踩过的，写新代码前先读这一节：

1. **`array<vec3u>` 步长是 16 字节**（vec3 对齐），不是 12。`indirectArgs` 的分配、
   回读、`dispatch_workgroups_indirect` 的 offset 全部按 16 计算。
2. **STORAGE 与 INDIRECT 用途互斥**：同一缓冲不能既被绑为 storage（分拣写）又被
   当作间接调度源。解法：`indirectArgs`（storage）每帧拷贝到专用 `indirectExec`
   （仅 INDIRECT）。
3. **压实容量要乘查询数**：多个查询可命中同一实体，`compactedEntityIds` 容量是
   `max_entities × n_queries`。
4. **scan 不能读自己刚写的 storage**：计数累积放局部数组，避免同调用写后读的
   驱动不可靠行为。
5. **`compactedEntityIds` 查询内顺序不确定**（原子散射），断言须排序后比较。
6. **`Store` 会递增版本号**——这是 `Changed` 检测的基础；不写版本号的"静默写"不存在。
7. **结构变更延迟一帧生效**（帧 N 发命令 → 帧 N+1 开头回放），写测试时别在第一帧断言。
8. **积分器语义**：物理示例每帧先 `vel *= dt` 再 `pos += vel`，dt<1 时位移不是线性累加。
9. **wgpu 30 API 形状**：`set_bind_group(0, Some(&bg), &[])`；
   `device.poll(PollType::Wait { submission_index: None, timeout: None })`；
   `slice(..).map_async(mode, cb)`；`get_mapped_range()` 返回 `Result`，读完要
   `drop(view)` + `unmap()`。
10. **`initial_values` 作用于整个种群**，不是逐实体。

---

## 19. 测试地图

66 个测试，全部 `cargo test -p neon-gpu-ecs` 通过；GPU 相关测试为 headless 真实设备
（`gpu-ready` 层级，非渲染验收）。

| 文件 | 覆盖 |
| --- | --- |
| `tests/ir_validate.rs`（18） | 序列化往返、字节校验、非法引用、写冲突、命名规范 |
| `tests/generator_snap.rs`（11） | 绑定文本契约、分拣文本、直线系统体、原子/Bool 访问、混合类型拒绝、headless `create_shader_module` 编译 |
| `tests/control_flow.rs`（11） | if/else、计数循环、fallthrough、嵌套 if 的状态机降译 + 4 个模式 headless 编译 |
| `tests/gpu_integration.rs`（4） | 分拣对 CPU 参考、重复分拣稳定性、限额拒绝、参考实现手算对账 |
| `tests/execution.rs`（7） | 物理积分对参考、资源速率、Changed 三阶段（命中→冻结）、Added、空调度、版本记账 |
| `tests/structural.rs`（4） | Spawn 回放、双环交换、回放幂等、Delete 回收 |
| `tests/render_data.rs`（3） | 实例缓冲填充、计数联动、数据新鲜度 |

公共夹具：`src/tests_support.rs::physics_world()`（Transform+Velocity+Health+DeltaTime
的最小合法世界，单元/集成测试共用）。

验收层级按 `AGENTS.md` 第 21 节：M1–M3 为 `contract-ready`，M4–M7 为
`gpu-ready`（headless）。**未宣称** `wgpu-rendered` / `interactive-accepted`。

---

## 20. License

MIT OR Apache-2.0
