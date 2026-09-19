//! Headless input-to-GPU incremental update probe.
//!
//! This probe separates four contracts that are easy to conflate: input dirty
//! slot precision, CPU retained evaluation scope, GPU upload scope, and renderer
//! composition. It only reports `end_to_end_incremental` when the CPU delta and
//! the GPU delta were both narrower than a full pass *and* the retained frame
//! still equals the golden full evaluation.

use neon_protocol::Revision;
use neon_ui_runtime::ui_retained_evaluator::{
    UiImpactSet, apply_ui_impact_set, evaluate_ui_program_initial,
};
use neon_ui_runtime::{
    UiInputStore, UiInputWriter, UiLocalPresentationState, compile_nui_flow_program,
    evaluate_ui_program, parse_nui_flow,
};
use neon_ui_schema::{
    UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_DELTA_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
    UiBounds, UiCpuViewport, UiInputChange, UiInputFrame, UiInputValue, UiProgramCapability,
    UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision,
};
use neon_wgpu_runtime::GpuUiProgramBackend;
use serde_json::json;

const FLOW: &str = "version 1
surface surface.input-incremental revision 1
budget nodes=16 bindings=16 instances=16 text=16 glyphs=64 events=16 clips=16
input left bool default false
input right bool default false
surface root row w 200 h 80
  panel left-panel visible $left w 80 h 40
  panel right-panel visible $right w 80 h 40
  panel filler-a w 20 h 20
  panel filler-b w 20 h 20
  panel filler-c w 20 h 20
  panel filler-d w 20 h 20
";

fn revision(delta_capable: bool) -> UiProgramRevision {
    let mut capabilities = vec![UiProgramCapability {
        name: UI_PROGRAM_CAPABILITY_NAME.into(),
        version: 1,
        owner: UiProgramCapabilityOwner::SharedContract,
        status: UiProgramCapabilityStatus::Supported,
    }];
    if delta_capable {
        capabilities.push(UiProgramCapability {
            name: UI_PROGRAM_DELTA_CAPABILITY_NAME.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::WgpuRuntime,
            status: UiProgramCapabilityStatus::Supported,
        });
    }
    UiProgramRevision {
        program_id: "surface.input-incremental".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities,
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
    let program_revision = revision(true);
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
    let viewport = UiBounds {
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 80.0,
    };
    let cpu_viewport = UiCpuViewport {
        logical_bounds: viewport,
        revision: Revision(1),
    };
    let local = UiLocalPresentationState::default();
    let mut gpu = GpuUiProgramBackend::new(1);
    let delta_negotiated = match gpu.stage(&device, &queue, &program, &store.snapshot(), viewport) {
        Ok(negotiated) => negotiated,
        Err(error) => {
            return json!({"status":"failed","stage":"gpu_stage_initial","error":error.code});
        }
    };
    let _ = gpu.activate_at_frame_boundary();
    let mut retained =
        evaluate_ui_program_initial(&program, &store.snapshot(), cpu_viewport, &local);
    let golden_initial = evaluate_ui_program(&program, &store.snapshot(), cpu_viewport, &local);
    if retained.frame() != golden_initial {
        return json!({"status":"failed","stage":"initial_equality"});
    }
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
    let impact = UiImpactSet::from_input_publication(
        &program,
        &applied.changed_slots,
        applied.input_revision,
        Revision(1),
    );
    let delta =
        match apply_ui_impact_set(&program, &mut retained, &applied.snapshot, &local, &impact) {
            Ok(delta) => delta,
            Err(error) => {
                return json!({"status":"failed","stage":"cpu_delta","error":error.code});
            }
        };
    let upload = match gpu.apply_frame_delta(&queue, &program, &applied.snapshot, &impact, &delta) {
        Ok(upload) => upload,
        Err(error) => {
            return json!(
                {"status":"failed","stage":"gpu_delta","error":error.code,
                 "detail":error.node_key}
            );
        }
    };
    let _ = gpu.activate_at_frame_boundary();
    let golden_changed = evaluate_ui_program(&program, &applied.snapshot, cpu_viewport, &local);
    let frames_equal = retained.frame() == golden_changed;
    let changed_binding_ids = program
        .dependency_index
        .input_to_bindings
        .get("left")
        .cloned()
        .unwrap_or_default();
    let stats = gpu.upload_stats();
    let cpu_incremental = delta.executed_binding_ids == changed_binding_ids
        && delta.executed_binding_ids.len() < program.binding_records.len()
        && delta.changed_states.iter().any(|state| state.visible)
        && delta.input_revision == applied.input_revision;
    let gpu_incremental = upload.bytes_written < upload.bytes_for_full_upload
        && upload.node_keys == vec!["left-panel".to_owned()]
        && upload.instance_records_written == 1
        && upload.input_slot_writes == 1;
    // A program revision that never negotiated the capability must not be able to
    // take the delta path, even with a well-formed impact set and delta.
    let (uncapable, uncapable_code) = uncapped_delta_rejected(&device, &queue, &document);
    let end_to_end_incremental =
        delta_negotiated && cpu_incremental && gpu_incremental && frames_equal;
    let pass = end_to_end_incremental
        && uncapable
        && applied.changed_slots == vec!["left".to_owned()]
        && store.dirty_slots() == vec!["left".to_owned()]
        && delta.changed_states.len() == 1
        && stats.static_buffer_uploads == 4
        && stats.full_input_uploads == 1
        && stats.partial_input_uploads == 0
        && stats.delta_uploads == 1;
    json!({
        "probe": "ui-input-incremental.v2",
        "status": if pass { "passed" } else { "failed" },
        "input": {"changed_key": "left", "program_revision": program.revision.revision, "input_revision": applied.input_revision.0},
        "producer": {"changed_slots": applied.changed_slots, "dirty_slots": store.dirty_slots()},
        "dependency": {"binding_ids": changed_binding_ids, "node_keys": impact.node_keys, "domains": impact.domains},
        "cpu_delta": {"scope": "impacted_bindings_only", "executed_binding_ids": delta.executed_binding_ids, "total_bindings": program.binding_records.len(), "changed_states": delta.changed_states, "primitives_rebuilt": delta.render_primitives_rebuilt, "layout_unchanged": delta.layout_unchanged, "narrower_than_full": cpu_incremental},
        "consumer": {"gpu_delta": upload, "upload_stats": stats, "capability_negotiated": delta_negotiated, "narrower_than_full": gpu_incremental},
        "equality_oracle": {"retained_frame_equals_golden_full_evaluation": frames_equal},
        "capability_gate": {"delta_without_capability_rejected": uncapable, "observed_code": uncapable_code},
        "end_to_end_incremental": end_to_end_incremental,
        "warnings": ["The GPU ranges written here are the program adapter's instance, input and dirty planes. The production composition path in UiWgpuRenderer still rebuilds its own instance list per frame; this probe does not claim that pass is incremental."],
        "pass": pass,
    })
}

/// Publishes an input against a revision that never negotiated the delta
/// capability and confirms the renderer refuses to narrow it.
fn uncapped_delta_rejected(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    document: &neon_ui_schema::NuiFlowDocument,
) -> (bool, String) {
    let program_revision = revision(false);
    let Ok(program) = compile_nui_flow_program(document, program_revision.clone()) else {
        return (false, "compile".to_owned());
    };
    let Ok(mut store) = UiInputStore::activate(program_revision, document.input_schema.clone())
    else {
        return (false, "input_activate".to_owned());
    };
    let viewport = UiBounds {
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 80.0,
    };
    let local = UiLocalPresentationState::default();
    let cpu_viewport = UiCpuViewport {
        logical_bounds: viewport,
        revision: Revision(1),
    };
    let base = store.snapshot();
    let mut gpu = GpuUiProgramBackend::new(2);
    if gpu.stage(device, queue, &program, &base, viewport).is_err() {
        return (false, "stage".to_owned());
    }
    let applied = match store.apply(
        UiInputWriter::External,
        UiInputFrame {
            program_revision: program.revision.clone(),
            expected_input_revision: base.input_revision,
            request_id: "ui-input-incremental-probe-uncapped".into(),
            idempotency_key: "ui-input-incremental-probe-uncapped-left".into(),
            changes: vec![UiInputChange {
                key: "left".into(),
                value: UiInputValue::Bool { value: true },
            }],
        },
    ) {
        Ok(applied) => applied,
        Err(error) => return (false, format!("input_apply:{}", error.code)),
    };
    let impact = UiImpactSet::from_input_publication(
        &program,
        &applied.changed_slots,
        applied.input_revision,
        Revision(1),
    );
    let mut retained = evaluate_ui_program_initial(&program, &base, cpu_viewport, &local);
    let Ok(delta) =
        apply_ui_impact_set(&program, &mut retained, &applied.snapshot, &local, &impact)
    else {
        return (false, "cpu_delta".to_owned());
    };
    match gpu.apply_frame_delta(queue, &program, &applied.snapshot, &impact, &delta) {
        Err(error) => (
            error.code == "ui_program_delta_capability_missing",
            error.code.to_owned(),
        ),
        Ok(_) => (false, "accepted".to_owned()),
    }
}
