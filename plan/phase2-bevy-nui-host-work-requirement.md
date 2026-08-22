# Phase 2 工作需求：修正 bevy-nui-host latest-value 数据流

> **目标仓库**: `D:\bevy-nui-host`（外部仓库，Neon3 工作区外）
> **对应计划**: `plan/性能优化2026822.md` 第 3 节（第二阶段）
> **优先级**: 高 — 当前 camera/anchor FIFO backlog 会导致 WorldUi 视觉延迟和 dropped frame

---

## 1. 背景

当前 `bevy-nui-host` 的 `publish_camera_snapshot()` 和 `publish_world_anchor()` 在每个 ECS Update 中向同一个 WGPU RPC FIFO 投递请求。相机拖动时会出现：

```
camera_1, anchor_1, camera_2, anchor_2, camera_3, anchor_3, ...
```

旧请求没有价值但仍排队执行，导致：
- 用户相机已经到新位置，renderer 仍在消费旧 camera/anchor
- WorldUi 视觉延迟追赶
- dropped frame 增加
- pointer 事件被 camera/anchor FIFO 阻塞

---

## 2. 必须修改的文件

**唯一文件**: `D:\bevy-nui-host\src\lib.rs`

---

## 3. 具体需求

### 3.1 LatestCameraFrame / LatestAnchorBatch 结构（替代 in_flight: bool）

当前使用 `camera_in_flight: bool` 和 `anchor_in_flight: bool`，不够。

**必须增加**（二选一）：

选项 A — 独立结构：

```rust
struct LatestCameraFrame {
    frame: CameraFrame,
    signature: WorldFrameSignature,
    dirty: bool,
    in_flight: bool,
}

struct LatestAnchorBatch {
    batch: WorldUiAnchorBatch,
    signature: WorldFrameSignature,
    dirty: bool,
    in_flight: bool,
}
```

选项 B — 统一资源（推荐）：

```rust
#[derive(Resource, Default)]
struct LatestWorldSubmission {
    camera: Option<PendingCameraSubmission>,
    anchors: Option<PendingAnchorSubmission>,
}

struct PendingCameraSubmission {
    frame: CameraFrame,
    signature: WorldFrameSignature,
    dirty: bool,
    in_flight: bool,
}

struct PendingAnchorSubmission {
    batch: WorldUiAnchorBatch,
    signature: WorldFrameSignature,
    dirty: bool,
    in_flight: bool,
}
```

### 3.2 生命周期规则

**每次 ECS Update**:
1. 计算当前最新 camera/anchor
2. 如果 `signature == last_sent`: 不做任何事（**静默跳过**）
3. 如果 `signature != last_sent`: 覆盖 pending latest，设 `dirty = true`

**flush**:
1. 如果没有 `in_flight` 且 `dirty`:
   - 发送 pending latest
   - `in_flight = true`
   - `dirty = false`

**收到 response**:
1. `in_flight = false`
2. 如果期间又产生新 pending: 下一次 Update 发送最新值

**绝对不能**:
- ❌ 收到 response 后无条件清除 `last_sent` signature
- ✅ 否则静止状态会被重复发送

### 3.3 WorldFrameSignature（避免碰撞）

不要只用 `DefaultHasher` 作为最终正确性依据。

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
struct WorldFrameSignature {
    camera_position: [u32; 3],      // f32.to_bits()
    camera_orientation: [u32; 4],    // f32.to_bits() × 4
    fov: u32,                        // f32.to_bits()
    near: u32,                       // f32.to_bits()
    far: u32,                        // f32.to_bits()
    anchor_count: u32,
    anchor_hash: u64,                // sorted anchor_id → hash
}
```

规则：
- 浮点值使用 `value.to_bits()`，不比较浮点近似
- anchor 顺序先按稳定 `anchor_id` 排序，再计算 hash
- hash 只用于性能跳过，**真正提交时仍使用完整 typed protocol payload**

### 3.4 Camera 和 Anchor 共同 frame sequence

当前 `CameraFrame` 和 `WorldUiAnchorBatch` 各自独立序号。

要求：
- `camera.sequence = world_frame_sequence`
- `anchor_batch.sequence = world_frame_sequence`
- 如果协议 schema 不允许修改，则 host 内部维护：

```rust
world_frame_sequence: u64
camera_sequence: u64
anchor_sequence: u64
```

并在诊断 trace 中关联：

```json
{
  "world_frame_sequence": 100,
  "camera_sequence": 100,
  "anchor_sequence": 100
}
```

### 3.5 两条 RPC lane（Pointer 不得被 Camera/Anchor FIFO 阻塞）

当前 endpoint worker 按 endpoint 串行。需要分成至少两条独立 lane：

| Lane | 职责 | 方法 |
|------|------|------|
| `world_latest_lane` | camera frame, anchor batch | `wgpu.world.camera.submit_frame`, `wgpu.world.ui.anchor.submit_batch` |
| `interaction_lane` | pointer event, semantic interaction | `ui.host.pointer_event`, `ui.host.semantic_click` |

要求：
- 两条 lane 都使用现有 **length-prefixed JSON over loopback TCP**
- 使用现有 `RpcRequest` / `RpcResponse` 类型
- 用**两个独立 `RpcClient` 实例**连接到同一个 WGPU endpoint
- Pointer lane **永远不能等待** world_latest_lane 里的旧请求
- 不是发明新协议

---

## 4. 验收标准

完成修改后必须通过以下验证：

| # | 验收项 | 验证方式 |
|---|--------|----------|
| 1 | 静止状态 camera 不重复发送 | 观察 RPC 日志，同一 camera 位置只发送一次 |
| 2 | 相机拖动时不产生 FIFO backlog | 日志中 camera 请求数不超过实际帧数 |
| 3 | pointer 事件不被 camera/anchor 阻塞 | pointer 请求在 camera 请求之前或同时处理 |
| 4 | WorldFrameSignature 正确跳过相同状态 | signature 比较后跳过无变化提交 |
| 5 | camera.sequence == anchor_batch.sequence | 日志中两个 sequence 值一致 |
| 6 | 响应处理不丢失新 pending 状态 | 连续快速拖动后最终位置正确送达 renderer |
| 7 | 测试通过 | `cargo test -p bevy-nui-host` 全部通过 |

---

## 5. 涉及的类型与函数（参考）

当前相关函数（需定位确切行号）：
- `publish_camera_snapshot()`
- `publish_world_anchor()`
- `camera_in_flight` / `anchor_in_flight` 字段
- RPC response 回调处理函数
- ECS Update 系统循环

协议类型（Neon3 workspace 中）：
- `CameraFrame` → `crates/neon-world-bridge/src/lib.rs`
- `WorldUiAnchorBatch` → `crates/neon-world-bridge/src/lib.rs`
- `RpcRequest` / `RpcResponse` → `crates/neon-protocol/src/lib.rs`
- `ui.host.pointer_event` → `crates/neon-wgpu-runtime/src/lib.rs`（headless server handler）

---

## 6. 注意事项

1. **不要修改 Neon3 工作区代码** — 所有改动仅限于 `D:\bevy-nui-host`
2. **不要改变协议 schema** — 使用现有字段，必要时用 host 内部字段
3. **不要删除** `CameraFrame`、`WorldUiAnchorBatch`、`UiHitBinding` 等正式协议类型
4. 测试环境需要启动 `neon-wgpu-runtime --headless-server` 作为 mock 端点
5. 修改后需运行 `cargo test -p bevy-nui-host` 确保回归