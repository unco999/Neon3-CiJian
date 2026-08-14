//! GPU smoke test: proves every kernel compiles and runs on a real device and
//! that the full DDIM sampling path executes end to end with a random-weight
//! pack (no PyTorch needed). Runs headless (no window), device created by the
//! example itself — this example exists purely as the crate's self-check.

use neon_wgpu_ai::format::{TerrainCond, TerrainUnetSpec};
use neon_wgpu_ai::{AiEngine, GenerateRequest};

fn random_pack(seed: u64) -> Vec<u8> {
    use neon_wgpu_ai::format::{MAGIC, FORMAT_VERSION, MODEL_KIND_TERRAIN_UNET_DDIM_V1};
    let mut state = seed | 1;
    let mut rng = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state & 0xFFFF_FFFF) as f32) / (1u64 << 32) as f32 * 2.0 - 1.0
    };
    let spec = TerrainUnetSpec::default_v1();
    let layout = spec.terrain_unet_layout();
    let meta = format!(
        r#"{{"model_kind":"{MODEL_KIND_TERRAIN_UNET_DDIM_V1}","dtype":"f32","T":1000,"base":96,"schedule":"cosine","source_ckpt":"random-fixture","param_count":67400000,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","created_at":"2026-01-01T00:00:00Z"}}"#
    );
    let mut out = Vec::with_capacity(280_000_000);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // model kind
    out.extend_from_slice(&0u32.to_le_bytes()); // dtype f32
    out.extend_from_slice(&(layout.len() as u32).to_le_bytes());
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(meta.as_bytes());
    for (name, dims) in &layout {
        let numel = dims.iter().map(|d| *d as u64).product::<u64>() as usize;
        let bytes: Vec<u8> = (0..numel).flat_map(|_| rng().to_le_bytes()).collect();
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in dims {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

fn main() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter available");
    let info = adapter.get_info();
    println!("adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type);
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("neon-wgpu-ai smoke"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("device request failed");

    let mut engine = AiEngine::new(device, queue);

    let started = std::time::Instant::now();
    let pack = random_pack(0x5EED);
    println!("random pack: {} MB generated in {:.1}s", pack.len() / 1_048_576, started.elapsed().as_secs_f64());

    let info = engine.load_model(&pack).expect("model load failed");
    println!(
        "model loaded: {} params, {} MB resident",
        info.param_count,
        info.resident_bytes / 1_048_576
    );

    // One tiny conditional generation: 32x32, 2 DDIM steps, no CFG.
    let generation = engine
        .generate(GenerateRequest {
            cond: TerrainCond {
                sub: Some(0),
                parent: Some(4),
                relief: Some(4),
                texture: Some(3),
                water: Some(0),
            },
            guidance: 0.0,
            steps: 2,
            seed: 42,
            size: 32,
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
        "generation 32x32 steps=2: min={lo:.4} max={hi:.4} mean={mean:.4} std={:.4} in {:.2}s",
        var.sqrt(),
        generation.elapsed_ms / 1000.0
    );
    assert!(map.iter().all(|v| v.is_finite()), "heightmap must be finite");

    // A second identical generation must be byte-identical (determinism).
    let again = engine
        .generate(GenerateRequest {
            cond: TerrainCond {
                sub: Some(0),
                parent: Some(4),
                relief: Some(4),
                texture: Some(3),
                water: Some(0),
            },
            guidance: 0.0,
            steps: 2,
            seed: 42,
            size: 32,
            preview_every: 0,
        })
        .expect("second generation failed");
    assert_eq!(map, &again.heightmap, "same seed must reproduce the same heightmap");

    // CFG path with guidance, 1 step.
    let cfg = engine
        .generate(GenerateRequest {
            cond: TerrainCond {
                sub: Some(1),
                parent: Some(7),
                relief: None,
                texture: None,
                water: Some(2),
            },
            guidance: 7.0,
            steps: 1,
            seed: 7,
            size: 32,
            preview_every: 0,
        })
        .expect("cfg generation failed");
    assert!(cfg.heightmap.iter().all(|v| v.is_finite()));
    println!(
        "cfg generation ok: {} elements, gpu elapsed total {:.2}s",
        cfg.heightmap.len(),
        engine.ctx().elapsed_ms / 1000.0
    );
    println!("SMOKE OK");
}