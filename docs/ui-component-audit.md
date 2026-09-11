# Neon3 UI 组件体系检查报告

日期：2026-09-12
状态：已完成探查，进入实施阶段

## 一、当前已有组件（22 种）

| 类别 | 组件 | 状态 |
|------|------|------|
| 容器 | Panel | 完整，支持 row/column/absolute 布局、gap、align、justify |
| 文本 | Label | 完整，支持富文本（rich） |
| 按钮 | Button | 完整，idle/hover/pressed/active 四态 |
| 图片 | Image | 完整，支持 fit 模式 |
| 渲染表面 | RenderSurface | 完整 |
| 画布 | Canvas | 完整，数据驱动 canvas_data |
| 输入 | TextInput | 基础完整，缺剪贴板/焦点环 |
| 选择 | Checkbox / RadioButton / Selectable | 完整 |
| 数值 | Slider / DragValue | 完整 |
| 下拉 | Combo / Dropdown | 完整 |
| 导航 | Tabs | 基础完整，缺可关闭标签 |
| 浮层 | Tooltip / Modal / Dialog | 基础完整，缺返回值机制 |
| 列表 | ListBox | 基础完整，缺虚拟化/多选范围 |
| 滚动 | Scrollbar | 完整，但无 ScrollView 容器 |
| 进度 | ProgressBar | 完整 |
| 表格 | DataGrid | 完整，已虚拟化 |

## 二、功能缺失

### 2.1 属性绑定不完整（最关键，P0）

Schema 定义了 12 种可绑定属性，NUI Flow 只支持 7 种：

| 属性 | Schema | NUI Flow | 说明 |
|------|--------|----------|------|
| TextValue | ✅ | ✅ `value $var` | |
| Visible | ✅ | ✅ `visible $var` | |
| Enabled | ✅ | ✅ `enabled $var` | |
| Active | ✅ | ✅ `checked $var` | |
| Selected | ✅ | ✅ `selected $var` | |
| StateToken | ✅ | ✅ `state $var` | |
| NumericValue | ✅ | ✅ `numeric $var` | |
| **Opacity** | ✅ | ❌ | 序列化时 `_ => continue` 跳过 |
| **ImageAsset** | ✅ | ❌ | 同上 |
| **ScrollOffset** | ✅ | ❌ | 同上 |
| **CanvasData** | ✅ | ❌ | 同上 |

影响：无法用变量控制透明度、图片资源、滚动位置。grid-pulse 案例中格子颜色只能硬编码。

### 2.2 缺少滚动容器

有 Scrollbar 组件，但没有 ScrollView/ScrollArea——内容超出自动裁剪+滚动的容器。

### 2.3 交互缺失

- 拖拽排序（DragAndDrop）
- 键盘导航（Tab 焦点遍历、列表上下键）
- 剪贴板（TextInput Ctrl+C/V/X）
- 焦点环
- 右键菜单（ContextMenu）

### 2.4 动画缺失

- 只有 enter_transition 进入动画
- 无通用属性动画（颜色、位置、大小、透明度）
- 无状态过渡动画（hover/pressed 硬切）

### 2.5 其他半成品

- Modal/Dialog 无统一返回值机制
- Tabs 无可关闭标签按钮
- ListBox 无多选范围（Shift+点击）
- Tooltip 无显示延迟控制

## 三、未来需要增加的组件

### P0（编辑器刚需）
1. ScrollView — 滚动容器
2. Splitter — 可拖拽分割器
3. TreeView — 文件浏览器/层级结构
4. ContextMenu — 右键菜单

### P1（常用控件）
5. NumberInput — 带上下箭头的数字输入框
6. ColorPicker — 颜色选择器
7. Toast/Notification — 操作反馈通知
8. Accordion — 折叠面板
9. MenuBar — 顶部菜单栏

### P2（增强体验）
10. Switch — 开关
11. Badge — 徽标/角标
12. Avatar — 头像
13. Breadcrumb — 面包屑导航
14. Pagination — 分页
15. Popover — 轻量弹出层
16. Toolbar — 工具栏容器

### P3（特定场景）
17. DatePicker/TimePicker
18. Rating（评分）
19. Slider 双滑块（范围选择）
20. 虚拟列表（ListBox 虚拟化）

## 四、推进路线

### 阶段 A：补全属性绑定（已完成，commit 680c684）
- [x] Opacity 绑定（`opacity $var`，F32）
- [x] ImageAsset 绑定（`resource $var`，AssetHandle）
- [x] ScrollOffset 绑定（`scroll_offset $var`，Vec2）
- [x] CanvasData 绑定（`data $var`，已有专门处理）
- [x] asset_handle input kind + 默认值 `asset:empty`
- [x] struct field 支持 asset_handle
- [x] UiIrDocument::validate() ImageAsset 绑定视为外部图片
- [x] lower_nui_flow_effects 生成 ImageBinding effect
- [x] binding_accepts 补齐类型校验
- [x] 序列化 round-trip
- [x] 5个新测试，全量测试通过（ui-schema 37, ui-runtime 148, GPU 20）

注意：`scroll` 属性保持 NumericValue（Scrollbar 单轴），新增 `scroll_offset` 用于二维 Vec2 偏移。

### 阶段 B：滚动+分割（已完成，commit 201717d + da2ca61）
- [x] ScrollView 容器基础属性
  - [x] `scrollable` 无值属性（clip=Scroll）
  - [x] `scroll_offset` 字面量（x,y 格式）
  - [x] `scroll_offset` 绑定（Vec2）
  - [x] 序列化 round-trip
  - [x] 渲染层已有完整支持（滚轮+拖拽+metrics，3个测试通过）
- [x] Splitter 组件基础结构
  - [x] UiNodeKind::Splitter
  - [x] NUI Flow 解析 + 序列化
  - [ ] 拖拽调整相邻面板大小（待实现）

注意：渲染层已有完整的 ScrollView 交互支持（scroll_wheel_at_pointer + scroll_drag），NUI Flow 层只需暴露属性。

### 阶段 C：编辑器核心组件（进行中，commit 9fa3e13）
- [x] ContextMenu 基础结构（UiNodeKind + 解析 + 序列化）
- [x] ContextMenu 默认样式（半透明深色背景 + 圆角 + 边框）
- [x] TreeView 基础结构（UiNodeKind + 解析 + 序列化）
- [x] TreeView 默认样式（深色背景 + 边框）
- [x] Splitter 基础结构 + 默认样式（深灰分割条 + hover高亮）
- [ ] NumberInput（DragValue 已有数字输入基础，待加上下箭头）
- [ ] ContextMenu 右键触发 + 点击外部关闭
- [ ] TreeView 展开/折叠 + 层级缩进 + 选中
- [ ] Splitter 拖拽调整相邻面板大小

### 阶段 D：动画系统（3-5天）
- 通用属性动画框架
- 状态过渡动画

## 五、实施记录

### 阶段 A：属性绑定补全
- [ ] Opacity 绑定（`opacity $var`）
- [ ] ImageAsset 绑定（`image $var`）
- [ ] ScrollOffset 绑定（`scroll $var`，已存在 scroll 属性但可能不支持绑定）
- [ ] CanvasData 绑定（`data $var`）
- [ ] 绑定解析测试
- [ ] 运行时 apply_binding 测试
- [ ] 实际渲染验证（probe）
