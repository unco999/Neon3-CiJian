# Neon3 GPU 脚本垂直切片完成记录

日期: 2026-08-20

## 达成目标

`crates/neon-gpu-exec` 垂直切片完成：crit_combo 脚本 → `neon-gpu-script` 编译（19 测试绿）
→ `neon-gpu-exec` 分层 dispatch → headless GPU 执行 → 读回断言正确。workspace 全量回归
零失败。

## 链路验证内容

- 脚本语言层（已在此前会话完成，19 测试全绿）：
  - `scene crit_combo = { input / output / body }` 语法、SSA 强制、纯数据流。
  - 世界资源注册表 `WorldRegistry`（`domain.name` 限定名）、kernel 注册表（位置参数数 + params 白名单）。
  - 嵌套调用提升匿名 `%n` 节点、位置常量 key `#n`、`kind="physical"` 字符串枚举常量。
  - 编译期 `WriterConflict`、`UndeclaredOutput`、`ReadOnlyOutput` 校验。
- 执行器层（本次完成）：
  - 5 个 Codelet（DamageFormula/RngChance/Mul/Select/ApplyDamage）+ CPU 参考 `crit_roll`。
  - `Executor::run` 按 plan 层序 dispatch，全部 wave 录制进同一 encoder 一次 submit，中间零读回。
  - pipeline 缓存 key = `kernel#n#value_count#consts`，常量烘焙进 WGSL。
  - 读回走 `neon-gpu::hal_map`；输出 map key = 限定名 `target.hp`、`frame.crit`。
  - N=4：GPU 读回 `target.hp` == CPU 参考值；缺失 input 别名报 `MissingInput`。
  - `InputField` 需在 lib.rs 显式 `pub use` 才能被集成测试引用。

## 本次修掉的 5 个 bug（教训）

1. **WGSL 缺入口参数声明**：不能裸用 `global_invocation_id.x`，必须声明
   `fn main(@builtin(global_invocation_id) gid: vec3<u32>)` 再用 `gid.x`。
2. **WGSL u32 乘法溢出是未定义行为**：naga/DX12 后端与 Rust wrapping 结果不一致，导致
   随机 hash 在 CPU/GPU 两侧判定相反（先全暴击后全不暴击的假象）。**禁止依赖 u32 乘法溢出**；
   随机 hash 改用移位+XOR（`h ^= h<<13; h ^= h>>17; h ^= h<<5;`），`<<`/`>>`/`^`/`&` 是定义行为。
   前一次失败是 `seed*2654435761u+12345u` 溢出；换乘法版后 CPU/GPU 仍不一致（expected 42 vs
   gpu 34），再用第二个版本（纯移位 XOR）并 export `frame.crit` 诊断确认 rng 输出与 CPU 一致，
   最终定位到真正的 bug 是 select 语义。
3. **Select 语义**：脚本约定 `select(a, b, mask)` = mask>0.5 时选 **b**（命中标记 → 命中后的值）；
   实现写反成选 a，导致 mask 全 0 时全走暴击分支。诊断方式：export `frame.crit` 读回 [0,0,0,0] 与
   CPU 一致，但 hp 全为 2x → 锁定 select。
4. **format! 嵌套占位符**：`format!("...{seed_decl}...", n=n)` 不会递归替换 seed_decl 里预先拼好的
   `{n}`，产生 `array<u32, {n}>` 字面量导致 shader 解析失败；需在 seed_decl 内部直接 format 好 n。
5. **错误优先级**：`infer_entity_count` 应先全量检查 input 别名存在（MissingInput）再做 buffer 尺寸
   一致性校验，否则尺寸不匹配的 buffer 会先报 Readback 而不是 MissingInput。

## 其他要点

- 诊断时给脚本加 `export frame.crit = crit`（世界资源注册为 writable、output 声明加 frame.crit）
  即可直接读回中间量定位问题——可观测性优先的实践。
- `{c}f` 浮点后缀字面量依赖 naga 宽容解析；新 codelet 建议写成 `{c}` 并显式类型。
- 修 `bytemuck` 需放到 [dependencies]（executor 运行时用，不只是 dev）。
- `self.pipelines.entry(key).or_insert_with(|| self.build_pipeline(...))` 有借用冲突（闭包内再借 self），
  改为 contains_key + 先建后插。

## 后续方向（Phase 3+）

- liveness + scratch 分配：复用中间 buffer，替代每节点独立 buffer。
- `for` / `until_converge` 语法与 unroll 编译期展开。
- type 推导与 `field<f32,[8]>` 逐字段访问（stats[0]）。
- multi-scene 串联（crit_combo 输出直接作为下一场景输入）。