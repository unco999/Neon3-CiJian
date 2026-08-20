//! End-to-end GPU test: the crit_combo script runs on a real headless device
//! and the exported `target.hp` is read back and verified against a CPU
//! reference computation of the same algorithm.

use std::collections::HashMap;

use neon_gpu_exec::codelet::{ConstArg, FieldTy};
use neon_gpu_exec::{Codelet, Executor, InputField};
use neon_gpu_script::{compile, KernelRegistry, WorldRegistry};

const N: u32 = 4;
const WORKGROUP: u32 = 64;

const CRIT_SRC: &str = r#"
schema_version: 1

scene crit_combo = {
    input:
        target.stats as stats,
        target.def as def,
        frame.frame as frame,
        target.hp as target_hp
    output:
        target.hp,
        frame.crit
    body:
        let dmg = damage_formula(stats, def, kind="physical")
        let crit = rng_chance(seed=frame, chance=0.25)
        let hit = select(dmg, mul(dmg, 2.0), crit)
        let hp = apply_damage(target_hp, hit)
        export target.hp = hp
        export frame.crit = crit
}
"#;

fn world() -> WorldRegistry {
    let mut w = WorldRegistry::new();
    w.register("target", "stats", "field<f32,[8]>", false);
    w.register("target", "def", "field<f32,[1]>", false);
    w.register("frame", "frame", "field<u32,[1]>", false);
    w.register("target", "hp", "field<f32,[1]>", true);
    w.register("frame", "crit", "field<f32,[1]>", true);
    w
}

fn kernels() -> KernelRegistry {
    let mut k = KernelRegistry::new();
    k.register("damage_formula", 2, &["kind"]);
    k.register("rng_chance", 0, &["seed", "chance"]);
    k.register("select", 3, &[]);
    k.register("mul", 2, &[]);
    k.register("apply_damage", 2, &[]);
    k
}

fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .expect("no adapter available");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("neon-gpu-exec test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: Default::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: Default::default(),
        },
    ))
    .expect("device request failed");
    (device, queue)
}

fn storage_buffer(device: &wgpu::Device, label: &str, words: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: words as u64 * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

// ---- kernels --------------------------------------------------------------

struct DamageFormula;
impl Codelet for DamageFormula {
    fn input_count(&self) -> usize {
        2
    }
    fn allowed_consts(&self) -> Vec<String> {
        vec!["kind".into()]
    }
    fn wgsl(&self, _consts: &[ConstArg], n: u32, _vt: &[FieldTy]) -> String {
        format!(
            r#"
@group(0) @binding(0) var<storage, read> stats: array<f32, {n8}>;
@group(0) @binding(1) var<storage, read> def: array<f32, {n}>;
@group(0) @binding(2) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    out[i] = stats[i * 8u] - def[i];
}}
"#,
            n8 = n * 8,
            wg = WORKGROUP
        )
    }
}

struct RngChance;
impl Codelet for RngChance {
    fn input_count(&self) -> usize {
        1
    }
    fn allowed_consts(&self) -> Vec<String> {
        vec!["chance".into()]
    }
    fn wgsl(&self, consts: &[ConstArg], n: u32, vt: &[FieldTy]) -> String {
        let chance = consts
            .iter()
            .find(|c| c.key == "chance")
            .and_then(ConstArg::as_f32)
            .unwrap_or(0.5);
        let seed_decl = match vt.first() {
            Some(FieldTy::U32) => format!("var<storage, read> seed: array<u32, {n}>;"),
            _ => format!("var<storage, read> seed: array<f32, {n}>;"),
        };
        format!(
            r#"
@group(0) @binding(0) {seed_decl}
@group(0) @binding(1) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    let s: u32 = u32(seed[i]);
    var h: u32 = s ^ 12345u;
    h = h ^ (h << 13u);
    h = h ^ (h >> 17u);
    h = h ^ (h << 5u);
    let r = f32(h & 0xFFFFFFu) / 16777216.0;
    if (r < {chance}f) {{ out[i] = 1.0; }} else {{ out[i] = 0.0; }}
}}
"#,
            wg = WORKGROUP
        )
    }
}

struct Mul;
impl Codelet for Mul {
    fn input_count(&self) -> usize {
        2
    }
    fn allowed_consts(&self) -> Vec<String> {
        vec!["#1".into()]
    }
    fn accepts(&self, value_count: usize, const_keys: &[String]) -> bool {
        (value_count == 2 && const_keys.is_empty())
            || (value_count == 1 && const_keys == ["#1".to_string()])
    }
    fn wgsl(&self, consts: &[ConstArg], n: u32, _vt: &[FieldTy]) -> String {
        if let Some(c) = consts.iter().find(|c| c.key == "#1") {
            let c = c.as_f32().unwrap();
            return format!(
                r#"
@group(0) @binding(0) var<storage, read> a: array<f32, {n}>;
@group(0) @binding(1) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    out[i] = a[i] * {c}f;
}}
"#,
                wg = WORKGROUP
            );
        }
        format!(
            r#"
@group(0) @binding(0) var<storage, read> a: array<f32, {n}>;
@group(0) @binding(1) var<storage, read> b: array<f32, {n}>;
@group(0) @binding(2) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    out[i] = a[i] * b[i];
}}
"#,
            wg = WORKGROUP
        )
    }
}

struct Select;
impl Codelet for Select {
    fn input_count(&self) -> usize {
        3
    }
    fn allowed_consts(&self) -> Vec<String> {
        vec![]
    }
    fn wgsl(&self, _consts: &[ConstArg], n: u32, _vt: &[FieldTy]) -> String {
        format!(
            r#"
@group(0) @binding(0) var<storage, read> a: array<f32, {n}>;
@group(0) @binding(1) var<storage, read> b: array<f32, {n}>;
@group(0) @binding(2) var<storage, read> mask: array<f32, {n}>;
@group(0) @binding(3) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    if (mask[i] > 0.5) {{ out[i] = b[i]; }} else {{ out[i] = a[i]; }}
}}
"#,
            wg = WORKGROUP
        )
    }
}

struct ApplyDamage;
impl Codelet for ApplyDamage {
    fn input_count(&self) -> usize {
        2
    }
    fn allowed_consts(&self) -> Vec<String> {
        vec![]
    }
    fn wgsl(&self, _consts: &[ConstArg], n: u32, _vt: &[FieldTy]) -> String {
        format!(
            r#"
@group(0) @binding(0) var<storage, read> hp: array<f32, {n}>;
@group(0) @binding(1) var<storage, read> hit: array<f32, {n}>;
@group(0) @binding(2) var<storage, read_write> out: array<f32, {n}>;

@compute @workgroup_size({wg})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i >= {n}u) {{ return; }}
    out[i] = max(hp[i] - hit[i], 0.0);
}}
"#,
            wg = WORKGROUP
        )
    }
}

// ---- CPU reference --------------------------------------------------------

fn crit_roll(seed: u32) -> f32 {
    let mut h = seed ^ 12345;
    h ^= h << 13;
    h ^= h >> 17;
    h ^= h << 5;
    (h & 0xFF_FFFF) as f32 / 16777216.0
}

fn expected_hp(atk: &[f32], def: &[f32], hp: &[f32], frame: &[u32]) -> Vec<f32> {
    atk.iter()
        .zip(def)
        .zip(hp)
        .zip(frame)
        .map(|(((a, d), h), f)| {
            let dmg = a - d;
            let hit = if crit_roll(*f) < 0.25 { dmg * 2.0 } else { dmg };
            (h - hit).max(0.0)
        })
        .collect()
}

// ---- test -----------------------------------------------------------------

#[test]
fn crit_combo_runs_on_gpu_and_reads_back_correct_hp() {
    let (device, queue) = headless_device();

    let compiled = compile(CRIT_SRC, &world(), &kernels()).expect("compile crit script");
    assert_eq!(compiled.scenes.len(), 1);
    let scene = &compiled.scenes[0];

    // Input data: 4 entities.
    let atk = [10.0f32, 8.0, 20.0, 5.0];
    let def = [2.0f32, 1.0, 5.0, 0.0];
    let hp = [50.0f32, 100.0, 30.0, 80.0];
    let frame = [1u32, 2, 3, 4];

    let mut stats_words = vec![0.0f32; (N * 8) as usize];
    for i in 0..N as usize {
        stats_words[i * 8] = atk[i];
    }
    let stats_buf = storage_buffer(&device, "stats", stats_words.len());
    queue.write_buffer(&stats_buf, 0, bytemuck::cast_slice(&stats_words));
    let def_buf = storage_buffer(&device, "def", N as usize);
    queue.write_buffer(&def_buf, 0, bytemuck::cast_slice(&def));
    let hp_buf = storage_buffer(&device, "hp", N as usize);
    queue.write_buffer(&hp_buf, 0, bytemuck::cast_slice(&hp));
    let frame_buf = storage_buffer(&device, "frame", N as usize);
    queue.write_buffer(&frame_buf, 0, bytemuck::cast_slice(&frame));

    let mut inputs = HashMap::new();
    inputs.insert("stats".into(), InputField { buffer: stats_buf, per_entity: 8, ty: FieldTy::F32 });
    inputs.insert("def".into(), InputField { buffer: def_buf, per_entity: 1, ty: FieldTy::F32 });
    inputs.insert("frame".into(), InputField { buffer: frame_buf, per_entity: 1, ty: FieldTy::U32 });
    inputs.insert("target_hp".into(), InputField { buffer: hp_buf, per_entity: 1, ty: FieldTy::F32 });

    let mut executor = Executor::new(device, queue);
    executor.register_codelet("damage_formula", Box::new(DamageFormula));
    executor.register_codelet("rng_chance", Box::new(RngChance));
    executor.register_codelet("mul", Box::new(Mul));
    executor.register_codelet("select", Box::new(Select));
    executor.register_codelet("apply_damage", Box::new(ApplyDamage));

    let outputs = executor.run(scene, &inputs).expect("gpu run failed");
    let hp_out = outputs.get("target.hp").expect("target.hp exported");

    let expected = expected_hp(&atk, &def, &hp, &frame);
    assert_eq!(hp_out.len() as u32, N);
    for i in 0..N as usize {
        assert!(
            (hp_out[i] - expected[i]).abs() < 1e-4,
            "entity {i}: gpu hp={} expected={}",
            hp_out[i],
            expected[i]
        );
    }

    // The script's own wave plan proves the parallelism we claimed.
    let dmg = scene.ir.nodes.iter().position(|n| n.result == "dmg").unwrap();
    let crit = scene.ir.nodes.iter().position(|n| n.result == "crit").unwrap();
    assert_eq!(scene.waves[0], vec![dmg, crit], "wave 0 must expose dmg+crit parallelism");
}

#[test]
fn missing_input_alias_rejected() {
    let (device, queue) = headless_device();
    let compiled = compile(CRIT_SRC, &world(), &kernels()).expect("compile");
    let scene = &compiled.scenes[0];

    let mut inputs = HashMap::new();
    let stats_buf = storage_buffer(&device, "x", (N * 8) as usize);
    let buf = storage_buffer(&device, "x2", N as usize);
    inputs.insert("stats".into(), InputField { buffer: stats_buf, per_entity: 8, ty: FieldTy::F32 });
    inputs.insert("def".into(), InputField { buffer: buf.clone(), per_entity: 1, ty: FieldTy::F32 });
    inputs.insert("frame".into(), InputField { buffer: buf.clone(), per_entity: 1, ty: FieldTy::U32 });

    let mut executor = Executor::new(device, queue);
    executor.register_codelet("damage_formula", Box::new(DamageFormula));
    executor.register_codelet("rng_chance", Box::new(RngChance));
    executor.register_codelet("mul", Box::new(Mul));
    executor.register_codelet("select", Box::new(Select));
    executor.register_codelet("apply_damage", Box::new(ApplyDamage));

    let err = executor.run(scene, &inputs).unwrap_err();
    assert!(matches!(
        err,
        neon_gpu_exec::ExecError::MissingInput { ref alias } if alias == "target_hp"
    ));
}