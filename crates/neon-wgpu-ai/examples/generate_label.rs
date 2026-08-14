//! Real-weight conditional generation driven by class labels like `--parent desert`.
//! Loads a NEONWAI1 pack, maps text labels to the fixed class vocabularies in
//! `format.rs`, runs DDIM/CFG on the GPU, prints statistics and optionally
//! writes a grayscale PPM of the heightmap. This example deliberately mirrors
//! what a future UI/CLI flow does.
//!
//! Usage:
//!   cargo run -p neon-wgpu-ai --example generate_label -- \
//!     --pack assets/ai/terrain_run1/terrain_run1.pack \
//!     --sub dune_sea --parent desert --relief high --texture fine_ridged --water water_lots \
//!     --steps 10 --size 64 --out out.ppm

use neon_wgpu_ai::format::{
    PARENT_CLASSES, RELIEF_CLASSES, SUB_CLASSES, TEXTURE_CLASSES, WATER_CLASSES, TerrainCond,
};
use neon_wgpu_ai::{AiEngine, GenerateRequest};

fn class_index(name: &str, classes: &[&str]) -> Option<u32> {
    classes.iter().position(|c| *c == name).map(|i| i as u32)
}

fn main() {
    let mut pack_path = None;
    let mut out_path = None;
    let mut guidance = 7.0f32;
    let mut steps = 10u32;
    let mut size = 64u32;
    let mut seed = 42u64;
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().expect("flag value");
        match flag.as_str() {
            "--pack" => pack_path = Some(value),
            "--steps" => steps = value.parse().expect("steps"),
            "--size" => size = value.parse().expect("size"),
            "--seed" => seed = value.parse().expect("seed"),
            "--guidance" => guidance = value.parse().expect("guidance"),
            "--out" => out_path = Some(value),
            "--sub" | "--parent" | "--relief" | "--texture" | "--water" => {
                labels.push((flag.trim_start_matches("--").to_owned(), value));
            }
            other => panic!("unknown flag {other}"),
        }
    }
    let pack_path = pack_path.expect("--pack is required");

    let mut cond = TerrainCond::default();
    for (dim, value) in &labels {
        let index = match dim.as_str() {
            "sub" => class_index(value, &SUB_CLASSES).ok_or_else(|| format!("unknown sub '{value}'")),
            "parent" => class_index(value, &PARENT_CLASSES).ok_or_else(|| format!("unknown parent '{value}'")),
            "relief" => class_index(value, &RELIEF_CLASSES).ok_or_else(|| format!("unknown relief '{value}'")),
            "texture" => class_index(value, &TEXTURE_CLASSES).ok_or_else(|| format!("unknown texture '{value}'")),
            "water" => class_index(value, &WATER_CLASSES).ok_or_else(|| format!("unknown water '{value}'")),
            _ => unreachable!(),
        }
        .expect("label lookup failed");
        match dim.as_str() {
            "sub" => cond.sub = Some(index),
            "parent" => cond.parent = Some(index),
            "relief" => cond.relief = Some(index),
            "texture" => cond.texture = Some(index),
            "water" => cond.water = Some(index),
            _ => unreachable!(),
        }
    }
    println!("cond: sub={:?} parent={:?} relief={:?} texture={:?} water={:?}",
             cond.sub, cond.parent, cond.relief, cond.texture, cond.water);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .expect("no wgpu adapter available");
    let info = adapter.get_info();
    println!("adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("neon-wgpu-ai generate_label"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .expect("device request failed");

    let mut engine = AiEngine::new(device, queue);
    let bytes = std::fs::read(&pack_path).unwrap_or_else(|error| panic!("read {pack_path}: {error}"));
    let info = engine.load_model(&bytes).expect("model load failed");
    println!(
        "model loaded: {} params, {} MB resident",
        info.param_count,
        info.resident_bytes / 1_048_576
    );

    let generation = engine
        .generate(GenerateRequest {
            cond,
            guidance,
            steps,
            seed,
            size,
            preview_every: 0,
        })
        .expect("generation failed");
    let map = &generation.heightmap;
    let (mut lo, mut hi, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f64);
    for v in map {
        lo = lo.min(*v);
        hi = hi.max(*v);
        sum += *v as f64;
    }
    let mean = sum / map.len() as f64;
    let var = map.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / map.len() as f64;
    println!(
        "generated {size}x{size} steps={steps} gw={guidance}: min={lo:.4} max={hi:.4} mean={mean:.4} std={:.4} in {:.2}s",
        var.sqrt(),
        generation.elapsed_ms / 1000.0
    );
    assert!(map.iter().all(|v| v.is_finite()), "heightmap must be finite");

    if let Some(out_path) = out_path {
        let lo = map.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = map.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mut ppm = Vec::with_capacity(64 + map.len() * 3);
        ppm.extend_from_slice(format!("P6\n{size} {size}\n255\n").as_bytes());
        for v in map {
            let norm = ((v - lo) / (hi - lo).max(1e-9)).clamp(0.0, 1.0);
            let gray = (norm * 255.0).round() as u8;
            ppm.extend_from_slice(&[gray, gray, gray]);
        }
        std::fs::write(&out_path, ppm).expect("write ppm");
        println!("wrote {out_path}");
    }
    println!("GENERATE OK");
}