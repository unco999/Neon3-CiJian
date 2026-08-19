# Neon3 × Bevy 资产体系对接调研报告

> 状态：调研 + 方案讨论稿，尚未实施。
>
> 范围：NuiFlow UI 脚本资源、UI 图片资源如何与 Bevy 的 assets 体系对接，以及
> projectd（工程项目模块）在其中的角色。

## 1. 现状盘点

### 1.1 Neon 资源体系（现状）

Neon 已经有一套**独立于 Bevy** 的资源体系，核心在 `neon-protocol` 与 `neon-projectd`：

- **稳定身份** `AssetRef { project_id, asset_id, revision, kind }`（`neon-protocol/src/lib.rs:150`）。
- **资产字节权威** `neon-projectd`：`assets: HashMap<(project_id, asset_id, revision, kind), AssetBytes>`，
  只存字节、不创建 GPU 对象，通过 RPC `asset.get_bytes` / `asset.list` 对外提供。
- **字节格式**：图片是**原始 RGBA8**（`media_type = "application/x-neon-rgba8"`），字体是 TTF。
  没有 png/jpg 解码链。
- **渲染侧** `neon-wgpu-runtime`：`preload_image` 把 RGBA8 字节塞进 `resident_images`，
  `rebuild_image_atlas` 把所有图片**打包进一张 atlas 纹理**（`Rgba8Unorm`），记录 uv；
  绘制时用 atlas bind group + uv 采样。

NUI flow 里的资源引用是**符号 key**，与具体 AssetRef 解耦：

```nui
resource avatar image          # 声明一个图片资源（符号 key）
...
image head resource avatar     # image 节点引用 key
```

运行时通过 `bind_nui_flow_resources(document, &bindings: HashMap<String, AssetRef>)`
把符号 key 绑定到稳定 `AssetRef`（`neon-ui-runtime/src/nui_flow.rs:525`）。

### 1.2 Bevy assets 体系（现状）

`bevy-nui-host` 现在只用 Bevy assets 加载**场景/角色/环境贴图**：

```rust
asset_server.load("scenes/tokyo/littlest_matrix_tokyo.glb")   // 场景
asset_server.load("characters/mei.glb")                        // 角色
asset_server.load("environment_maps/san_giuseppe_bridge_4k_diffuse.ktx2")  // 环境贴图
```

NUI flow 脚本是**编译期嵌入 + 代码生成**，不走 Bevy assets：

```rust
ui_sources.push(include_str!("../assets/ui/ordinary-status.nui").to_string());  // 编译期
ui_sources.push(monster_field_flow());                                           // 代码生成 string
```

然后通过 RPC `ui.flow.submit` 把 string 发给 UI Runtime。

### 1.3 两条体系的割裂点

| 维度 | Neon | Bevy | 问题 |
| --- | --- | --- | --- |
| 身份 | `AssetRef`(project_id/asset_id/revision/kind) | 路径 + `Handle<T>` | 三方无法互查 |
| 字节权威 | projectd | 磁盘文件 + AssetServer | 谁是"真源" |
| 图片格式 | 原始 RGBA8 + atlas 打包 | 解码后 `Image`(pixel data) | 格式不一致 |
| 加载方式 | RPC 手动拉取（`preload_fixture_image` 硬编码） | AssetServer 异步 + 热重载 | Neon 无热重载/自动依赖解析 |
| 生命周期 | revision 版本化 | Handle 引用计数 + 事件 | 生命周期模型不同 |

最关键的一条：**图片加载在 Neon 里目前是手动硬编码的**（`preload_fixture_image` 只拉
`fixture-project/81`），没有"flow 声明 resource → 自动拉取 → atlas"的端到端链路。这也是
"匹配 Bevy"要解决的核心。

## 2. 核心张力

1. **Neon 是唯一 GPU owner**（`AGENTS.md` 约束）：UI 的最终像素由 `neon-wgpu-runtime`
   渲染，图片必须进 Neon 的 atlas。Bevy 的 `Handle<Image>` 无法跨进程直接被 Neon 的
   wgpu 使用（atlas 打包逻辑在 Neon）。
2. **projectd 是资产字节权威**：图片字节应该归 projectd，而不是散落在 Bevy 的磁盘目录。
3. **Bevy 用户的心智模型**：用户希望用 `asset_server.load("ui/avatar.png")` 这种标准写法，
   而不是去学 Neon 的 RPC/AssetRef。

结论：**不能让 UI 图片完全绕开 Bevy assets，也不能让渲染绕过 Neon。** 正确做法是分层——
Bevy assets 管"声明 + 加载 + 热重载"，projectd 管"字节权威 + 版本"，Neon 管"渲染 + atlas"。

## 3. 方案设计

### 3.1 总体原则

- **身份统一**：`AssetRef` 是唯一稳定身份，Bevy 路径和 flow 符号 key 都是它的别名。
- **projectd 是字节权威**：所有 UI 资产字节最终落 projectd；Bevy 磁盘文件是"入口"。
- **Neon 是渲染权威**：图片最终进 Neon atlas，Bevy 只负责"把字节送到 Neon"。
- **Bevy 是声明入口**：宿主用 `asset_server.load` 声明需要哪些资源，桥接层自动同步。

### 3.2 资源身份统一（一张注册表）

引入一个轻量 `AssetRegistry`（可放在 Bevy 侧，也可放在 projectd），维护三向映射：

```text
Bevy 路径 "ui/avatar.png"  <->  AssetRef { project_id, asset_id, revision, kind="image" }
flow 符号 key "avatar"     <->  AssetRef（bind_nui_flow_resources 已做）
```

映射规则建议：`asset_id` 用路径的稳定 hash，`revision` 用字节内容的 hash（或递增），
`project_id` 用宿主项目名。这样同一张图在任何一侧都能定位到同一个 `AssetRef`。

### 3.3 NUI flow 脚本方案（推荐：Bevy 化）

flow 是**宿主声明的 UI 程序**，不是项目数据资产，建议走 Bevy assets：

- 定义 `NuiFlowAsset(String)` 实现 `Asset` trait + `NuiFlowAssetLoader`（读 `.nui` 文本）。
- 宿主用 `asset_server.load("ui/character-status.nui")` 拿到 `Handle<NuiFlowAsset>`。
- 一个 Neon 桥接插件监听 `AssetEvent::LoadedWithDependencies`，自动 `ui.flow.submit`
  给 UI Runtime，**替代现在 `include_str!` + 手动 submit_flow_source 的写法**。
- 收益：热重载（改 .nui 文件即生效）、消除编译期嵌入、Bevy 统一的加载状态查询。

### 3.4 UI 图片方案（推荐：Bevy 加载 + 桥接同步 + Neon 渲染）

图片**不走 Bevy 的渲染路径，但走 Bevy 的加载/声明路径**：

```text
用户 asset_server.load("ui/avatar.png") -> Handle<Image>（Bevy 标准写法）
        │  AssetEvent::LoadedWithDependencies
        ▼
Neon Asset Bridge（桥接插件）
        │  读 Bevy Image 的 pixel data -> 转 RGBA8 字节
        ▼
asset.put_bytes -> projectd（字节权威，得到/更新 AssetRef + revision）
        │  或直接推给 neon-wgpu-runtime
        ▼
neon-wgpu-runtime preload_image -> atlas（渲染）
```

- 桥接层职责：**Bevy `Image` → RGBA8 字节 → projectd/Neon**。Bevy 的 `Image` 是解码后
  的 pixel data，桥接层转成 Neon 的 RGBA8（或保留原始编码让 Neon 解码，见决策点 3）。
- 图片热重载：`AssetEvent::Modified` → 桥接层重新 put_bytes（revision 递增）→ Neon 重建
  atlas。天然契合 Neon 的 revision 机制。
- flow 的 `resource <key> image` 通过 3.2 的注册表自动绑定到对应 `AssetRef`，不再需要
  手工构造 `bindings` 快照。

### 3.5 projectd 的角色扩展

projectd 保持"字节权威"，需要补两个能力：

1. **`asset.put_bytes`**：让桥接层能把本地图片字节推给 projectd（现在只有 `get_bytes` 只读）。
2. **`asset.list` 已经存在**，用于桥接层启动时同步"已有哪些 AssetRef"，避免重复上传。

`revision` 策略：字节内容 hash 作为 revision（内容寻址），天然去重 + 幂等。

### 3.6 桥接层落点（Neon Asset Bridge）

建议做成一个 Bevy 插件 `NeonAssetBridgePlugin`，放在 `bevy-nui-host` 或独立 crate：

- 监听 `AssetEvent<Image>` / `AssetEvent<NuiFlowAsset>`。
- 维护 3.2 的 `AssetRegistry`。
- 通过 RPC 与 projectd / neon-wgpu-runtime 通信（复用现有 `neon-ipc`）。
- 处理异步：图片加载完成 → 上传 → 收到 AssetRef → 绑定 flow resource → 通知 Neon 预加载。

## 4. 关键决策点（需拍板）

1. **图片字节格式**：桥接层把 Bevy `Image` 转成 RGBA8（Neon 现有格式），还是让 Neon 支持
   png/jpg 解码？→ 建议先转 RGBA8（复用现有 atlas 链路，改动最小）。
2. **asset_id/revision 生成规则**：路径 hash / 内容 hash？→ 建议内容 hash 做 revision（幂等去重）。
3. **桥接层放哪**：Bevy 侧插件（推荐）还是 projectd 主动拉取？→ 推荐 Bevy 侧插件（事件驱动简单）。
4. **图片是否允许"只走 Neon 不走 Bevy"**：对纯程序化生成的 UI（如 monster flow 的纯文字面板）
   不需要图片，保持现状即可；只有声明了 image 的 flow 才走桥接。

## 5. 推荐方案（一句话）

**UI 图片不绑定 Bevy 的渲染路径，但绑定 Bevy 的加载/声明路径**：宿主用标准
`asset_server.load("ui/xxx.png")` + `.nui` 资产，一个 `NeonAssetBridgePlugin` 自动把图片
字节转 RGBA8 同步到 projectd（字节权威）并触发 Neon 重建 atlas，把 flow 脚本自动提交给
UI Runtime；全程以 `AssetRef` 为唯一稳定身份，flow 符号 key 和 Bevy 路径都是它的别名。

## 6. 实施路径

1. projectd 加 `asset.put_bytes`（字节权威补写入能力）。
2. 定义 `NuiFlowAsset` + loader；`NeonAssetBridgePlugin` 监听 Image/NuiFlowAsset 事件。
3. `AssetRegistry`：路径 <-> AssetRef <-> flow key 三向映射，内容 hash 做 revision。
4. 桥接：Bevy Image → RGBA8 → put_bytes → 通知 Neon `preload_image` → 绑定 flow resource。
5. 热重载联动 + headless 验收（图片上传后 UI 正确显示、revision 去重、图片热更新）。
