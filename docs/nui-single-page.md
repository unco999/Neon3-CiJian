# NUI Flow 单页教程

> **给 AI / 开发者的一句话**：NUI Flow 是 Neon3 的**声明式 UI 语言**。它只描述
> "画什么、怎么排列、数据从哪来、用户操作发什么消息"——不执行代码、不读文件、
> 不碰 GPU。所有插图均为真实渲染结果（560x300，headless GPU server 输出）。

---

## 1. 这是什么

Neon3 是"UI 声明与渲染分离"的系统：

```text
领域服务（你的业务代码/服务）
    │  ① 发送 typed input 帧（值/文本/网格数据）
    ▼
neon-ui-runtime（解析 NUI Flow → 编译 UI 程序）
    │  ② 提交 fragment
    ▼
neon-wgpu-runtime（唯一 GPU owner：排版、命中、像素）
    │  ③ 用户操作 → semantic intent（语义意图）
    ▼
领域服务（收到意图，更新状态 → 回到 ①）
```

**NUI Flow 只负责 ① 与 ③ 之间的声明部分**：你写好 `.nui` 文本交给 runtime，
runtime 负责"编译 + 渲染 + 把用户操作翻译成语义意图"。

---

## 2. 第一份 Flow：文档长什么样

一个最小文档由 **version + surface + 节点** 组成，缩进 = 2 空格：

```text
version 1
surface minimal column w 360 h 200 gap 8 pad 12 align stretch fill #17201E
  text title h 24 value "Hello NUI Flow"
  text subtitle h 18 value "One surface. One panel. One text."
```

![01-minimal](media/nui-guide/01-minimal.png)

- `version 1`：文档语法版本，必须第一行
- `surface <id> <布局> w <宽> h <高> ...`：声明一个表面（画面根）
- `text <id> value "..."`：一个文本节点；`h 24` 是行高
- `column` / `row`：主轴方向；`gap` 子项间距；`pad` 内边距；`fill` 背景色

---

## 3. 布局规律：column / row / gap / pad / align

```text
version 1
surface layout column w 480 h 220 gap 10 pad 12 align stretch fill #1B2530
  panel left row w 200 h 40 gap 6 pad 6 fill #2E4255
    text a value "A"
    text b value "B"
    text c value "C"
  panel right row w 200 h 40 gap 6 pad 6 fill #3B5066
    text x value "X"
    text y value "Y"
  text note value "row lays children left-to-right; gap separates them"
```

![02-layout](media/nui-guide/02-layout.png)

**规律**：
- `column`：子项自上而下（主轴=垂直）；`row`：子项从左到右（主轴=水平）
- `gap`：兄弟节点间距；`pad`：节点内边距；`align`：交叉轴对齐（stretch=撑满）
- 颜色用 `#RRGGBB`；`panel` 是通用容器，`text` 是纯文本
- **节点 id 是稳定语义名**（`left`、`right`），不是数组下标——改名等于破坏协议

---

## 4. 数据规律：input 槽是唯一的数据入口

```text
version 1
input health f32:0..100 default 82
input name text default text:empty
surface inputs column w 400 h 200 gap 8 pad 12 align stretch fill #17201E
  text health-label value "Health:"
  progress_bar health_bar numeric $health
  text name-label value "Name:"
  text name-value value $name
```

![03-inputs](media/nui-guide/03-inputs.png)

**规律**：
- `input <名> <类型> default <默认值>` **必须在 surface 之前、顶层声明**
- 类型：`f32:0..100`（有界浮点）、`i32`、`u32`、`text`（文本句柄）、`grid`（网格）
- 域数据用 `$槽名` 绑定：`numeric $health`、`value $name`
- **input 槽是领域服务与 UI 之间唯一的握手**——UI 从不直接读业务状态

---

## 5. 交互规律：button / slider → semantic intent

```text
version 1
input volume f32:0..100 default 50
surface events column w 420 h 230 gap 8 pad 12 align stretch fill #17201E
  text title h 24 value "Controls"
  button primary h 36 value "Save" event app.save
  button danger h 36 value "Delete" event app.delete
  slider volume numeric $volume
  text hint value "button -> semantic intent; slider drag -> value_commit"
```

![04-events](media/nui-guide/04-events.png)

**规律**：
- `event <domain>.<verb>.<subject>` 声明意图；按钮点击 → `activate` 意图
- 滑杆拖拽 → `value_commit` 意图（携带数值）
- **意图是应用领域的词，不是 UI 的词**：写 `terrain.tool.select`，
  不写 `button3.clicked`——UI 细节不泄漏出 runtime

---

## 6. 溢出规律：scroll 容器

```text
version 1
surface scroll column w 400 h 240 gap 8 pad 12 align stretch fill #17201E
  text title h 24 value "Inspector"
  scroll inspector column h 160 gap 4 pad 8 fill #22302D
    text p1 value "Material: Oak"
    text p2 value "Roughness: 0.42"
    text p3 value "Metallic: 0.00"
    text p4 value "Opacity: 1.00"
    text p5 value "Scale: 2.00 m"
    text p6 value "Revision: 14"
    text p7 value "Author: Studio"
    text p8 value "Note: overflow scrolls"
```

![05-scroll](media/nui-guide/05-scroll.png)

**规律**：内容超出容器高度时，`scroll` 提供滚动裁剪；子项仍按 column 排列。

---

## 7. 大数据规律：data_grid + 绑定列

```text
version 1
input assets grid default grid:empty
surface datagrid column w 520 h 260 gap 8 pad 12 align stretch fill #17201E
  text title h 24 value "Assets"
  data_grid assets-grid h 180 source $assets capacity 4 row_height 28 overscan 1 columns "id:80:text,name:180:edit:64:asset.name.commit,status:120:dropdown:draft|ready:asset.status.set"
  text hint value "grid data arrives as a typed input frame from the domain service"
```

![06-datagrid](media/nui-guide/06-datagrid.png)

**规律**：
- 大列表用 `data_grid` + `source $网格槽`，不要手写几百个 text
- `columns "id:80:text,..."`：列宽:类型（可编辑列带事件）
- `capacity` 虚拟化窗口；`overscan` 预取行数——**无论 100 行还是 1 万行，声明完全一样**

---

## 8. 组合：一个真实工作台

```text
version 1
input health f32:0..100 default 64
input name text default text:empty
surface workbench column w 560 h 300 gap 8 pad 12 align stretch fill #1B2530
  text title h 24 value "Terrain Workbench"
  panel summary row w 536 h 56 gap 8 pad 8 fill #22384C
    text terrain-name value $name
    progress_bar hp numeric $health
  panel tools row w 536 h 44 gap 6 pad 6 fill #2E4255
    button t1 h 32 value "Sculpt" event terrain.tool.select
    button t2 h 32 value "Water" event terrain.tool.select
    button t3 h 32 value "Material" event terrain.tool.select
  text status value "mode sculpt, brush round"
```

![07-workbench](media/nui-guide/07-workbench.png)

---

## 9. 铁律（违反即被拒绝）

| 禁止 | 原因 |
| --- | --- |
| 在 Flow 里写代码/表达式/`=` 赋值 | 声明式语言，非可执行 |
| 在 Flow 里放 URL/文件路径/JSON | runtime 不读外部资源 |
| 节点 id 用数字/下标 | id 必须是稳定语义名 |
| 用 `button3.clicked` 这类 UI 词做意图 | intent 必须是领域词 |
| UI 直接读业务状态 | 只能经 input 槽 |
| 在 Flow 里放 GPU handle / buffer 名 | 那是 wgpu-runtime 的私事 |

> 例子：`text status value "mode=sculpt brush=round"` 里的 `=` 会被解析器拒绝
> （`ui_program_forbidden_flow_feature`），改成 `mode sculpt, brush round`。

---

## 10. 我能做什么 / 不能做什么（边界速记）

| 能 | 不能 |
| --- | --- |
| 声明层级、布局、间距、颜色、字号 | 执行代码、读文件、访问网络 |
| 声明 typed input 槽（数值/文本/网格） | 创建 GPU 资源、改项目数据 |
| 声明按钮/滑杆/输入框/网格/滚动/工具提示 | 在 UI 里计算业务规则 |
| 声明语义意图（domain.verb.subject） | 双向绑定业务状态 |
| 声明数据绑定（`$slot`） | 持有渲染句柄 |

**一句话总结**：NUI Flow = "把 UI 声明交给 runtime，把业务留在服务端，
把交互翻译成语义意图"——input 槽进、semantic intent 出、稳定 id 贯穿。

---

## 复现这些插图

```powershell
# 需要：neon-wgpu-runtime（headless GPU server）
cargo run -p neon-ui-runtime --bin nui_guide_render -- 127.0.0.1:43147 docs/media/nui-guide
```
