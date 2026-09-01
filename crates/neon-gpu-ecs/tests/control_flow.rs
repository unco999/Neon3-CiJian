//! M3 acceptance tests: If/Jump lowering to the `loop + switch` program
//! counter state machine. Every pattern gets a text snapshot assertion and a
//! headless `create_shader_module` compilation check.

use neon_gpu_ecs::generator::generate_wgsl;
use neon_gpu_ecs::ir::*;
use neon_gpu_ecs::tests_support::physics_world;
use neon_gpu_ecs::EcsError;

/// Base IR with a single Health-only read/write query and no systems yet.
fn control_world() -> EcsIr {
    let mut ir = physics_world();
    // Add a query/system pair focused on Health (F32).
    ir.queries.push(QueryDef {
        id: 1,
        with: vec![ComponentAccess { component_id: 2, access_type: AccessType::ReadWrite }],
        without: vec![],
        filters: vec![],
    });
    ir.systems.clear();
    ir.schedule.stages.clear();
    ir
}

fn add_system(ir: &mut EcsIr, name: &str, locals: u32, instructions: Vec<Instr>) -> u32 {
    let id = ir.systems.len() as u32;
    ir.systems.push(SystemDef {
        id,
        name: name.into(),
        query_id: 1,
        resource_refs: vec![],
        local_var_count: locals,
        instructions,
    });
    ir.schedule.stages.push(Stage {
        id: ir.schedule.stages.len() as u32,
        name: format!("stage_{id}"),
        system_ids: vec![id],
    });
    id
}

fn f32_bytes(v: f32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

fn generate(ir: &EcsIr) -> String {
    match generate_wgsl(ir) {
        Ok(wgsl) => wgsl,
        Err(e) => panic!("generation failed: {e}"),
    }
}

// ---------------------------------------------------------------- if/else ---

#[test]
fn simple_if_else_lowers_to_state_machine() {
    let mut ir = control_world();
    // if (health > 50) { health = 100 } else { health = 0 }
    add_system(&mut ir, "heal", 3, vec![
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(50.0) },
        Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
        Instr::If { cond: 2, true_block: 4, false_block: 7 },
        // then: store 100
        Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(100.0) },
        Instr::Store { src: 0, component_id: 2 },
        Instr::Jump { target: 9 },
        // else: store 0
        Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(0.0) },
        Instr::Store { src: 0, component_id: 2 },
        // implicit final return
        Instr::Return,
    ]);
    let wgsl = generate(&ir);
    assert!(wgsl.contains("var ecs_pc : u32 = 0u;"));
    assert!(wgsl.contains("loop {"));
    assert!(wgsl.contains("switch (ecs_pc) {"));
    assert!(wgsl.contains("if (v2) {"));
    // Both branches set ecs_pc; the fall-through join happens via cases.
    assert!(wgsl.contains("default: {"));
}

// --------------------------------------------------------------- for loop ---

#[test]
fn counter_loop_lowers_to_state_machine() {
    let mut ir = control_world();
    // i = 0; loop { if (i < 3) { health += 1; i += 1; jump back } else return }
    add_system(&mut ir, "tick_three", 5, vec![
        // 0: i = 0
        Instr::Const { dest: 0, ty: ComponentType::U32, bytes: 0u32.to_le_bytes().to_vec() },
        // 1: loop head
        Instr::Const { dest: 1, ty: ComponentType::U32, bytes: 3u32.to_le_bytes().to_vec() },
        Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Less },
        Instr::If { cond: 2, true_block: 4, false_block: 11 },
        // 4: body
        Instr::Load { dest: 3, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 4, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
        Instr::BinaryOp { dest: 3, lhs: 3, rhs: 4, op: BinaryOpCode::Add },
        Instr::Store { src: 3, component_id: 2 },
        Instr::Const { dest: 1, ty: ComponentType::U32, bytes: 1u32.to_le_bytes().to_vec() },
        // 9: i += 1 -> jump back to loop head
        Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
        Instr::Jump { target: 1 },
        // 11: exit
        Instr::Return,
    ]);
    let wgsl = generate(&ir);
    // The back edge lands as `ecs_pc = <case>; break;` inside the loop.
    assert!(wgsl.matches("ecs_pc =").count() >= 3);
    assert!(wgsl.contains("return;"));
}

// ------------------------------------------------------- fall-through block ---

#[test]
fn branch_to_fallthrough_block_falls_into_next_case() {
    let mut ir = control_world();
    // if (health > 0) { } else { health = 1 }; health = health + 1
    add_system(&mut ir, "join_flow", 3, vec![
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(0.0) },
        Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
        Instr::If { cond: 2, true_block: 6, false_block: 4 },
        // 4: else branch
        Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
        Instr::Store { src: 0, component_id: 2 },
        // 6: join block WITHOUT terminator -> falls through to block 7
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
        Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
        Instr::Store { src: 0, component_id: 2 },
        Instr::Return,
    ]);
    let wgsl = generate(&ir);
    // The join block emits `ecs_pc = <next case>u; break;`.
    assert!(wgsl.matches("case ").count() >= 3);
}

// ----------------------------------------------------------- nested if ------

#[test]
fn nested_if_lowers_to_state_machine() {
    let mut ir = control_world();
    // if (health > 50) { if (health > 80) { health = 200 } else { health = 100 } }
    add_system(&mut ir, "nested", 4, vec![
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(50.0) },
        Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
        Instr::If { cond: 2, true_block: 4, false_block: 12 },
        // outer then
        Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(80.0) },
        Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
        Instr::If { cond: 2, true_block: 7, false_block: 10 },
        // inner then: 200
        Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(200.0) },
        Instr::Store { src: 0, component_id: 2 },
        Instr::Jump { target: 12 },
        // inner else: 100
        Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(100.0) },
        Instr::Store { src: 0, component_id: 2 },
        // 12: join -> implicit return (falls off the end)
        Instr::Return,
    ]);
    let wgsl = generate(&ir);
    assert!(wgsl.matches("if (v2) {").count() == 2);
}

// ----------------------------------------------- straight-line unchanged ----

#[test]
fn straight_line_body_keeps_flat_form() {
    let wgsl = generate(&physics_world());
    assert!(!wgsl.contains("var ecs_pc"));
    assert!(wgsl.contains("fn system_physics_update("));
}

// ----------------------------------------------- invalid branch target ------

#[test]
fn branch_target_out_of_range_is_rejected_by_validation() {
    let mut ir = control_world();
    add_system(&mut ir, "bad_target", 2, vec![
        Instr::Const { dest: 0, ty: ComponentType::U32, bytes: 0u32.to_le_bytes().to_vec() },
        Instr::If { cond: 0, true_block: 5, false_block: 1 },
        Instr::Return,
    ]);
    // IR validation rejects out-of-range targets before generation.
    match generate_wgsl(&ir) {
        Err(EcsError::IrInvalid(message)) => {
            assert!(message.contains("out of range"), "{message}");
        }
        other => panic!("expected IrInvalid, got {other:?}"),
    }
}

// ----------------------------------------------- cross-stage no conflict ----

#[test]
fn same_writer_in_different_stages_is_allowed() {
    let mut ir = control_world();
    // Two systems both write Health, but in separate stages -> allowed.
    add_system(&mut ir, "writer_a", 1, vec![
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Store { src: 0, component_id: 2 },
        Instr::Return,
    ]);
    add_system(&mut ir, "writer_b", 1, vec![
        Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
        Instr::Store { src: 0, component_id: 2 },
        Instr::Return,
    ]);
    generate_wgsl(&ir).expect("different stages must not conflict");
}

// ------------------------------------------------- headless compilation -----

fn ecs_test_device() -> (wgpu::Device, wgpu::Queue) {
    let backends = if cfg!(target_os = "windows") {
        wgpu::Backends::VULKAN
    } else {
        wgpu::Backends::PRIMARY
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("M3 WGSL compilation requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M3 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M3 test adapter must create a device and queue")
}

fn compiled_world(build: impl Fn(&mut EcsIr)) {
    let mut ir = control_world();
    build(&mut ir);
    let wgsl = generate(&ir);
    let (device, _queue) = ecs_test_device();
    let _module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("neon-gpu-ecs M3 module"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
}

#[test]
fn if_else_pattern_compiles_on_headless_device() {
    compiled_world(|ir| {
        add_system(ir, "heal", 3, vec![
            Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
            Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(50.0) },
            Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
            Instr::If { cond: 2, true_block: 4, false_block: 7 },
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(100.0) },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Jump { target: 9 },
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(0.0) },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Return,
        ]);
    });
}

#[test]
fn counter_loop_pattern_compiles_on_headless_device() {
    compiled_world(|ir| {
        add_system(ir, "tick_three", 5, vec![
            Instr::Const { dest: 0, ty: ComponentType::U32, bytes: 0u32.to_le_bytes().to_vec() },
            Instr::Const { dest: 1, ty: ComponentType::U32, bytes: 3u32.to_le_bytes().to_vec() },
            Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Less },
            Instr::If { cond: 2, true_block: 4, false_block: 11 },
            Instr::Load { dest: 3, component_id: 2, access: AccessType::ReadWrite },
            Instr::Const { dest: 4, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
            Instr::BinaryOp { dest: 3, lhs: 3, rhs: 4, op: BinaryOpCode::Add },
            Instr::Store { src: 3, component_id: 2 },
            Instr::Const { dest: 1, ty: ComponentType::U32, bytes: 1u32.to_le_bytes().to_vec() },
            Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
            Instr::Jump { target: 1 },
            Instr::Return,
        ]);
    });
}

#[test]
fn fallthrough_pattern_compiles_on_headless_device() {
    compiled_world(|ir| {
        add_system(ir, "join_flow", 3, vec![
            Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
            Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(0.0) },
            Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
            Instr::If { cond: 2, true_block: 6, false_block: 4 },
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
            Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(1.0) },
            Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Return,
        ]);
    });
}

#[test]
fn nested_if_pattern_compiles_on_headless_device() {
    compiled_world(|ir| {
        add_system(ir, "nested", 4, vec![
            Instr::Load { dest: 0, component_id: 2, access: AccessType::ReadWrite },
            Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(50.0) },
            Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
            Instr::If { cond: 2, true_block: 4, false_block: 12 },
            Instr::Const { dest: 1, ty: ComponentType::F32, bytes: f32_bytes(80.0) },
            Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Greater },
            Instr::If { cond: 2, true_block: 7, false_block: 10 },
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(200.0) },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Jump { target: 12 },
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: f32_bytes(100.0) },
            Instr::Store { src: 0, component_id: 2 },
            Instr::Return,
        ]);
    });
}
