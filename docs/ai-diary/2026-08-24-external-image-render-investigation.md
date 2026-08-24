---
date: 2026-08-24
topic: Bevy external image render investigation
type: research
---

## 涉及的 crate / 文件路径

- `D:/bevy-nui-host/src/main.rs`
- `D:/bevy-nui-host/src/lib.rs`
- `D:/bevy-nui-host/assets/ui/status-flow.nui`
- `crates/neon-ui-runtime/src/bin/image_resource_probe.rs`
- `crates/neon-wgpu-runtime/src/lib.rs`
- `crates/neon-wgpu-runtime/src/ui_renderer.rs`

## 发现的问题

- 用户截图中 `city-sign-image` 的预期区域为空，但 Flow 声明、素材裁剪区、RGBA8 上传、atlas residency 和 `UiImageInstance` 诊断均存在。
- 先前的 `bevy_image_flow_probe` 只验证 source/upload/binding/resident/sample metadata，不能证明外部共享 target 或 Bevy 最终 composite 的对应像素可见。
- 运行截图对应的 `D:/bevy-nui-host/target/debug/neon3-bevy-nui-host.exe` 构建时间为 09:20:38；headless external image atlas preload 的当前源码修改时间为 10:11 之后。因此旧二进制不包含后续的 headless atlas preload 实现。

## 采取的方案

- 沿 Bevy `Image` -> `UiImageSource` -> `ui.image.upload` -> `wgpu.ui.image.upload` -> headless screen/world atlas -> `IMAGE_SHADER` -> shared D3D12 target -> Bevy composite 追踪调用链。
- 验证 `Bistro_Sign_Main_BaseColor` 的 `[0, 170, 970, 520]` 裁剪区全部 504400 个像素 alpha 非零，487863 个像素非黑，排除空/透明素材。
- 重建 `neon3-bevy-nui-host` 主程序，使其链接当前 `neon-wgpu-runtime` 的 headless external-image preload 实现。
- 修复 headless static-frame 缓存：新 external image 成功预载到 atlas 后，强制下一帧重新绘制，即使 Flow fragment 没有 revision 变化。
- 将 Bevy source 日志改为只在实际排队上传时输出 `bevy_external_image_upload_queued` 和 request ID；此前每帧都会打印 source metadata，掩盖了真正的上传状态。
- 将 image 上传系统改为 asset-event 驱动：稳定资源不再在每帧执行 2 MB CPU copy、FNV hash 和 504400 像素 alpha scan；`Added`、`Modified` 或 `LoadedWithDependencies` 事件才重新打开上传路径。
- 增加 `debug.external.color.sample`，从 producer-owned shared screen target 采样到 renderer-owned diagnostic mirror，再 readback。shared D3D12 texture 本身维持既有 usage，避免 wgpu raw-wrapper 的 `COPY_SRC` state assertion。
- Bevy consumer 在选择新的共享 color ring slot 时输出 `bevy_external_ui_consumer_selected`，包含 `buffer_index`、`frame_sequence` 与前一帧配对，供定位 producer 已更新但 consumer 保留旧 slot 的情况。
- 确认 Bevy consumer 连续选择最新 frame（3 至 27），排除它停留在 image upload 前的 ring slot。原 `Bistro_Sign` source region 是大面积暗 atlas；将实际 host 裁剪收紧为 `[28,269,664,312]`，以在 `180x96` image node 内保留招牌内容并将上传量从约 2.0 MB 降至约 0.83 MB。
- 定位最终遮挡根因：`city-console` 为 `Modal`，其 panel rectangles 走 popup pass；此前 image 仍在普通 image batch 先画，随后 popup pass 的 `city-overview` 不透明 rect 覆盖 image。将 modal/dialog/tooltip 子树 image 分离为 `popup_images`，在 popup rects 之后、popup text 之前绘制。
- 性能复核：首次 `970x520` image upload 可出现一次约 2.7s atlas 建立峰值；稳定帧的 atlas 不会 rebuild/write。world camera/anchor 变化此前会同时执行 `screen_ui.invalidate_plan()`，清空固定 ScreenUi（含 text/image）的 layout cache。移除此错误失效，保留 world plan invalidation、screen fragment revision、viewport resize 和 image residency 的正确刷新路径。
- 继续定位持续掉帧：world transform 的 `invalidate_plan()` 也会清空 world text cache，且 cache key 包含随投影变化的 logical `x/y`。新增 `invalidate_plan_for_world_transform()`，只失效 plan revision；world text cache key 改为 text、atlas generation、逻辑尺寸、clip 尺寸、world scale 和 scroll，不含投影位置。缓存命中后仍以当前 visual origin/clip 平移 glyph instances。
- 明确后续验收缺口：增加外部共享 target 或 Bevy 最终 composite target 的 frame capture/readback 像素断言，不能再以 sampled metadata 代替最终像素验收。

## 当前状态

进行中。主程序已经重建且包含 atlas-preload frame invalidation；尚未启动交互窗口验证用户视觉结果，也尚未新增最终 target 的外部 image 像素 probe。

## 测试与验证结果

- `cargo run -p neon-ui-runtime --bin image_resource_probe`：通过。固定 2x2 RGBA input 产生 atlas region `(1,1,2,2)`，Flow 节点显示 `resident: true` 和匹配 UV。
- `cargo run --bin bevy_image_flow_probe`：通过。升级为真实尺寸 `970x520`、`2017600` 字节 deterministic RGBA8 输入，先提交 Flow 再上传 image；upload accepted 后 frame sequence 为 2，screen atlas 已 preloaded 且 image node resident。
- `cargo test -p neon-wgpu-runtime two_resident_images_render_from_one_atlas_with_distinct_uvs -- --nocapture`：通过，离屏像素断言验证红/绿 atlas 图像。
- `cargo test -p neon-wgpu-runtime external_image_binding_uploads_and_renders_without_asset_ref -- --nocapture`：通过，验证 `UiImageSource` + `ImageBinding` 直接输出非透明外部 image 像素。
- `cargo build --bin neon3-bevy-nui-host`：通过，存在既有 unused/dead-code warnings；最终重建后的主程序时间为 11:23:58。
- `cargo check --bin bevy_image_flow_probe`：通过，存在相同的既有 warnings。
- `cargo run --bin bevy_image_flow_probe`：通过。Probe 使用实际 `city-console` 嵌套布局和 `2560x1440` 2x target；image bounds 为 `(48,110,180,96)`，物理 image 内点 `(120,260)` 在 producer target frame 2 / buffer 1 readback 为 `RGBA [48,156,88,255]`。这证明 `IMAGE_SHADER` 已经将 image 写入 Screen UI target。
- `cargo build --bin neon3-bevy-nui-host`：通过；最终主程序包含 producer pixel sample、asset-event upload throttling 和 consumer ring-selection diagnostics。
- Modal ordering regression：`bevy_image_flow_probe` 改为 `modal city-console`，在同一 2x target / 同一 image bounds 场景通过；frame 2 / buffer 1 / physical `(120,260)` 仍读回 `RGBA [48,156,88,255]`。这条 probe 在旧顺序下会被 popup panel 覆盖。
- `cargo check -p neon-wgpu-runtime -p neon-ui-runtime`：通过，存在既有 unused/dead-code warnings。
- `cargo run --bin bevy_image_flow_probe`：在移除 screen invalidation 后通过，modal image readback 保持 `RGBA [48,156,88,255]`。
- `cargo build --bin neon3-bevy-nui-host`：通过，存在既有 unused/dead-code warnings。
- 发现并移除 image 每帧重复上传：`UiWgpuRenderer::draw()` 在生成 `images` 后先写一次 `image_buffer`，随后排序成 `ordered_images` 又写一次；modal image 还会在 popup pass 写入。现在普通 image 只由最终排序 batch 写入，popup image 只在 popup pass 写入。
- Bevy consumer screen bind group 按 buffer index 缓存，避免每个 RenderApp frame 创建 bind group；image/modal probe 和 image pixel tests 保持通过。
- Bevy consumer 性能审计：Screen UI render system 每帧创建 screen bind group，并把每帧变化的 Bevy scene color view 绑定到 Screen shader；Screen shader 的 debug mode=4 路径实际只输出 UI color，因此新增稳定 1x1 dummy scene color，并按 `buffer_index` 缓存 screen bind group，避免每帧创建 bind group。
- Bevy consumer cache 验证：`cargo check --bin neon3-bevy-nui-host` 通过；image/modal probe 仍通过，producer frame 2 / buffer 1 / `(120,260)` readback 为 `RGBA [48,156,88,255]`。
- `cargo test -p neon-wgpu-runtime world_transform_update_does_not_relayout_static_text -- --nocapture`：通过。测试执行两次真实 offscreen GPU draw，在移动 world root 且重建 plan 后断言 text layout 计数不增加。
- 再次执行 `cargo run --bin bevy_image_flow_probe`：通过，modal image pixel readback 保持 `RGBA [48,156,88,255]`。
- `git diff --check`：无 diff whitespace 错误；命令仅报告既有文件将从 LF 转为 CRLF 的 Git warnings。

## 未完成事项与下一步

- 用重建后的主程序重新打开 Bevy host，确认 `city-sign-image` 在截图的左侧 180x96 区域显示，并观察 `bevy_external_image_upload_queued` 后的 `bevy_external_image_resident` JSONL 记录。
- 为 external image 增加共享 target/final composite readback probe，输出 request ID、atlas generation/region/UV、frame/buffer pairing、producer sample 与 final pixel assertion。
