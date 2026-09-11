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

### 阶段 A：补全属性绑定（1-2天）
- 补齐 Opacity / ImageAsset / ScrollOffset / CanvasData 的 NUI Flow 绑定
- 投入产出比最高，改完后所有组件都能用变量控制这些属性

### 阶段 B：滚动+分割（2-3天）
- ScrollView 容器（Panel + scroll=true + 自动 Scrollbar）
- Splitter 组件

### 阶段 C：编辑器核心组件（3-5天）
- TreeView
- ContextMenu
- NumberInput

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
