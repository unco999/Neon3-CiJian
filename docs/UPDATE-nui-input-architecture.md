# Neon3 NUI Input 架构升级 — 更新说明

**日期**: 2026-09-12
**版本**: v0.2.7 (开发中)

## 概述

本次升级解决了 NUI Input 系统的核心限制：原系统只有 11 种标量类型，不支持 Struct/Array，全量重打包，无派生表达式。现已完成 4 个阶段的底层能力修复。

## 已完成阶段

### 阶段1: GPU 类型补齐 + 增量上传 (commit de7e565)
- `pack_inputs` 补齐 Vec2(8B)/Vec4(16B)/Color(16B)/AssetHandle(12B) GPU 打包
- NUI Flow 新增 `vec2`/`vec4`/`color` kind
- `pack_changed_slots` 基于 changed_slots 做 partial write_buffer
- 12 个 GPU 字节布局测试 + 5 个 NUI Flow 解析测试

### 阶段2: Struct 类型全链路 (commits 41c5d55, b5c344a, 02493f1)
- `UiInputKind::Struct` + `UiInputValue::Struct`，字段按 BTreeMap 字母序排列
- `gpu_slot_count()` 递归计算，`flatten_value()` 递归展开为连续 16 字节 slot
- NUI Flow 多行 struct 声明: `input player struct { hp f32 default 100, mp f32 }`
- 字段级绑定: `text label value $player.name`，运行时点路径提取
- `pack_changed_slots` 支持 `"player.hp"` 字段级增量更新
- 5 个 Struct GPU 测试 + 6 个 NUI Flow 测试

### 阶段3: Array 类型 (commits c60987d, 0c0fee2, dc4d07d)
- `UiInputKind::Array { element_kind, length }` + `UiInputValue::Array`
- GPU 展开: 元素按顺序排列，每元素占 element_kind.slots 个 slot
- NUI Flow 语法: `input grid array[36] f32`
- 数组索引绑定: `$grid[0]`、`$slots[0].count`（运行时检查越界）
- 3 个 Array GPU 测试 + 6 个 NUI Flow 测试

### 阶段4: 简单派生表达式 (commit dc4d07d)
- 语法: `input low_hp bool = $hp < 0.3`
- 运算符: `==` `!=` `>` `<` `>=` `<=`
- 操作数: `$variable`（支持索引/字段路径）或字面量（f32/i32/bool）
- `UiInputSlot.derived_expression` 字段存储表达式
- `replace_resolved_inputs` 时自动重算所有派生值
- 3 个表达式解析测试

## 测试案例: grid-pulse (commit 60ca49e)

`cases/grid-pulse/` — 6x6 高频变化网格案例

- **生成器**: `node generate.js` 产出 `grid-pulse.nui`
- **规模**: 36 个格子 + 1 个 array input(36 f32) + 36 个派生表达式
- **绑定**: 每个格子 `visible $cell_i_on`，其中 `cell_i_on = $grid[i] > 0.5`
- **验证**: `grid_pulse_case_parses` 测试确认 37 inputs / 36 derived exprs / 36 bindings

## 测试结果

| 套件 | 通过 | 总数 |
|------|------|------|
| ui-runtime (lib) | 144 | 144 |
| wgpu-runtime GPU | 20 | 20 |

## 已知限制

1. **派生表达式运行时注入**: `UiProgramSemanticEventRouter` 需调用 `set_input_schema()` 才能启用派生值重算。实际运行时路径（UiRuntime → router）的 schema 注入尚未接入。
2. **Array of Struct NUI Flow 声明**: schema/GPU 层支持结构体数组，但 NUI Flow 解析器目前只支持 `array[N] <scalar_kind>`，不支持 `array[N] struct {...}`。
3. **pack_changed_slots 数组索引**: GPU 端 `resolve_field_path` 目前只支持 Struct 点路径，不支持 `"slots[0].count"` 数组索引路径的增量更新。
4. **Opacity 不支持绑定**: NUI Flow 中 `opacity` 属性只接受字面量，不支持 `$var`。可用 `visible` 替代。

## 文件清单

- `crates/neon-ui-schema/src/lib.rs` — Array/Struct/derived_expression 类型
- `crates/neon-wgpu-runtime/src/ui_program_gpu.rs` — GPU 展开 + 增量打包
- `crates/neon-ui-runtime/src/nui_flow.rs` — 解析器（array/struct/derived/索引绑定）
- `crates/neon-ui-runtime/src/lib.rs` — 运行时绑定解析 + 派生表达式求值
- `cases/grid-pulse/` — 测试案例生成器 + 产出
