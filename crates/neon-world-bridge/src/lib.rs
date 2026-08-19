//! Cross-language, renderer-neutral world and camera synchronization contract.
//!
//! This crate contains only revisioned control data and latest-value camera
//! samples. It never exposes GPU resources, renderer-local matrices, or pointers.

use std::collections::BTreeMap;

use neon_protocol::Revision;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorldSpaceId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CameraId(pub String);

/// Stable host-side identity for a world-space UI anchor. The host owns the
/// identity (for example a gameplay object id such as `player.main`); Neon only
/// samples the latest world-space position for screen projection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorldAnchorId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraKind {
    TwoDimensional,
    ThreeDimensional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    RightHandedYUpNegativeZForward,
    RightHandedZUpNegativeYForward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldPrecisionMode {
    CameraRelativeF64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldInformationSnapshot {
    pub world_space_id: WorldSpaceId,
    pub revision: Revision,
    pub coordinate_system: CoordinateSystem,
    pub units_per_meter: f64,
    pub precision_mode: WorldPrecisionMode,
}

impl WorldInformationSnapshot {
    pub fn validate(&self) -> Result<(), WorldBridgeError> {
        if self.world_space_id.0.trim().is_empty() {
            return Err(WorldBridgeError::InvalidWorldInformation);
        }
        if !self.units_per_meter.is_finite() || self.units_per_meter <= 0.0 {
            return Err(WorldBridgeError::InvalidWorldInformation);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CameraFramePayload {
    TwoDimensional {
        center: [f64; 2],
        rotation_radians: f32,
        zoom: f32,
    },
    ThreeDimensional {
        position: [f64; 3],
        orientation: [f32; 4],
        vertical_fov_radians: f32,
        near: f32,
        far: f32,
    },
}

impl CameraFramePayload {
    pub fn kind(&self) -> CameraKind {
        match self {
            Self::TwoDimensional { .. } => CameraKind::TwoDimensional,
            Self::ThreeDimensional { .. } => CameraKind::ThreeDimensional,
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::TwoDimensional {
                center,
                rotation_radians,
                zoom,
            } => {
                center.iter().all(|value| value.is_finite())
                    && rotation_radians.is_finite()
                    && zoom.is_finite()
                    && *zoom > 0.0
            }
            Self::ThreeDimensional {
                position,
                orientation,
                vertical_fov_radians,
                near,
                far,
            } => {
                position.iter().all(|value| value.is_finite())
                    && orientation.iter().all(|value| value.is_finite())
                    && orientation.iter().map(|value| value * value).sum::<f32>() > 0.0
                    && vertical_fov_radians.is_finite()
                    && *vertical_fov_radians > 0.0
                    && *vertical_fov_radians < std::f32::consts::PI
                    && near.is_finite()
                    && far.is_finite()
                    && *near > 0.0
                    && far > near
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraFrame {
    pub camera_id: CameraId,
    pub world_space_id: WorldSpaceId,
    pub producer_epoch: u64,
    pub sequence: u64,
    pub timestamp_monotonic_ns: u64,
    pub payload: CameraFramePayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraAvailability {
    Missing,
    Available { sequence: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldBridgeError {
    InvalidWorldInformation,
    InvalidCameraFrame,
    InvalidCameraControlSample,
    WorldSpaceMismatch,
    StaleCameraFrame,
    InvalidWorldAnchor,
    StaleWorldAnchor,
}

/// Latest-value input from an optional camera-control provider. Transport
/// adapters (including UDP) must validate this contract before WGPU consumes it.
/// It contains semantic camera axes only: no UI node, pointer position, endpoint,
/// or GPU resource crosses this boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraControlSample {
    pub camera_id: CameraId,
    pub session_id: String,
    pub producer_epoch: u64,
    pub sequence: u64,
    pub timestamp_monotonic_ns: u64,
    pub movement_axes: [f32; 3],
    pub look_delta: [f32; 2],
    pub wheel_delta: f32,
}

impl CameraControlSample {
    pub fn validate(&self) -> Result<(), WorldBridgeError> {
        if self.camera_id.0.trim().is_empty()
            || self.session_id.trim().is_empty()
            || self.sequence == 0
            || !self.movement_axes.iter().all(|value| value.is_finite())
            || !self.look_delta.iter().all(|value| value.is_finite())
            || !self.wheel_delta.is_finite()
        {
            return Err(WorldBridgeError::InvalidCameraControlSample);
        }
        Ok(())
    }
}

/// Renderer-local focus gate for optional camera-control transports. Providers
/// cannot activate camera input; only the focused RenderSurface can do so.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraControlFocus {
    /// Stable RenderSurface registry target, never a UI node or renderer hit ID.
    pub render_surface_target_id: String,
    pub camera_id: CameraId,
    pub session_id: String,
    pub renderer_epoch: u64,
    pub provider_epoch: u64,
    pub focused: bool,
}

impl CameraControlFocus {
    pub fn accepts(&self, sample: &CameraControlSample) -> bool {
        self.focused
            && !self.render_surface_target_id.trim().is_empty()
            && self.camera_id == sample.camera_id
            && self.session_id == sample.session_id
            && self.provider_epoch == sample.producer_epoch
            && sample.validate().is_ok()
    }
}

/// Latest-value world-space position of a host-owned UI anchor.
///
/// The anchor carries only identity and a world-space point. It never carries a
/// renderer-local matrix, screen coordinate, UI node, or GPU resource. The sole
/// renderer owner projects it to screen space using the matching camera frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldUiAnchor {
    pub anchor_id: WorldAnchorId,
    pub world_space_id: WorldSpaceId,
    pub producer_epoch: u64,
    pub sequence: u64,
    pub timestamp_monotonic_ns: u64,
    /// World-space anchor point. Position zero is valid.
    pub position: [f64; 3],
    /// Whether the host requests a billboard-style facing presentation. Neon
    /// renders the UI in its own fullscreen composition regardless; this flag is
    /// a presentation hint, never a 3D quad or texture request.
    pub billboard: bool,
}

impl WorldUiAnchor {
    fn is_valid(&self) -> bool {
        !self.anchor_id.0.trim().is_empty()
            && self.sequence != 0
            && self.position.iter().all(|value| value.is_finite())
    }
}

/// Latest-value world state for a single renderer session.
///
/// Position zero is valid. A camera is missing only before its first valid
/// frame, which keeps a camera located at the world origin renderable.
#[derive(Clone, Debug, Default)]
pub struct WorldInformationBridge {
    world: Option<WorldInformationSnapshot>,
    cameras: BTreeMap<CameraId, CameraFrame>,
    anchors: BTreeMap<WorldAnchorId, WorldUiAnchor>,
}

impl WorldInformationBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn world(&self) -> Option<&WorldInformationSnapshot> {
        self.world.as_ref()
    }

    pub fn configure_world(
        &mut self,
        snapshot: WorldInformationSnapshot,
    ) -> Result<(), WorldBridgeError> {
        snapshot.validate()?;
        self.world = Some(snapshot);
        self.cameras.clear();
        self.anchors.clear();
        Ok(())
    }

    pub fn submit_camera_frame(&mut self, frame: CameraFrame) -> Result<(), WorldBridgeError> {
        let Some(world) = self.world.as_ref() else {
            return Err(WorldBridgeError::WorldSpaceMismatch);
        };
        if frame.world_space_id != world.world_space_id {
            return Err(WorldBridgeError::WorldSpaceMismatch);
        }
        if frame.sequence == 0 || !frame.payload.is_valid() {
            return Err(WorldBridgeError::InvalidCameraFrame);
        }
        if let Some(previous) = self.cameras.get(&frame.camera_id)
            && (previous.producer_epoch > frame.producer_epoch
                || (previous.producer_epoch == frame.producer_epoch
                    && previous.sequence >= frame.sequence))
        {
            return Err(WorldBridgeError::StaleCameraFrame);
        }
        self.cameras.insert(frame.camera_id.clone(), frame);
        Ok(())
    }

    /// Stores the latest world-space position for a host-owned UI anchor. It
    /// applies the same world-space matching and monotonic-sequence checks as
    /// camera frames, so a stale or wrong-world anchor never reaches rendering.
    pub fn submit_anchor(&mut self, anchor: WorldUiAnchor) -> Result<(), WorldBridgeError> {
        let Some(world) = self.world.as_ref() else {
            return Err(WorldBridgeError::WorldSpaceMismatch);
        };
        if anchor.world_space_id != world.world_space_id {
            return Err(WorldBridgeError::WorldSpaceMismatch);
        }
        if !anchor.is_valid() {
            return Err(WorldBridgeError::InvalidWorldAnchor);
        }
        if let Some(previous) = self.anchors.get(&anchor.anchor_id)
            && (previous.producer_epoch > anchor.producer_epoch
                || (previous.producer_epoch == anchor.producer_epoch
                    && previous.sequence >= anchor.sequence))
        {
            return Err(WorldBridgeError::StaleWorldAnchor);
        }
        self.anchors.insert(anchor.anchor_id.clone(), anchor);
        Ok(())
    }

    pub fn anchor(&self, anchor_id: &WorldAnchorId) -> Option<&WorldUiAnchor> {
        self.anchors.get(anchor_id)
    }

    pub fn anchors(&self) -> &BTreeMap<WorldAnchorId, WorldUiAnchor> {
        &self.anchors
    }

    /// Latest valid camera frame for a camera id, regardless of kind.
    pub fn camera(&self, camera_id: &CameraId) -> Option<&CameraFrame> {
        self.cameras.get(camera_id)
    }

    pub fn camera_availability(
        &self,
        camera_id: &CameraId,
        kind: CameraKind,
    ) -> CameraAvailability {
        self.cameras
            .get(camera_id)
            .filter(|frame| frame.payload.kind() == kind)
            .map_or(CameraAvailability::Missing, |frame| {
                CameraAvailability::Available {
                    sequence: frame.sequence,
                }
            })
    }

    pub fn camera_is_available(&self, camera_id: &CameraId, kind: CameraKind) -> bool {
        matches!(
            self.camera_availability(camera_id, kind),
            CameraAvailability::Available { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> WorldInformationSnapshot {
        WorldInformationSnapshot {
            world_space_id: WorldSpaceId("project.world.main".into()),
            revision: Revision(1),
            coordinate_system: CoordinateSystem::RightHandedYUpNegativeZForward,
            units_per_meter: 1.0,
            precision_mode: WorldPrecisionMode::CameraRelativeF64,
        }
    }

    #[test]
    fn camera_at_origin_is_available_after_a_valid_frame() {
        let mut bridge = WorldInformationBridge::new();
        bridge.configure_world(world()).unwrap();
        let camera_id = CameraId("editor".into());
        assert_eq!(
            bridge.camera_availability(&camera_id, CameraKind::ThreeDimensional),
            CameraAvailability::Missing
        );
        bridge
            .submit_camera_frame(CameraFrame {
                camera_id: camera_id.clone(),
                world_space_id: WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                payload: CameraFramePayload::ThreeDimensional {
                    position: [0.0, 0.0, 0.0],
                    orientation: [0.0, 0.0, 0.0, 1.0],
                    vertical_fov_radians: 1.0,
                    near: 0.1,
                    far: 1000.0,
                },
            })
            .unwrap();
        assert!(bridge.camera_is_available(&camera_id, CameraKind::ThreeDimensional));
    }

    #[test]
    fn bridge_rejects_stale_or_wrong_world_frames() {
        let mut bridge = WorldInformationBridge::new();
        bridge.configure_world(world()).unwrap();
        let frame = CameraFrame {
            camera_id: CameraId("map".into()),
            world_space_id: WorldSpaceId("wrong".into()),
            producer_epoch: 1,
            sequence: 1,
            timestamp_monotonic_ns: 1,
            payload: CameraFramePayload::TwoDimensional {
                center: [0.0, 0.0],
                rotation_radians: 0.0,
                zoom: 1.0,
            },
        };
        assert_eq!(
            bridge.submit_camera_frame(frame),
            Err(WorldBridgeError::WorldSpaceMismatch)
        );
    }

    #[test]
    fn camera_control_requires_the_matching_focused_surface_session() {
        let sample = CameraControlSample {
            camera_id: CameraId("world-ui-lab".into()),
            session_id: "lab-session".into(),
            producer_epoch: 4,
            sequence: 1,
            timestamp_monotonic_ns: 10,
            movement_axes: [0.0, 1.0, 0.0],
            look_delta: [1.0, -1.0],
            wheel_delta: 0.0,
        };
        let focused = CameraControlFocus {
            render_surface_target_id: "world-ui-lab.preview".into(),
            camera_id: sample.camera_id.clone(),
            session_id: sample.session_id.clone(),
            renderer_epoch: 4,
            provider_epoch: 4,
            focused: true,
        };
        assert!(focused.accepts(&sample));
        assert!(
            !CameraControlFocus {
                focused: false,
                ..focused
            }
            .accepts(&sample)
        );
    }

    #[test]
    fn anchor_at_position_is_stored_and_queryable() {
        let mut bridge = WorldInformationBridge::new();
        bridge.configure_world(world()).unwrap();
        let anchor_id = WorldAnchorId("player.main".into());
        assert!(bridge.anchor(&anchor_id).is_none());
        bridge
            .submit_anchor(WorldUiAnchor {
                anchor_id: anchor_id.clone(),
                world_space_id: WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                position: [1.0, 2.0, 3.0],
                billboard: true,
            })
            .unwrap();
        let stored = bridge.anchor(&anchor_id).expect("anchor stored");
        assert_eq!(stored.position, [1.0, 2.0, 3.0]);
        assert!(stored.billboard);
    }

    #[test]
    fn bridge_rejects_stale_and_wrong_world_anchors() {
        let mut bridge = WorldInformationBridge::new();
        bridge.configure_world(world()).unwrap();
        let anchor_id = WorldAnchorId("player.main".into());
        let base = WorldUiAnchor {
            anchor_id: anchor_id.clone(),
            world_space_id: WorldSpaceId("project.world.main".into()),
            producer_epoch: 1,
            sequence: 1,
            timestamp_monotonic_ns: 1,
            position: [0.0, 0.0, 0.0],
            billboard: true,
        };
        bridge.submit_anchor(base.clone()).unwrap();
        // Wrong world space is rejected.
        assert_eq!(
            bridge.submit_anchor(WorldUiAnchor {
                world_space_id: WorldSpaceId("wrong".into()),
                sequence: 2,
                ..base.clone()
            }),
            Err(WorldBridgeError::WorldSpaceMismatch)
        );
        // A stale (non-increasing) sequence is rejected.
        assert_eq!(
            bridge.submit_anchor(WorldUiAnchor {
                sequence: 1,
                ..base.clone()
            }),
            Err(WorldBridgeError::StaleWorldAnchor)
        );
        // A non-finite position is rejected.
        assert_eq!(
            bridge.submit_anchor(WorldUiAnchor {
                sequence: 2,
                position: [f64::NAN, 0.0, 0.0],
                ..base
            }),
            Err(WorldBridgeError::InvalidWorldAnchor)
        );
    }
}
