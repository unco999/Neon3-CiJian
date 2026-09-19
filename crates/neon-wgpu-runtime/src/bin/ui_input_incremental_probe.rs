//! Headless input-to-GPU incremental update probe.
//!
//! This probe separates three contracts that are easy to conflate:
//! input dirty-slot precision, GPU input-buffer upload precision, and full CPU
//! UI evaluation. It emits JSONL and intentionally reports the last one as a
//! diagnostic limitation rather than hiding it behind a passing upload test.

use neon_protocol::Revision;
use neon_ui_runtime::{
    UiInputStore, UiInputWriter, UiLocalPresentationState, compile_nui_flow_program,
    evaluate_ui_program, parse_nui_flow,
};
use neon_ui_schema::{
    UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiBounds, UiInputChange, UiInputFrame,
    UiInputValue, UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
    UiProgramRevision,
};
use neon_wgpu_runtime::GpuUiProgramBackend;
use serde_json::json;

const FLOW: &str = "version 1
surface surface.input-incremental revision 1
budget nodes=8 bindings=8 instances=8 text=8 glyphs=64 events=8 clips=8
input left bool default false
input right bool default false
surface root row w 200 h 80
  panel left-panel visible $left w 80 h 40
  panel right-panel visible $right w 80 h 40
";

fn revision() -> UiProgramRevision {
    UiProgramRevision {
        program_id: "surface.input-incremental".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: vec![UiProgramCapability {
            name: UI_PROGRAM_CAPABILITY_NAME.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        }],
    }
}

fn device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .expect("headless adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ui-input-incremental-probe"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .expect("headless device")
}

fn main() {
    let result = run();
    println!("{}", result);
    if result["status"] != "passed" {
        std::process::exit(1);
    }
}

fn run() -> serde_json::Value {
    let document = match parse_nui_flow(FLOW) {
        Ok(document) => document,
        Err(error) => {
            return json!({"status":"failed","stage":"parse","error":format!("{error:?}")});
        }
    };
    let program_revision = revision();
    let program = match compile_nui_flow_program(&document, program_revision.clone()) {
        Ok(program) => program,
        Err(error) => {
            return json!({"status":"failed","stage":"compile","error":format!("{error:?}")});
        }
    };
    let mut store = match UiInputStore::activate(program_revision, document.input_schema.clone()) {
        Ok(store) => store,
        Err(error) => return json!({"status":"failed","stage":"input_activate","error":error.code}),
    };
    let (device, queue) = device();
    let mut gpu = GpuUiProgramBackend::new(1);
    let viewport = UiBounds {
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 80.0,
    };
    if let Err(error) = gpu.stage(&device, &queue, &program, &store.snapshot(), viewport) {
        return json!({"status":"failed","stage":"gpu_stage_initial","error":error.code});
    }
    let _ = gpu.activate_at_frame_boundary();
    let initial_cpu = evaluate_ui_program(
        &program,
        &store.snapshot(),
        neon_ui_schema::UiCpuViewport {
            logical_bounds: viewport,
            revision: Revision(1),
        },
        &UiLocalPresentationState::default(),
    );
    let frame = UiInputFrame {
        program_revision: program.revision.clone(),
        expected_input_revision: Revision(0),
        request_id: "ui-input-incremental-probe-request".into(),
        idempotency_key: "ui-input-incremental-probe-left-true".into(),
        changes: vec![UiInputChange {
            key: "left".into(),
            value: UiInputValue::Bool { value: true },
        }],
    };
    let applied = match store.apply(UiInputWriter::External, frame) {
        Ok(applied) => applied,
        Err(error) => return json!({"status":"failed","stage":"input_apply","error":error.code}),
    };
    if let Err(error) = gpu.stage(&device, &queue, &program, &applied.snapshot, viewport) {
        return json!({"status":"failed","stage":"gpu_stage_changed","error":error.code});
    }
    let gpu_frame = gpu.activate_at_frame_boundary();
    let changed_binding_ids = program
        .dependency_index
        .input_to_bindings
        .get("left")
        .cloned()
        .unwrap_or_default();
    let changed_nodes: Vec<String> = changed_binding_ids
        .iter()
        .filter_map(|id| {
            program
                .binding_records
                .iter()
                .find(|binding| binding.binding_id == *id)
        })
        .map(|binding| binding.node_key.clone())
        .collect();
    let changed_cpu = evaluate_ui_program(
        &program,
        &applied.snapshot,
        neon_ui_schema::UiCpuViewport {
            logical_bounds: viewport,
            revision: Revision(1),
        },
        &UiLocalPresentationState::default(),
    );
    let stats = gpu.upload_stats();
    let upload_stats = json!({
        "static_buffer_uploads": stats.static_buffer_uploads,
        "full_input_uploads": stats.full_input_uploads,
        "partial_input_uploads": stats.partial_input_uploads,
        "input_slot_writes": stats.input_slot_writes,
        "skipped_input_uploads": stats.skipped_input_uploads,
    });
    let pass = applied.changed_slots == vec!["left".to_owned()]
        && store.dirty_slots() == vec!["left".to_owned()]
        && changed_nodes == vec!["left-panel".to_owned()]
        && gpu_frame
            .as_ref()
            .is_some_and(|frame| frame.dirty_slots == vec!["left".to_owned()])
        && stats.static_buffer_uploads == 3
        && stats.full_input_uploads == 1
        && stats.partial_input_uploads == 1
        && stats.input_slot_writes == 1
        && initial_cpu.nodes.len() == changed_cpu.nodes.len();
    json!({
        "probe": "ui-input-incremental.v1",
        "status": if pass { "passed" } else { "failed" },
        "input": {"changed_key": "left", "program_revision": program.revision.revision, "input_revision": applied.input_revision.0},
        "producer": {"changed_slots": applied.changed_slots, "dirty_slots": store.dirty_slots()},
        "dependency": {"binding_ids": changed_binding_ids, "node_keys": changed_nodes},
        "consumer": {"gpu_dirty_slots": gpu_frame.map(|frame| frame.dirty_slots), "upload_stats": upload_stats},
        "cpu_evaluation": {"initial_node_count": initial_cpu.nodes.len(), "changed_node_count": changed_cpu.nodes.len(), "scope": "full_program", "end_to_end_incremental": false},
        "warnings": ["CPU evaluate_ui_program still materializes all nodes and traverses all bindings; only input upload and GPU dirty metadata are sparse."],
        "pass": pass,
    })
}
