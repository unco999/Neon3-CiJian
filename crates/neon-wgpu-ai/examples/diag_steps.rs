//! NaN checkpoint diagnostic: runs the first DDIM step manually and reads
//! back every intermediate stage to find where non-finite values appear.
//! Usage:
//!   cargo run -p neon-wgpu-ai --example diag_steps -- \
//!     --pack assets/ai/terrain_run1/terrain_run1.pack --size 32 --steps 2

use neon_wgpu_ai::format::TerrainCond;
use neon_wgpu_ai::gpu::GpuCtx;
use neon_wgpu_ai::ops;
use neon_wgpu_ai::unet::UnetExecutor;
use neon_wgpu_ai::{AiEngine, WeightPack};

fn stats(name: &str, data: &[f32]) {
    let mut nan = 0usize;
    let mut inf = 0usize;
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    for v in data {
        if v.is_nan() {
            nan += 1;
        } else if v.is_infinite() {
            inf += 1;
        } else {
            lo = lo.min(*v);
            hi = hi.max(*v);
            sum += *v as f64;
        }
    }
    let mean = if data.is_empty() { 0.0 } else { sum / data.len() as f64 };
    println!("{name}: n={} nan={} inf={} min={lo} max={hi} mean={mean:.4}", data.len(), nan, inf);
}

fn main() {
    let mut pack_path = None;
    let mut steps = 2u32;
    let mut size = 32u32;
    let mut seed = 42u64;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().expect("flag value");
        match flag.as_str() {
            "--pack" => pack_path = Some(value),
            "--steps" => steps = value.parse().expect("steps"),
            "--size" => size = value.parse().expect("size"),
            "--seed" => seed = value.parse().expect("seed"),
            other => panic!("unknown flag {other}"),
        }
    }
    let pack_path = pack_path.expect("--pack is required");
    let len = size as u64 * size as u64;

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
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("neon-wgpu-ai diag_steps"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        },
    ))
    .expect("device request failed");

    let bytes = std::fs::read(&pack_path).unwrap_or_else(|error| panic!("read {pack_path}: {error}"));
    let pack = WeightPack::parse(&bytes).expect("pack parse");
    for name in [
        "downs.0.b1.n1.weight",
        "downs.0.b2.n1.weight",
        "downs.0.b1.c1.weight",
        "downs.0.b2.c1.weight",
        "downs.0.b1.film.weight",
        "downs.0.b2.film.weight",
        "downs.0.b1.n1.bias",
        "downs.0.b2.n1.bias",
    ] {
        let t = pack.tensor(name).expect("tensor");
        let v = t.to_f32().expect("f32");
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for x in &v {
            lo = lo.min(*x);
            hi = hi.max(*x);
        }
        println!("cpu {name}: n={} min={lo} max={hi}", v.len());
    }

    let mut engine = AiEngine::new(device, queue);
    engine.load_model(&bytes).expect("model load failed");

    let (model, ctx) = engine.model_and_ctx();
    let noise = ops::randn(ctx, len, seed);
    stats("randn", &ctx.readback_f32(&noise.buffer, len as usize).expect("readback randn"));

    let (t, t0) = model.schedule.ddim_times(steps, 0);
    println!("step0: t={t} t0={t0}");

    let cond = TerrainCond {
        sub: Some(6),
        parent: Some(1),
        relief: Some(3),
        texture: Some(2),
        water: Some(2),
    };
    let null_idx = TerrainCond::default().indices();

    let ec = {
        let mut executor = UnetExecutor::new(ctx, model);
        executor
            .forward(&noise.buffer, t, cond.indices(), size)
            .expect("forward cond")
    };
    stats("forward(cond)", &ctx.readback_f32(&ec.buffer, len as usize).expect("readback ec"));

    let eu = {
        let mut executor = UnetExecutor::new(ctx, model);
        executor
            .forward(&noise.buffer, t, null_idx, size)
            .expect("forward null")
    };
    stats("forward(null)", &ctx.readback_f32(&eu.buffer, len as usize).expect("readback eu"));

    let e = ops::cfg_combine(ctx, &ec.buffer, &eu.buffer, 7.0, len);
    stats("cfg_combine", &ctx.readback_f32(&e.buffer, len as usize).expect("readback e"));

    let (sab_t, s1ab_t) = (model.schedule.sab[t as usize], model.schedule.s1ab[t as usize]);
    let (sab_t0, s1ab_t0) = (model.schedule.sab[t0 as usize], model.schedule.s1ab[t0 as usize]);
    let next = ops::ddim_step(ctx, &noise.buffer, &e.buffer, sab_t, s1ab_t, sab_t0, s1ab_t0, len);
    stats("ddim_step", &ctx.readback_f32(&next.buffer, len as usize).expect("readback next"));
    println!("schedule: sab[t]={sab_t} s1ab[t]={s1ab_t} sab[t0]={sab_t0} s1ab[t0]={s1ab_t0}");
}
