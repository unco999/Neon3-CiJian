//! NaN checkpoint diagnostic: runs the first DDIM step manually and reads
//! back every intermediate stage to find where non-finite values appear.
//! Usage:
//!   cargo run -p neon-wgpu-ai --example diag_steps -- \
//!     --pack assets/ai/terrain_run1/terrain_run1.pack --size 32 --steps 2

use neon_wgpu_ai::format::TerrainCond;
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

    let a8: Vec<f32> = (0..8).map(|i| (i as f32) + 1.0).collect();
    let mbuf = ctx.upload(bytemuck::cast_slice(&a8), "mat_a");
    for (k, n) in [(8usize, 4usize), (8usize, 32usize)] {
        let weight_nk: Vec<f32> = (0..n * k).map(|i| (i as f32) + 1.0).collect();
        let weight_kn: Vec<f32> = (0..k * n)
            .map(|i| weight_nk[(i % n) * k + i / n])
            .collect();
        let wbuf_nk = ctx.upload(bytemuck::cast_slice(&weight_nk), "mat_w_nk");
        let wbuf_kn = ctx.upload(bytemuck::cast_slice(&weight_kn), "mat_w_kn");
        let expect: Vec<f32> = (0..n)
            .map(|j| (0..k).map(|i| a8[i] * weight_nk[j * k + i]).sum())
            .collect();
        for (tb, v) in [
            (true, ops::matmul_t(ctx, &mbuf, &wbuf_nk, 1, n as u32, k as u32, false, true)),
            (false, ops::matmul_t(ctx, &mbuf, &wbuf_kn, 1, n as u32, k as u32, false, false)),
        ] {
            let v = ctx.readback_f32(&v.buffer, n).expect("mat out");
            let ok = v.iter().zip(&expect).all(|(a, b)| (a - b).abs() < 1e-2);
            println!("matmul m=1 k={k} n={n} trans_b={tb}: ok={ok} got_first4={:?} expect_first4={:?}", &v[..n.min(4)], &expect[..n.min(4)]);
            assert!(ok, "matmul regression failed for trans_b={tb}");
        }
    }

    let scores_cpu = [1.0f32, 2.0, 3.0, 4.0, 4.0, 3.0, 2.0, 1.0, -2.0, 0.0, 2.0, 4.0];
    let scores = ctx.upload(bytemuck::cast_slice(&scores_cpu), "softmax_rows");
    ops::softmax_rows(ctx, &scores, 3, 4, 0, 1.0);
    let scores_gpu = ctx.readback_f32(&scores, scores_cpu.len()).expect("softmax out");
    for (got, input) in scores_gpu.chunks_exact(4).zip(scores_cpu.chunks_exact(4)) {
        let max = input.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let denom: f32 = input.iter().map(|x| (*x - max).exp()).sum();
        for (actual, x) in got.iter().zip(input) {
            let expected = (*x - max).exp() / denom;
            assert!((actual - expected).abs() < 1e-5, "softmax regression: {actual} != {expected}");
        }
    }
    println!("softmax rows regression: ok");

    let gn_input: Vec<f32> = (0..8).flat_map(|c| [c as f32, c as f32 + 2.0]).collect();
    let gn_weight = ctx.upload(bytemuck::cast_slice(&[1.0f32; 8]), "gn_weight");
    let gn_bias = ctx.upload(bytemuck::cast_slice(&[0.0f32; 8]), "gn_bias");
    let gn_x = ctx.upload(bytemuck::cast_slice(&gn_input), "gn_input");
    let gn = ops::group_norm(ctx, &gn_x, &gn_weight, &gn_bias, 8, 8, 1, 2, 1e-5, "gn_regression")
        .expect("group norm");
    let gn_gpu = ctx.readback_f32(&gn.buffer, gn_input.len()).expect("group norm out");
    for (i, actual) in gn_gpu.iter().enumerate() {
        let expected = if i % 2 == 0 { -0.999995 } else { 0.999995 };
        assert!((actual - expected).abs() < 1e-4, "group norm regression at {i}: {actual} != {expected}");
    }
    println!("group norm 8-group regression: ok");

    let film_input = [1.0f32, 2.0, 3.0, 10.0, 20.0, 30.0];
    let film_params = [0.5f32, -0.25, 1.0, -2.0];
    let film_input_buf = ctx.upload(bytemuck::cast_slice(&film_input), "film_regression_input");
    let film_params_buf = ctx.upload(bytemuck::cast_slice(&film_params), "film_regression_params");
    let film = ops::film(ctx, &film_input_buf, &film_params_buf, 2, film_input.len() as u64);
    let film_gpu = ctx.readback_f32(&film.buffer, film_input.len()).expect("film regression out");
    let film_expected = [2.5f32, 4.0, 5.5, 5.5, 13.0, 20.5];
    for (index, (actual, expected)) in film_gpu.iter().zip(film_expected).enumerate() {
        assert!((actual - expected).abs() < 1e-6, "FiLM NCHW regression at {index}: {actual} != {expected}");
    }
    println!("film NCHW channel regression: ok");

    let ddim_x = [0.8f32, -0.2, 1.5, -1.0];
    let ddim_e = [0.1f32, 0.4, -0.2, 0.3];
    let ddim_x_buf = ctx.upload(bytemuck::cast_slice(&ddim_x), "ddim_regression_x");
    let ddim_e_buf = ctx.upload(bytemuck::cast_slice(&ddim_e), "ddim_regression_e");
    let (sab_t, s1ab_t, sab_t0, s1ab_t0) = (0.5f32, 0.25f32, 0.75f32, 0.125f32);
    let ddim = ops::ddim_step(ctx, &ddim_x_buf, &ddim_e_buf, sab_t, s1ab_t, sab_t0, s1ab_t0, 4);
    let ddim_gpu = ctx.readback_f32(&ddim.buffer, 4).expect("ddim regression out");
    for (index, actual) in ddim_gpu.iter().enumerate() {
        let x0h = ((ddim_x[index] - s1ab_t * ddim_e[index]) / sab_t).clamp(-3.0, 3.0);
        let expected = sab_t0 * x0h + s1ab_t0 * ddim_e[index];
        assert!((actual - expected).abs() < 1e-6, "DDIM regression at {index}: {actual} != {expected}");
    }
    println!("ddim coefficient regression: ok");

    let noise_a = ops::randn(ctx, 64, 42);
    let noise_b = ops::randn(ctx, 64, 43);
    let noise_a_repeat = ops::randn(ctx, 64, 42);
    let noise_a_values = ctx.readback_f32(&noise_a.buffer, 64).expect("noise seed 42");
    let noise_b_values = ctx.readback_f32(&noise_b.buffer, 64).expect("noise seed 43");
    let noise_a_repeat_values = ctx.readback_f32(&noise_a_repeat.buffer, 64).expect("noise seed 42 repeat");
    assert_ne!(noise_a_values, noise_b_values, "different seeds must produce different latent noise");
    assert_eq!(noise_a_values, noise_a_repeat_values, "the same seed must be deterministic");
    println!("randn seed regression: ok");

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
