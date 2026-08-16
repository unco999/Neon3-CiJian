//! Verify a real NEONWAI1 pack produced by `assets/ai/terrain_run1/convert_ckpt.py`.
//! Contract-level check only: parse, meta, structural schema validation and
//! shader-independent statistics. Does not touch a device (headless safe).
//!
//! Usage:
//!   cargo run -p neon-wgpu-ai --example verify_pack -- <path-to.pack>

use neon_wgpu_ai::format::{TerrainUnetSpec, WeightPack};

fn main() {
    let path = std::env::args().nth(1).expect("usage: verify_pack <path-to.pack>");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let pack = WeightPack::parse(&bytes).expect("pack parse failed");
    let spec = TerrainUnetSpec::default_v1();
    spec.validate_terrain_pack(&pack).expect("structural schema validation failed");

    let count: u64 = pack.tensors.values().map(|t| t.numel()).sum();
    println!(
        "pack OK: model={} dtype={} T={} base={} schedule={} source={}",
        pack.meta.model_kind, pack.meta.dtype, pack.meta.T, pack.meta.base, pack.meta.schedule, pack.meta.source_ckpt
    );
    println!(
        "tensors={} params={} resident_bytes={} sha256={}",
        pack.tensors.len(),
        count,
        pack.meta.param_count * 4,
        pack.meta.sha256
    );
    assert_eq!(pack.meta.param_count, 67_383_809);
    println!("VERIFY OK");
}
