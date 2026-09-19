//! B0-3/B0-4 fixed-scenario consumer baseline probe.
//!
//! Drives the retained WGPU renderer offscreen with the same case shapes used
//! by `ui_patch_baseline_probe` (neon-ui-runtime) and emits one JSONL record
//! per reconcile with `retained/created/removed/updated/moved` counts plus the
//! refresh_plan stage timing. This is the Phase 0 consumer-side baseline:
//! every record proves what the retained renderer actually rebuilt.

use std::collections::HashMap;
use std::time::Instant;

use neon_protocol::Revision;
use neon_ui_runtime::{apply_nui_ir_patch, parse_nui_flow};
use neon_ui_schema::{
    NuiSourceSpan, UiFragment, UiFragmentId, UiIrPatch, UiIrPatchOperation, UiIrPatchOperationKind,
};
use neon_wgpu_runtime::{
    UiCompositionInvalidation, UiDrawMode, UiDrawStageTimings, UiInstanceReuseBlocker,
    UiPlanRefreshCause, UiWgpuRenderer,
};
use serde_json::{Value, json};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const WIDTH: u32 = 1600;
const HEIGHT: u32 = 12000;
const SPAN: NuiSourceSpan = NuiSourceSpan {
    line: 0,
    column: 0,
    end_line: 0,
    end_column: 0,
};

fn flow_source(node_count: usize, revision: u64) -> String {
    let mut source = format!(
        "version 1\nsurface surface.baseline revision {revision}\nbudget nodes=8192 bindings=8192 instances=8192 text=8192 glyphs=131072 events=64 clips=8192\nflow baseline\nsurface root column w 1200 h 11000 fill #102030\n"
    );
    for row in 0..node_count {
        source.push_str(&format!("  text row-{row:04} value \"row {row}\"\n"));
    }
    source
}

fn set_op(path: &str, revision: u64, property: &str, value: &str) -> UiIrPatchOperation {
    UiIrPatchOperation {
        kind: UiIrPatchOperationKind::Set,
        target_path: path.into(),
        expected_revision: Revision(revision),
        payload: Some(json!({"property": property, "value": value})),
        source_span: SPAN,
    }
}

fn insert_op(revision: u64, key: &str) -> UiIrPatchOperation {
    UiIrPatchOperation {
        kind: UiIrPatchOperationKind::Insert,
        target_path: "root".into(),
        expected_revision: Revision(revision),
        payload: Some(json!({"kind": "text", "key": key})),
        source_span: SPAN,
    }
}

fn remove_op(path: &str, revision: u64) -> UiIrPatchOperation {
    UiIrPatchOperation {
        kind: UiIrPatchOperationKind::Remove,
        target_path: path.into(),
        expected_revision: Revision(revision),
        payload: None,
        source_span: SPAN,
    }
}

fn move_ops(revision: u64) -> Vec<UiIrPatchOperation> {
    (0..10)
        .map(|i| UiIrPatchOperation {
            kind: UiIrPatchOperationKind::Move,
            target_path: format!("row-{i:04}"),
            expected_revision: Revision(revision),
            payload: Some(json!({"parent": "root"})),
            source_span: SPAN,
        })
        .collect()
}

struct Runner {
    renderer: UiWgpuRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    view: wgpu::TextureView,
    draw_sequence: u64,
}

impl Runner {
    fn new() -> Self {
        let (device, queue) = acquire_device();
        let renderer = UiWgpuRenderer::new(&device, FORMAT);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-reconcile-baseline-target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        Self {
            renderer,
            device,
            queue,
            view,
            draw_sequence: 0,
        }
    }

    /// Each case starts from a freshly constructed renderer so the plan-reuse
    /// cache and reconcile counters of one case never leak into the next.
    fn reset(&mut self) {
        self.renderer = UiWgpuRenderer::new(&self.device, FORMAT);
        self.draw_sequence = 0;
    }

    fn draw(&mut self, fragment: &UiFragment) -> (f64, UiDrawStageTimings) {
        self.draw_at(fragment, [1200.0, 11000.0])
    }

    /// One drawn frame, with the logical viewport under test. `viewport` only
    /// changes the layout basis; the physical target stays fixed.
    fn draw_at(
        &mut self,
        fragment: &UiFragment,
        logical_viewport: [f32; 2],
    ) -> (f64, UiDrawStageTimings) {
        self.draw_sequence += 1;
        let fragments = HashMap::from([(fragment.fragment_id.clone(), fragment.clone())]);
        let started = Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ui-reconcile-baseline-pass"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui-reconcile-baseline-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.draw(
                &self.device,
                &self.queue,
                &mut pass,
                &fragments,
                [WIDTH, HEIGHT],
                logical_viewport,
                self.draw_sequence as f32 / 60.0,
                UiDrawMode::Screen,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("probe poll");
        (
            started.elapsed().as_secs_f64() * 1000.0,
            self.renderer.stage_timings(),
        )
    }
}

fn acquire_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        #[cfg(windows)]
        backends: wgpu::Backends::DX12,
        #[cfg(not(windows))]
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("probe adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ui-reconcile-baseline-probe-device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .expect("probe device")
}

fn fragment_from(ir_revision: Revision, root: neon_ui_schema::UiNode) -> UiFragment {
    UiFragment {
        fragment_id: UiFragmentId("surface.baseline".into()),
        revision: ir_revision,
        root,
        effects: Vec::new(),
    }
}

fn emit(record: Value) {
    println!("{record}");
}

/// Full frame self-explanation: what revision it was built from, whether the
/// retained composition and instance vector were reused, which single condition
/// prevented reuse, and how many buffer ranges actually reached the queue.
fn frame_json(timings: &UiDrawStageTimings) -> Value {
    json!({
        "frame_sequence": timings.frame_sequence,
        "fragment_revision": timings.fragment_revision,
        "input_revision": timings.input_revision,
        "composition_reused": timings.composition_reused,
        "plan_refresh": timings.plan_refresh.as_str(),
        "composition_invalidation": timings.composition_invalidation.as_str(),
        "instance_rebuilt": timings.instance_rebuilt,
        "instance_reuse_blocker": timings.instance_reuse_blocker.as_str(),
        "instance_count": timings.instance_count,
        "buffer_ranges_written": timings.buffer_ranges_written(),
        "color_ranges": timings.instance_range_writes,
        "color_records": timings.instance_records_written,
        "color_bytes": timings.instance_bytes_written,
        "depth_ranges": timings.depth_range_writes,
        "depth_records": timings.depth_records_written,
        "depth_bytes": timings.depth_bytes_written,
        "timing_ms": {
            "refresh_plan": timings.refresh_plan_ms,
            "compose_visuals": timings.compose_visuals_ms,
            "text_layout": timings.text_layout_ms,
            "group_sort": timings.group_sort_ms,
            "buffer_upload": timings.buffer_upload_ms,
        },
    })
}

fn fail(message: &str) -> ! {
    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "final": true,
        "status": "failed",
        "error": {"code": "probe_failed", "message": message},
        "pass": false,
    }));
    std::process::exit(1);
}

fn check_step(case: &str, created: u64, removed: u64, moved: u64, updated: u64) -> bool {
    match case {
        "A_property_set_1/set" => created == 0 && removed == 0 && updated >= 1,
        "B_transaction_batch_50/batch50" => created == 0 && removed == 0 && updated >= 50,
        "C_task_lifecycle/insert" => created == 1 && removed == 0,
        "C_task_lifecycle/update" => created == 0 && removed == 0 && updated >= 1,
        "C_task_lifecycle/remove" => created == 0 && removed == 1,
        "D_selection_only_500/set" => created == 0 && removed == 0 && updated >= 1,
        "E_insert_one_100/insert" => created == 1 && removed == 0,
        "E_insert_one_1000/insert" => created == 1 && removed == 0,
        "F_remove_one_100/remove" => created == 0 && removed == 1,
        "G_reorder_batch_100/move10" => created == 0 && removed == 0 && moved >= 1,
        "H_large_batch_1000/batch100" => created == 0 && removed == 0 && updated >= 100,
        _ => false,
    }
}

fn run_case(
    runner: &mut Runner,
    case: &str,
    node_count: usize,
    steps: Vec<(&'static str, Vec<UiIrPatchOperation>)>,
) -> u64 {
    runner.reset();
    let document = match parse_nui_flow(&flow_source(node_count, 3)) {
        Ok(document) => document,
        Err(error) => fail(&format!("{case}/base: parse failed: {error:?}")),
    };
    let mut ir = document.ir.clone();
    let mut revision = ir.revision;
    let mut fragment = fragment_from(revision, ir.root.clone());
    let mut failures = 0_u64;
    let (draw_ms, timings) = runner.draw(&fragment);
    let stats = runner.renderer.reconcile_stats();
    // A fresh renderer must create every planned node (root panel + rows) and
    // report no removals, updates or moves.
    let pass = stats.created == node_count as u64 + 1
        && stats.removed == 0
        && stats.updated == 0
        && stats.moved == 0
        && timings.instance_count > 0;
    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "case": format!("{case}/base"),
        "input": {"node_count": node_count, "operation_count": 0},
        "producer": {"patch_sequence": 0, "base_revision": 3, "ir_revision": revision.0},
        "consumer": {"fragment_revision": fragment.revision.0, "draw_sequence": runner.draw_sequence},
        "draw_ms": draw_ms,
        "frame": frame_json(&timings),
        "retained": stats,
        "pass": pass,
    }));
    if !pass {
        failures += 1;
    }
    // Static repeat: the same fragment and an advanced clock must reuse the
    // retained composition, keep drawing the same instance count, and write
    // nothing to any instance buffer.
    let (_, static_timings) = runner.draw(&fragment);
    let static_pass = static_timings.composition_reused
        && static_timings.composition_invalidation == UiCompositionInvalidation::Reused
        && static_timings.plan_refresh == UiPlanRefreshCause::Reused
        && static_timings.instance_reuse_blocker == UiInstanceReuseBlocker::Reused
        && !static_timings.instance_rebuilt
        && static_timings.instance_count == timings.instance_count
        && static_timings.instance_count > 0
        && static_timings.buffer_ranges_written() == 0;
    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "case": format!("{case}/static-repeat"),
        "input": {"node_count": node_count, "operation_count": 0},
        "consumer": {"fragment_revision": fragment.revision.0, "draw_sequence": runner.draw_sequence},
        "frame": frame_json(&static_timings),
        "pass": static_pass,
    }));
    if !static_pass {
        failures += 1;
    }
    for (step_index, (step, operations)) in steps.into_iter().enumerate() {
        let patch = UiIrPatch {
            expected_revision: revision,
            operations,
        };
        let operation_count = patch.operations.len();
        ir = match apply_nui_ir_patch(&ir, &patch) {
            Ok(patched) => patched,
            Err(error) => fail(&format!("{case}/{step}: patch failed: {error:?}")),
        };
        revision = ir.revision;
        fragment = fragment_from(revision, ir.root.clone());
        let (draw_ms, timings) = runner.draw(&fragment);
        let stats = runner.renderer.reconcile_stats();
        let pass = check_step(
            &format!("{case}/{step}"),
            stats.created,
            stats.removed,
            stats.moved,
            stats.updated,
        ) && timings.instance_count > 0
            && timings.plan_refresh != UiPlanRefreshCause::Reused;
        emit(json!({
            "probe": "ui_reconcile_baseline.v1",
            "case": format!("{case}/{step}"),
            "input": {"node_count": node_count, "operation_count": operation_count},
            "producer": {"patch_sequence": step_index as u64 + 1, "base_revision": revision.0 - 1, "ir_revision": revision.0},
            "consumer": {"fragment_revision": fragment.revision.0, "draw_sequence": runner.draw_sequence},
            "draw_ms": draw_ms,
            "frame": frame_json(&timings),
            "retained": stats,
            "pass": pass,
        }));
        if !pass {
            failures += 1;
        }
        // Settle frame: the very next frame carries no new change, so it must
        // fall back onto the retained path and write nothing. Pairing the
        // changed frame's `frame_sequence` with this one proves the fast path
        // recovers after a real rebuild.
        let (_, settle) = runner.draw(&fragment);
        let settle_pass = settle.composition_reused
            && !settle.instance_rebuilt
            && settle.instance_count == timings.instance_count
            && settle.buffer_ranges_written() == 0
            && settle.frame_sequence > timings.frame_sequence
            && settle.fragment_revision == timings.fragment_revision;
        emit(json!({
            "probe": "ui_reconcile_baseline.v1",
            "case": format!("{case}/{step}/settle"),
            "input": {"node_count": node_count, "operation_count": 0},
            "producer": {"changed_frame_sequence": timings.frame_sequence},
            "consumer": {"fragment_revision": fragment.revision.0, "draw_sequence": runner.draw_sequence},
            "frame": frame_json(&settle),
            "pass": settle_pass,
        }));
        if !settle_pass {
            failures += 1;
        }
    }
    failures
}

/// One invalidation source: proves the three properties the fast path must keep
/// simultaneously. `expect` names the cause the renderer must report, and every
/// scenario re-checks that the frame after it settles back onto the retained
/// path with zero buffer writes.
fn run_invalidation_scenario(
    runner: &mut Runner,
    scenario: &'static str,
    node_count: usize,
    operations: Vec<UiIrPatchOperation>,
    viewport: [f32; 2],
    hover: Option<[f32; 2]>,
    expect_plan: UiPlanRefreshCause,
    expect_invalidation: UiCompositionInvalidation,
    narrow: bool,
) -> u64 {
    runner.reset();
    let document = match parse_nui_flow(&flow_source(node_count, 3)) {
        Ok(document) => document,
        Err(error) => fail(&format!("{scenario}: parse failed: {error:?}")),
    };
    let mut ir = document.ir.clone();
    let mut fragment = fragment_from(ir.revision, ir.root.clone());
    let (_, base) = runner.draw(&fragment);
    let (_, before) = runner.draw(&fragment);
    if !operations.is_empty() {
        let patch = UiIrPatch {
            expected_revision: ir.revision,
            operations,
        };
        match apply_nui_ir_patch(&ir, &patch) {
            Ok(patched) => ir = patched,
            Err(error) => fail(&format!("{scenario}: patch failed: {error:?}")),
        }
        fragment = fragment_from(ir.revision, ir.root.clone());
    }
    if let Some(position) = hover {
        runner.renderer.set_pointer_position(position);
    }
    let (_, changed) = runner.draw_at(&fragment, viewport);
    let narrower = !narrow
        || (changed.instance_count > 1
            && changed.instance_records_written > 0
            && changed.instance_records_written < changed.instance_count as u32);
    let pass = before.composition_reused
        && before.buffer_ranges_written() == 0
        && before.instance_count == base.instance_count
        && changed.plan_refresh == expect_plan
        && changed.composition_invalidation == expect_invalidation
        && !changed.composition_reused
        && changed.instance_count > 0
        && narrower;
    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "case": format!("scenario/{scenario}/invalidate"),
        "input": {"node_count": node_count, "hover": hover, "viewport": viewport},
        "producer": {"ir_revision": ir.revision.0, "fragment_revision": fragment.revision.0},
        "consumer": {
            "static_frame_sequence": before.frame_sequence,
            "changed_frame_sequence": changed.frame_sequence,
        },
        "expected": {"plan_refresh": expect_plan.as_str(),
            "composition_invalidation": expect_invalidation.as_str()},
        "before": frame_json(&before),
        "frame": frame_json(&changed),
        "pass": pass,
    }));
    let mut failures = if pass { 0 } else { 1 };
    // The same frame content must settle back onto the retained path.
    if let Some(position) = hover {
        // Re-asserting the same pointer position must not keep the frame dirty.
        runner.renderer.set_pointer_position(position);
        let (_, held) = runner.draw_at(&fragment, viewport);
        let held_pass = held.composition_invalidation == UiCompositionInvalidation::PointerVisual;
        emit(json!({
            "probe": "ui_reconcile_baseline.v1",
            "case": format!("scenario/{scenario}/hover-held"),
            "input": {"node_count": node_count},
            "frame": frame_json(&held),
            "pass": held_pass,
        }));
        if !held_pass {
            failures += 1;
        }
    }
    let (_, settle) = runner.draw_at(&fragment, viewport);
    let settle_pass = settle.composition_reused
        && !settle.instance_rebuilt
        && settle.instance_count > 0
        && settle.buffer_ranges_written() == 0;
    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "case": format!("scenario/{scenario}/settle"),
        "input": {"node_count": node_count},
        "frame": frame_json(&settle),
        "pass": settle_pass,
    }));
    if !settle_pass {
        failures += 1;
    }
    failures
}

fn main() {
    let mut runner = Runner::new();
    let mut failures = 0_u64;
    let set_rows = |count: usize, property: &str, value: &str, revision: u64| {
        (0..count)
            .map(|i| set_op(&format!("root/row-{i:04}"), revision, property, value))
            .collect::<Vec<_>>()
    };

    failures += run_case(
        &mut runner,
        "A_property_set_1",
        100,
        vec![(
            "set",
            vec![set_op("root/row-0000", 3, "value", "\"updated\"")],
        )],
    );
    failures += run_case(
        &mut runner,
        "B_transaction_batch_50",
        100,
        vec![("batch50", set_rows(50, "opacity", "0.5", 3))],
    );
    failures += run_case(
        &mut runner,
        "C_task_lifecycle",
        100,
        vec![
            ("insert", vec![insert_op(3, "task-extra")]),
            (
                "update",
                vec![set_op("root/task-extra", 4, "value", "\"task\"")],
            ),
            ("remove", vec![remove_op("root/task-extra", 5)]),
        ],
    );
    failures += run_case(
        &mut runner,
        "D_selection_only_500",
        500,
        vec![("set", vec![set_op("root/row-0250", 3, "opacity", "0.9")])],
    );
    failures += run_case(
        &mut runner,
        "E_insert_one_100",
        100,
        vec![("insert", vec![insert_op(3, "row-extra")])],
    );
    failures += run_case(
        &mut runner,
        "E_insert_one_1000",
        1000,
        vec![("insert", vec![insert_op(3, "row-extra")])],
    );
    failures += run_case(
        &mut runner,
        "F_remove_one_100",
        100,
        vec![("remove", vec![remove_op("root/row-0042", 3)])],
    );
    failures += run_case(
        &mut runner,
        "G_reorder_batch_100",
        100,
        vec![("move10", move_ops(3))],
    );
    failures += run_case(
        &mut runner,
        "H_large_batch_1000",
        1000,
        vec![("batch100", set_rows(100, "opacity", "0.25", 3))],
    );

    // Invalidation-source scenarios. Each one proves: the static frame before
    // the change reused everything and wrote nothing, the named source rebuilds
    // and reports itself as the cause, and the frame after it settles back onto
    // the retained path.
    failures += run_invalidation_scenario(
        &mut runner,
        "fragment_revision",
        20,
        vec![set_op("root/row-0019", 3, "opacity", "0.5")],
        [1200.0, 11000.0],
        None,
        UiPlanRefreshCause::FragmentRevision,
        UiCompositionInvalidation::PlanRebuilt,
        true,
    );
    failures += run_invalidation_scenario(
        &mut runner,
        "viewport",
        20,
        Vec::new(),
        [900.0, 8000.0],
        None,
        UiPlanRefreshCause::Viewport,
        UiCompositionInvalidation::PlanRebuilt,
        false,
    );
    failures += run_invalidation_scenario(
        &mut runner,
        "pointer_hover",
        20,
        Vec::new(),
        [1200.0, 11000.0],
        Some([10.0, 10.0]),
        UiPlanRefreshCause::Reused,
        UiCompositionInvalidation::PointerVisual,
        false,
    );

    emit(json!({
        "probe": "ui_reconcile_baseline.v1",
        "final": true,
        "status": if failures == 0 { "passed" } else { "failed" },
        "failures": failures,
        "pass": failures == 0,
    }));
    if failures != 0 {
        std::process::exit(1);
    }
}
