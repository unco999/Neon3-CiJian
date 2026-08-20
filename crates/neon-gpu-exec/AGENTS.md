# neon-gpu-exec — GPU 脚本执行器

## 目标
`neon-gpu-script` 编译出的 `CompiledScript/CompiledScene`（IR 层）在此按层序 dispatch 到
`neon-gpu` 的 headless Device 上执行，并读回 `export` 声明的世界资源。

## 架构
- `codelet.rs`：`Codelet` trait（`input_count`、`allowed_consts`、`accepts`(默认真)、`wgsl`）、
  `FieldTy`（F32/U32，`.bytes()`）、`ConstArg`（`as_f32`/`as_str`）、`split_args`（位置参数中
  分离 value 与 const）。
- `executor.rs`：`Executor`（`new`、`register_codelet`、`run`）、`InputField{buffer,per_entity,ty}`。
  - `run(scene, inputs)`：按 `plan.layering` 分层，一层一个 command wave，**全部 wave 录制进同一个
    encoder 一次性 submit，中间零读回**；层内按 kernel 分组共享 pipeline。
  - 每节点一个独立输出 storage buffer（简单分配；liveness/scratch 复用是后续阶段）。
  - pipeline 缓存 key = `kernel#n#value_count#consts`，常量烘焙进 WGSL。
  - `infer_entity_count`：先全量检查 input 存在（MissingInput），再按 buffer.size/per_entity 一致推断 n。
  - 读回走 `neon-gpu::hal_map`（MAP_READ|COPY_DST 独立 buffer，copy buffer→readback→map 等待→cast）。
  - 输出 map key = 世界资源限定名 `domain.name`。
- `error.rs`：`ExecError`（MissingInput、EmptyBuffer、UnknownKernel、UnknownInput、Readback、BadConst）。

## 关键实现约定
- WGSL 必须用入口参数 `fn main(@builtin(global_invocation_id) gid: vec3<u32>)`，不能裸用
  `global_invocation_id`。
- **严禁依赖 u32 乘法溢出语义**：WGSL 整数溢出是未定义行为，DX12(naga) 与 Rust wrapping 结果可能不同。
  随机 hash 一律用移位+XOR 实现（`h ^= h<<13; h ^= h>>17; h ^= h<<5;`），`<<`/`>>`/`^`/`&` 在 WGSL 是定义行为。
- `select(a, b, mask)` 语义 = **mask>0.5 选 b**（命中标记 → 命中后的值）。
- 浮点字面量后缀 `{c}f` 依赖 naga 宽容解析；新 codelet 优先写成 `{c}` 显式类型。

## 验证
- `tests/crit_combat.rs`：5 个 Codelet（DamageFormula/RngChance/Mul/Select/ApplyDamage）+ CPU 参考
  `crit_roll`（同一移位 hash）+ 2 测试（GPU 端到端读回 `target.hp` 等于 CPU 期望；缺失输入别名报
  `MissingInput`）。
- 全 workspace：`cargo test` 全绿（含 neon-gpu 121 个 GPU 集成测试）。

## 演进顺序
Phase 3：liveness + scratch 分配（复用中间 buffer）；`for`/`until_converge` 语法与 unroll；
type 推导与 `field<f32,[8]>` 逐字段访问；GPU 内多场景（multi-scene）串联。