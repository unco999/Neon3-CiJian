# Panel 样式与动画能力差距分析 + bevy-nui-host 全功能评估报告

日期: 2026-08-22
性质: 调研报告（无代码改动）
调研对象:
- `D:\Neon3`（Neon3 工作区：schema/runtime/renderer）
- `D:\bevy-nui-host`（外部 Bevy host，独立仓库）
- 全功能组件展示: `crates/neon-ui-runtime/tests/fixtures/ui/imgui-component-gallery.nui`

---

## 一、结论摘要

1. **Neon3 当前 panel 样式是"最小可用集"**：只有背景色、边框色、边框宽、圆角、透明度五项，且 NUI Flow 语法层**只能声明 `fill`/`line`**，`border_width`/`corner_radius`/`opacity` 只能写在 state machine style 里，节点上不能直接写。
2. **动画能力是"单向短动画"**：只支持 state machine 驱动的单段 `from -> to` 过渡，线性/RGB 插值，四种二次 easing；没有 transform/scale/rotate、没有 spring、没有 keyframe、没有 stagger、没有 repeat、没有入场/退场动画。
3. **bevy-nui-host 不是全功能 NUI 渲染器，也不是全功能 NUI 的实现者**：它是"消费渲染结果 + 提供游戏世界数据"的 host。NUI 的解析、编译、状态机、布局、绘制全部由 Neon3 侧（ui-runtime + wgpu-runtime）完成，bevy-nui-host 只是通过长度前缀 JSON-over-TCP 调用 Neon3 的 RPC，并导入 D3D12 共享纹理做最终合成。
4. **组件 gallery 的 NUI 目前 bevy-nui-host 不能全部支持**：bundle-host 当前只跑一个极简 `combined-ui` 状态面板（health/mana/level/progress_bar/button），数据网格、下拉、tabs、列表、拖拽、tooltip、dialog、checkbox/radio/slider/drag_value 等控件虽然 Neon3 renderer 都实现了，但 bevy host 侧没有完整接入这些控件的语义事件和变量回写，也没有渲染它们的实际场景；能显示不代表能交互闭环。
5. **bundle 能力上限是 256 world quads**（`world_ui_pipeline.rs:71` 等），超出直接 panic，这是横向扩展的最大硬限制之一。

---

## 二、当前 Panel 样式能力（Neon3）

### 2.1 UiStyle 实际字段（`crates/neon-ui-schema/src/lib.rs`）

| 属性 | 类型 | 默认值 | NUI Flow 语法 | Rust 可设 |
|------|------|--------|---------------|-----------|
| `background_color` | `[f32;4]` | `[0.12, 0.14, 0.18, 1.0]` | `fill #RRGGBB[AA]` | ✅ |
| `border_color` | `[f32;4]` | `[0.34, 0.42, 0.52, 0.7]` | `line #RRGGBB[AA]` | ✅ |
| `border_width` | `f32` | `1.0` | ❌ 节点上不可写，仅 state style 可写 | ✅ |
| `corner_radius` | `f32` | `4.0` | ❌ 节点上不可写，仅 state style 可写 | ✅ |
| `opacity` | `f32` | `1.0` | ❌ 节点上不可写，仅 state style 可写 | ✅ |

### 2.2 布局属性（`UiLayout`）

| 属性 | Flow 语法 | 状态 |
|------|-----------|------|
| row/column/overlay | ✅ | 已支持 |
| w/h/minw/maxw/grow/shrink/basis | ✅ | 已支持 |
| pad（统一四向） | ✅ | 已支持，但**不支持独立方向 padding** |
| margin | ❌ | 结构存在，Flow 语法不可写 |
| gap/align/justify | ✅ | 已支持 |
| clip（none/bounds/rounded/scroll） | ✅ | 已支持 |
| scroll_offset | ❌ | 仅 Rust |
| align_self | ❌ | 仅 Rust |

### 2.3 样式缺失清单

| 能力 | 严重性 | 现状 |
|------|--------|------|
| 渐变填充 | 高 | **缺失**，只有纯色 |
| 阴影/box-shadow | 高 | **缺失**，无额外 render pass |
| 模糊/毛玻璃 | 中 | **缺失** |
| outline | 低 | **缺失**（只有 border） |
| transform/rotate/scale | 高 | **缺失**，无 2D 变换矩阵 |
| 文字颜色 | 高 | **缺失**，文字颜色由默认样式推导，不可单独设置 |
| 字体大小/字重 | 高 | **缺失**，文字大小由 `bounds.height` 隐式推导 |
| 背景图 | 中 | 可用 `image resource` 但属于资源节点，不是 background-image 样式 |
| theme token（ink/token） | 中 | **语法已解析但未实现**，无 token 解析/替换/注入 |
| responsive layout | 高 | **缺失**，无 media query / viewport 条件 |

---

## 三、当前动画能力（Neon3）

### 3.1 已支持

| 项目 | 支持情况 |
|------|----------|
| 动画属性 | `bounds`、`background_color`、`border_color`、`border_width`、`corner_radius`、`opacity`、`numeric_value`（progress bar 类） |
| 插值 | CPU 和 GPU 双侧线性插值；颜色为**线性 RGBA lerp**（非 HSL，非 sRGB-correct） |
| easing | `Linear`、`EaseIn`（t²）、`EaseOut`（1-(1-t)²）、`EaseInOut`（分段二次）——都是二次曲线 |
| 驱动方式 | 仅 state machine transition（`sync`/`on` + `transition ... motion <key>`） |
| GPU 推进 | shader 内 `animation_progress` 根据 time uniform 推进，避免每帧 CPU 采样 |
| 多播 | ✅ 一个 transition 可为多个节点（面板 + 子控件）附加 `enter_transition`；多个面板 motion 按 `scope_key` 独立保留 |
| 中断/retarget | ✅ 目标变化时从当前采样值 retarget；已到目标则跳过，避免 no-op 重启 |

### 3.2 缺失清单

| 能力 | 严重性 | 现状 |
|------|--------|------|
| transform/rotate/scale 动画 | 高 | **缺失**（连静态 transform 都没有） |
| spring physics 弹性 | 高 | **缺失** |
| 入场/退场动画（fade-in/slide-in） | 中 | **缺失**，无声明式入口 |
| keyframe 多段动画 | 中 | **缺失**，只有单段 from->to |
| stagger 子节点依次延迟 | 中 | **缺失** |
| repeat/infinite 循环 | 低 | **缺失** |
| cubic-bezier 自定义曲线 | 中 | **缺失** |
| 多属性不同 easing | 中 | **缺失**，所有属性共享同一 easing |
| 独立动画触发 API | 中 | **缺失**，只能靠 state machine 触发 |
| hover/pressed/focus 伪类样式 | 中 | renderer 有硬编码 hover/pressed 亮度增强，但 Flow 不可声明 |

### 3.3 特别说明

- 全功能 gallery fixture `imgui-component-gallery.nui` **没有** 使用 state machine / motion / transition / style，说明这些是独立于控件展示能力的"presentation 层"。
- `nui-flow-ai-authoring.md` 只约束"新功能必须声明式、不能有脚本"，因此上面缺失的样式/动画能力**都是可以在不破坏边界的前提下增加的**（语法 + schema + renderer 三层配合）。

---

## 四、bevy-nui-host 架构判定

### 4.1 它是什么

根据工作区文档（`plan/neon3-world-ui-unified.md`、`plan/phase2-bevy-nui-host-work-requirement.md`、`docs/nui-flow-reference-and-bevy-status-debug.md`、`.workbuddy/memory/*`）：

```text
bevy-nui-host（外部仓库）
  ├─ src/lib.rs          (约 136KB, 主逻辑)
  ├─ src/main.rs         (26KB, monster 场景 + monster_field_flow() 动态生成 NUI 文本)
  ├─ src/dx12_consumer.rs(导入 Neon3 导出的 D3D12 共享纹理)
  ├─ src/gpu_readback.rs (GPU readback, ID target 像素采样)
  └─ src/bin/phase2_latest_probe.rs
```

它本质上是：
1. **Neon3 的协议 client**：使用 Neon3 workspace 的 `RpcRequest`/`RpcResponse`/`CameraFrame`/`WorldUiAnchorBatch` 等类型，通过 length-prefixed JSON over loopback TCP 连接 `neon-wgpu-runtime --headless-server`。
2. **共享 surface 消费者**：`dx12_consumer.rs` 导入 Neon3 导出的 D3D12 shared texture（+fence），Import→Sample→Release，做外部合成。
3. **游戏世界数据提供者**：把 Bevy 相机 frame、world anchor、entity 变量（health/mana/level）提交给 Neon3，让 Neon3 做视锥剔除、投影和 UI 绘制。
4. **交互回写者**：采样 ID target -> `ui.host.inbound` 语义事件 -> 接收变量回写。

### 4.2 它不是"全功能 NUI 实现者"

NUI 的核心能力（解析、编译、状态机、布局计算、绘制、命中、动画）**全部在 Neon3 侧**：

| 能力 | 谁实现 |
|------|--------|
| NUI Flow 解析/编译/校验 | `neon-ui-runtime`（Neon3） |
| state machine/motion/transition | `neon-ui-runtime::nui_state_machine`（Neon3） |
| UI 布局/绘制/文本/图像/渲染 | `neon-wgpu-runtime`（Neon3） |
| hit-test/pointer 捕获 | `neon-wgpu-runtime`（Neon3） |
| 动画采样/插值 | `neon-wgpu-runtime::ui_renderer`（Neon3） |
| D3D12 shared texture 导出 | `neon-wgpu-runtime`（Neon3） |
| **Bevy 侧** | 导入纹理、提交 camera/anchor/变量、发点击、接收变量回写 |

**结论：bevy-nui-host 是"真全功能 NUI"的显示/游戏侧宿主，不是 NUI 的实现者。** 它能不能"支持"某功能，取决于：
1. Neon3 侧是否实现了该 UI 能力（已实现大部分）；
2. bevy-nui-host 是否把对应语义事件/变量回写闭环接好（当前只有极简面板）。

---

## 五、组件 gallery NUI 的 Bevy 支持度逐项对照

以 `imgui-component-gallery.nui`（105 行全功能展示）为基准：

### 5.1 Neon3 renderer 已实现（bevy 可见性取决于场景是否使用）

| 能力 | Neon3 | bevy-nui-host 当前是否实际使用 |
|------|-------|-------------------------------|
| panel/text/button | ✅ | ✅（status 面板用） |
| input 文本输入 | ✅ | ❌ 未用 |
| checkbox/radio/selectable | ✅ | ❌ 未用 |
| slider/drag_value | ✅ | ❌ 未用 |
| progress_bar | ✅ | ✅（health/mana） |
| combo/dropdown/tabs/list_box | ✅ | ❌ 未用 |
| scrollbar/scroll view | ✅ | ❌ 未用 |
| image | ✅ | ✅（怪物贴图走 Bevy 自身，Neon UI image 未用） |
| render surface | ✅ | ✅（world-ui-lab.preview） |
| data_grid | ✅ | ❌ 未用 |
| dialog | ✅ | ❌ 未用 |
| tooltip | ✅ | ❌ 未用 |
| template/repeat | ✅ | ❌ 未用（计划中：`NeonWorldUi<V>` 批量模板是未来目标） |
| drag/drop | ✅ | ❌ 未用 |
| world panel | ✅ | ⚠️ plan 已设计 `NeonWorldUi<V>` + anchor 流程，当前单实例可用 |

### 5.2 bevy-nui-host 实际闭环的路径

- `ui.flow.submit`：✅（main.rs 动态生成 combined-ui source）
- `ui.input.frame`：✅（health/mana/level）
- `ui.host.inbound`（语义事件）：✅（character.status.toggle 已进入 Bevy）
- `wgpu.world.camera.submit_frame` / `wgpu.world.ui.anchor.submit_batch`：✅（已实现，Phase 2 需求是把它改成 latest-value coalescing）
- 共享 surface 导入/合成：✅（三缓冲）
- 变量回写 `apply_changes`：⚠️ 计划中（`neon3-world-ui-unified.md` 阶段 3 才做）

### 5.3 结论

```
组件 gallery 的"渲染能力"：Neon3 renderer 全部支持（能画出来）
组件 gallery 的"交互闭环"：只有极小一部分被 bevy host 实际接起来
"bevy 是否全部支持"：从"能显示"角度 = 大部分可以；
                      从"完整交互 + 变量回写 + 数据源接入"角度 = 远未全部支持
```

所以准确的答案是：**bevy-nui-host 不是"全功能 NUI"的完整实现，它只接入了一个最小 slice；NUI 的全功能由 Neon3 侧支撑，bevy 侧要"全部支持"还需要按组件逐个打通语义事件 + 权威回写 + 数据源绑定，工作量集中在 bevy host 而非 Neon3。**

---

## 六、施工优先级建议（横向扩展方向）

### P0 必须先修（否则扩展会 panic / 锁死）

1. `MAX_WORLD_UI_QUADS = 256` 硬上限直接 assert -> 改为分批/动态容量 + 稳定超限错误码。
2. GPU/runtime mutex 无限 `yield_now` 重试 -> 有界等待 + `renderer_busy`。
3. `RpcServer::serve_until` 同步 handler 串行 -> 长任务 job 化（详见 `docs/ipc-replacement-evaluation-2026-08-22.md`）。
4. hover 每次移动触发完整 hit pass/readback -> CPU broad-phase + latest-value coalescing。

### P1 样式/动画横向扩展

| 优先级 | 能力 | 层次 |
|--------|------|------|
| 1 | 节点上直接声明 `border_width/corner_radius/opacity` | 语法层（低难度） |
| 1 | 独立方向 padding / margin / align_self | 语法层（低难度） |
| 1 | 文字颜色/字号/字重 | schema + renderer（中难度） |
| 2 | transform/scale（静态）→ transform 动画 | schema + renderer（高难度） |
| 2 | cubic-bezier / spring easing | runtime + renderer（中难度） |
| 2 | theme token 真正解析注入 | 语法 + runtime（中难度） |
| 3 | 入场/退场动画（fade/slide） | 语法 + runtime（中难度） |
| 3 | keyframe / stagger / repeat | runtime + renderer（中难度） |
| 3 | 渐变 / 阴影 / 模糊 | renderer shader（高难度） |
| 3 | HSL / sRGB-correct 颜色插值 | renderer（低难度） |

### P2 数据面横向扩展

- `ui.input.repeat` 批量模板 rows（1000 实例一个 frame）——底座已备，施工即可。
- world UI instance 化 shader（per-instance 变量驱动 glyph/bar），替代 atlas 过渡。
- 双 lane RPC（world_latest_lane / interaction_lane）——已在 `phase2-bevy-nui-host-work-requirement.md` 明确定义。

---

## 七、关键证据文件索引

| 证据 | 文件 |
|------|------|
| UiStyle 仅 5 字段 | `neon-ui-schema/src/lib.rs` |
| Flow 语法层仅 fill/line | `neon-ui-runtime/src/nui_flow.rs` |
| state style 才支持 border_width/corner_radius/opacity | `neon-ui-runtime/src/nui_flow.rs`（style 解析） |
| 动画插值/GPU 推进 | `neon-wgpu-runtime/src/ui_renderer.rs` |
| 多播/scope_key | `neon-ui-runtime/src/lib.rs`（PendingStateMotion） |
| bevy 架构/未来计划 | `plan/neon3-world-ui-unified.md` |
| bevy latest-value 需求 | `plan/phase2-bevy-nui-host-work-requirement.md` |
| bevy 实际 flow 源 | `docs/nui-flow-reference-and-bevy-status-debug.md` |
| bevy 调试结论 | `.workbuddy/memory/2026-08-18.md` ~ `2026-08-21.md` |
| 组件 gallery 全功能 NUI | `crates/neon-ui-runtime/tests/fixtures/ui/imgui-component-gallery.nui` |

> 注：本文档基于 Neon3 工作区内可读证据 + `D:\bevy-nui-host` 目录结构枚举完成；由于环境权限限制未直接读取 bevy-nui-host 源文件内容，bevy 侧实现结论来自工作区 plan/docs/workbuddy 的既有调研记录，如需百分百确认 bevy 侧代码细节，建议后续授予该目录读取权限后再补一轮源码级核对。