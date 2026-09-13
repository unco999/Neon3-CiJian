//! Backwards-compatible name for the deterministic retarget probe.
//!
//! The former version of this binary owned an `AnimState` and mutated styles
//! with probe-side `if/else` logic. That was not a valid proof of the NUI
//! state-machine boundary. The real interactive showcase now lives in
//! `neon-ui-runtime::animation_showcase_interactive_probe`; this legacy WGPU
//! name remains a compile-safe alias for the lower-level P0 probe.

include!("animation_retarget_probe.rs");
