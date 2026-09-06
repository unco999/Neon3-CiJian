# Neon3 音乐播放器设计稿

## 目标

做一个真正可操作的桌面音乐播放器案例，重点展示：

- UI 贴图与图片资源
- 多面板布局
- 列表、滚动、搜索、筛选、tabs、slider、输入框
- 播放队列、收藏、音量、进度、播放状态
- 领域状态与 NUI Flow 分离
- Windows 窗口中的稳定视觉和真实交互

第一版名称暂定为 **Pulse**。

## 视觉方向

关键词：黑曜石底、低多边形切面、金色强光、灰白几何建筑、紫色星尘、暖白文字。

- 背景：`#080909`
- 主面板：`#151515`
- 次面板：`#242424`
- 主色：`#D99A42`
- 播放状态：`#D8C3FF`
- 警示/进度：`#F4D28A`
- 主文字：`#F4F0E8`
- 次文字：`#96908A`

不依赖大面积渐变。视觉层次主要靠低多边形贴图、黑色留白、金色边缘光、细边框、专辑封面、进度色和局部高亮完成。

Downloads 中的图片形成了统一母题：黑色几何空间、金色光柱、低多边形人物、紫色星空和石材金属纹理。它们适合做首页主视觉、专辑封面和当前播放背景。带生成平台水印的图片只作为风格参考，正式使用需要无水印版本或明确授权。

## 第一屏布局

窗口建议 `1280x760`，最小内容区 `1040x640`。主视觉图片放在内容区，不用高对比满屏背景压住文字。

```text
┌─────────────────────────────────────────────────────────────┐
│ Pulse     搜索音乐                         设置 账户        │
├──────────────┬──────────────────────────────┬─────────────┤
│ 侧栏          │ 主内容                        │ 当前播放    │
│ 首页          │ 几何主视觉 / 最近播放           │ 封面         │
│ 我的音乐      │ 专辑卡片 / 歌曲列表             │ 歌曲名       │
│ 播放列表      │                              │ 艺术家       │
│ 收藏          │                              │ 进度条       │
│              │                              │ 播放控制     │
├──────────────┴──────────────────────────────┴─────────────┤
│ 小型播放栏：封面 歌曲名 进度 播放/暂停 上一首 下一首 音量   │
└─────────────────────────────────────────────────────────────┘
```

### 侧栏

- `tabs` 或按钮组：Home、Library、Playlists、Favorites
- 当前项有金色左边线和浅色背景
- 播放列表使用可滚动列表
- 未读/数量使用小型数字文本，不做装饰性 badge 堆叠

### 主内容

- 顶部搜索输入框
- `tabs`：最近播放、推荐、专辑、艺术家
- 专辑封面网格：第一版使用固定 4 列，避免动态布局不稳定
- 歌曲表格：序号、歌曲、艺术家、时长、收藏、更多操作
- 列表过长时使用 `scroll` 或 `data_grid`

### 当前播放面板

- 大专辑封面，使用低多边形人物或建筑图
- 歌曲名、艺术家、专辑名
- `progress_bar` 显示当前时间/总时长
- 上一首、播放/暂停、下一首、循环、随机
- 音量 `slider`
- 可选波形区域：使用 `canvas` 的 typed points/lines 数据

### 底部播放栏

底部栏始终固定高度，不随歌曲名长度改变布局。歌曲名列设置最大宽度，过长文字由 domain 提供短标题或截断显示。

## 领域状态

```text
player snapshot
  active_view: home | library | playlists | favorites
  active_tab: recent | recommended | albums | artists
  search_query: text handle
  current_track: stable track id
  is_playing: bool
  position_ms: i32
  duration_ms: i32
  volume: i32:0..100
  shuffle: bool
  repeat: off | one | all
  liked: bool
  queue: grid
  albums: grid
  tracks: grid
  waveform: canvas_data
```

领域服务负责播放队列、搜索结果、收藏、时间推进和播放规则。NUI 只读取 typed inputs，按钮只发送 semantic intent。

## 主要 intent

```text
player.view.select
player.tab.select
player.search.commit
player.track.play
player.track.queue
player.track.like
player.transport.play_pause
player.transport.previous
player.transport.next
player.transport.seek
player.volume.commit
player.shuffle.toggle
player.repeat.select
```

不要把歌曲 ID、按钮编号或鼠标坐标放进 UI 私有逻辑。歌曲操作通过稳定 track key 或 typed grid row 传递。

## 现有能力可直接复用

| 需求 | 当前能力 | 做法 |
| --- | --- | --- |
| 面板和布局 | 已有 | `panel`、`row`、`column`、`gap`、`pad` |
| 歌曲列表 | 已有 | 优先 `data_grid`，小列表可用 `panel` + `repeat` |
| 滚动 | 已有 | `scroll` + 固定高度 |
| 搜索框 | 已有 | `input` + `text` input + `text_edit_commit` |
| 播放进度 | 已有 | `progress_bar numeric $position` |
| 音量 | 已有 | `slider numeric $volume` |
| 频道/页面切换 | 已有 | `tabs state $active_tab` 或稳定按钮 intent |
| 专辑封面 | 已有 | `image` + `ui.image.upload` |
| 波形图 | 部分已有 | `canvas data $waveform`，由 domain 发送 typed canvas 数据 |
| 播放状态分支 | 已有 | bool/enum input + `branch` |

## 需要补齐的底层能力

### P0：必须先做

1. **音频服务协议**
   - `audio.track.open`
   - `audio.play`
   - `audio.pause`
   - `audio.seek`
   - `audio.stop`
   - `audio.snapshot`
   - 带 `track_id`、`position_ms`、`duration_ms`、`is_playing`、epoch 和 revision

2. **音频播放实现**
   - Windows 首选 `cpal` 或项目认可的音频后端
   - 播放器进程拥有音频设备和解码器
   - UI/WGPU 不直接读音频文件，不拥有音频设备

3. **时间同步**
   - 播放进度不能依赖 UI 定时器猜测
   - audio service 发布单调时间/position snapshot
   - UI 只显示最新 snapshot

### P1：为了达到视觉质量

1. **图片呈现增强**：圆形头像、封面裁剪、contain/cover、图片圆角裁剪。
2. **图标系统**：稳定的内置 icon glyph 或图片图标资源，避免用文字 `+`、`-` 代替所有控制。
3. **视觉样式**：阴影、透明度层级、焦点/悬停/按下状态、统一 control theme。
4. **可复用列表行模板**：固定列宽、选中态、禁用态、hover 态。
5. **文本滚动/省略**：歌曲名和艺术家名不能撑大父布局。
6. **真实 target PNG capture**：`wgpu.render.target.capture` 目前只返回 target metadata；需要输出实际 PNG 或可读回的 frame artifact，才能做复杂视觉案例的自动验收。
7. **Window chrome**：第一层支持 `NEON_WINDOW_CHROME=borderless` 和非交互 root drag；下一层需要 Flow-local `window_button`（minimize/maximize/close）和显式 drag region，避免 domain 处理 OS 窗口操作。

### P2：增强体验

1. waveform 真实频谱或预计算波形数据
2. 播放队列拖拽排序
3. 进度条拖动 preview/commit
4. 全局快捷键和媒体键
5. 本地音频文件导入、元数据读取、封面提取

## 资源计划

### 当前图片的安排

| 图片类型 | 用途 |
| --- | --- |
| 竖幅骑士/建筑图 | 首页主视觉或当前播放详情背景 |
| 紫色星尘人物图 | Ambient、Cinematic 专辑封面 |
| 金色骑士/王冠人物图 | Epic、Battle、Orchestral 专辑封面 |
| 几何光柱建筑图 | 首页背景纹理或空状态背景 |

### 还需要的素材

- 12 至 20 张统一风格低多边形专辑封面，建议使用可商用或 CC0 图片
- 1 张默认用户头像
- 1 套 20 至 30 个 UI icon：play、pause、next、previous、shuffle、repeat、heart、volume、search、more、queue、home、library、playlist、settings
- 1 套小尺寸纹理：面板边框、选中态、播放态、分隔线
- 1 个 Pulse logo，建议做成金色几何字标
- 3 个空状态插图：无歌曲、无收藏、搜索无结果

### 推荐素材规格

| 素材 | 规格 |
| --- | --- |
| 专辑封面 | PNG/JPEG，`1024x1024`，最终显示 `160x160` 或 `256x256` |
| 小图标 | SVG 仅作为源文件，运行时转 PNG；或直接使用统一 PNG |
| UI 贴图 | 2x 倍率，透明 PNG，边框与中心区域分开考虑 |
| 空状态插图 | PNG，透明背景，宽度 `320~480` |
| 波形 | 不下载图片，使用 typed canvas 数据生成 |

目前不下载外部素材。你提供的图片足够确定第一版风格，但带水印图片只能作为参考，正式使用前需要无水印源文件或明确授权。已整理为播放器资源目录，并生成 512x512 缩略图供窗口上传，避免原始竖图造成启动超时。

## 实施顺序

1. 先做静态高质量 UI：黑曜石面板、低多边形主视觉、专辑封面、列表和播放栏。
2. 补 icon、图片 cover 裁剪、几何边框和 hover/focus 金色高亮状态。
3. 接入 mock audio domain，完成队列、播放状态、进度和按钮链路。
4. 实现真实 audio service，再替换 mock。
5. 加入 waveform、拖拽排序、搜索和本地文件导入。
6. 每个阶段都执行 `contract-ready`、`service-ready`、`wgpu-rendered`、`interactive-accepted` 验收。

## 第一版验收标准

- 启动后第一屏无黑块、无重叠、无文字撑破布局。
- 4 张专辑封面和歌曲列表真实显示。
- 搜索输入可提交，列表会按 snapshot 更新。
- 点击歌曲后当前播放面板、底部播放栏和播放状态同步变化。
- 播放/暂停、上一首、下一首、收藏、随机、循环、音量均有独立 intent。
- 进度条显示真实 domain position；没有音频服务时明确显示 mock 状态。
- 所有动态文字通过 input 或 text handle，不把业务字符串写死在 Flow。
- 每个跨进程流程都有 JSONL probe，输出 producer、consumer、revision 和 pass/fail。
