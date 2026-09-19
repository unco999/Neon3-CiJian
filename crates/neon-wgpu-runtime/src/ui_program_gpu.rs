//! Renderer-private adapter for the static UI program contract.
//!
//! This module is intentionally the only place that turns a `UiProgram` into
//! WGPU buffers.  Its sampled layout output is diagnostic data, never an input
//! or a replacement for the UI runtime's CPU execution backend.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use neon_protocol::Revision;
use neon_ui_runtime::ui_retained_evaluator::{UiChangeCause, UiFrameDelta, UiImpactSet};
use neon_ui_schema::{
    UiBoundProperty, UiBounds, UiBranchPredicate, UiDiagnostic, UiDiagnosticSeverity,
    UiGpuBackendAdapter, UiGpuFrameState, UiGpuLayoutNode, UiGpuLayoutReadback, UiGpuPassTiming,
    UiGpuUploadStatus, UiInputValue, UiInvalidationDomain, UiProgram, UiProgramRevision,
    UiResolvedInputs, UiResourceBudget,
};

/// Every record in the program buffers is one 16-byte slot.
const RECORD_STRIDE: u64 = 16;
/// The dirty buffer packs `u32` words: word 0 is the dirty slot count.
const DIRTY_WORD: u64 = 4;

#[derive(Debug)]
pub struct UiGpuProgramBuffers {
    program_revision: UiProgramRevision,
    node_buffer: wgpu::Buffer,
    binding_buffer: wgpu::Buffer,
    input_buffer: wgpu::Buffer,
    dirty_buffer: wgpu::Buffer,
    branch_buffer: wgpu::Buffer,
    layout_buffer: wgpu::Buffer,
    clip_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    diagnostic_buffer: wgpu::Buffer,
    capacity: UiResourceBudget,
}

/// Process-local map from a stable program node key to the byte ranges that the
/// node owns in this revision's GPU buffers.
///
/// The offsets are private to this process and valid only for the buffers
/// created from the same program revision. They never enter the IR, the
/// cross-process protocol, a hit id or a project file: the impact graph hands
/// the renderer node *keys*, and this index is the renderer's own answer to
/// "which bytes are those keys?".
#[derive(Default)]
struct GpuNodeRanges {
    /// node key -> (first offset, record count) in `instance_buffer`.
    instances: BTreeMap<String, (u64, u32)>,
}

impl GpuNodeRanges {
    fn is_covered(&self, node_key: &str) -> bool {
        self.instances.contains_key(node_key)
    }
}

/// Slot layout of the input buffer, derived once per program revision.
///
/// Rebuilding it scans every resolved input, so the partial-upload path keeps
/// it around and only re-derives it when a touched key's expansion no longer
/// matches its cached width.
struct InputSlotLayout {
    /// top-level input key -> starting slot index, in `BTreeMap` order.
    starts: BTreeMap<String, usize>,
    /// top-level input key -> number of 16-byte slots the value expands into.
    widths: BTreeMap<String, usize>,
}

impl InputSlotLayout {
    fn build(inputs: &UiResolvedInputs, budget: &UiResourceBudget) -> Self {
        let mut starts = BTreeMap::new();
        let mut widths = BTreeMap::new();
        let mut cursor = 0usize;
        for (key, value) in inputs.values.iter() {
            if cursor >= budget.max_bindings as usize {
                break;
            }
            let width = flatten_value(&value.value).len();
            starts.insert(key.clone(), cursor);
            widths.insert(key.clone(), width);
            cursor += width;
        }
        Self { starts, widths }
    }

    /// Detects a layout the cache no longer describes by checking only the keys
    /// this frame touches, so the common delta stays proportional to the change.
    fn is_stale_for(&self, inputs: &UiResolvedInputs, keys: &[String]) -> bool {
        keys.iter().any(|key| {
            let top = key.split('.').next().unwrap_or(key.as_str());
            match (self.widths.get(top), inputs.values.get(top)) {
                (Some(width), Some(value)) => *width != flatten_value(&value.value).len(),
                (None, Some(_)) => true,
                _ => false,
            }
        })
    }
}

/// Upload counters used by headless acceptance probes. These counters describe
/// producer-to-GPU upload scope; they do not claim that CPU layout evaluation
/// or renderer composition is incremental.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiGpuUploadStats {
    pub static_buffer_uploads: u64,
    pub full_input_uploads: u64,
    pub partial_input_uploads: u64,
    pub input_slot_writes: u64,
    pub skipped_input_uploads: u64,
    /// Deltas applied through `apply_frame_delta` instead of a full re-stage.
    pub delta_uploads: u64,
    /// Per-node instance records rewritten by delta uploads.
    pub delta_instance_records: u64,
    pub delta_skipped_no_capability: u64,
    pub bytes_written: u64,
    pub bytes_written_by_full_uploads: u64,
}

#[derive(Clone)]
struct StagedProgram {
    program: Rc<UiProgram>,
    inputs: UiResolvedInputs,
    viewport: UiBounds,
    dirty_slots: Vec<String>,
}

/// Everything this adapter derived from one program revision.
struct ProgramArtifacts {
    program: Rc<UiProgram>,
    buffers: UiGpuProgramBuffers,
    ranges: GpuNodeRanges,
    input_layout: InputSlotLayout,
    /// Bytes a full re-stage of this revision writes; the delta denominator.
    full_upload_bytes: u64,
    /// Words of `dirty_buffer` that still hold a previous frame's slot list.
    dirty_words: u32,
}

impl ProgramArtifacts {
    fn new(device: &wgpu::Device, program: &UiProgram, inputs: &UiResolvedInputs) -> Self {
        Self {
            buffers: create_buffers(device, program),
            ranges: plan_node_ranges(program),
            program: Rc::new(program.clone()),
            input_layout: InputSlotLayout::build(inputs, &program.resource_budget),
            full_upload_bytes: full_upload_bytes(program, inputs),
            dirty_words: 0,
        }
    }
}

/// Counters accumulated while a full stage runs, merged into the shared stats
/// after the buffer borrow ends.
#[derive(Default)]
struct StageWrites {
    static_buffer_uploads: u64,
    full_input_uploads: u64,
    partial_input_uploads: u64,
    skipped_input_uploads: u64,
    input_slot_writes: u64,
    bytes_written: u64,
    bytes_written_by_full_uploads: u64,
    dirty_words_written: u32,
}

/// Byte scope of one delta upload, measured against a full re-upload of the
/// same buffers. Probes use it to prove the write touched a fraction of the
/// program; it says nothing about renderer composition.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiGpuDeltaUpload {
    pub program_revision: u64,
    pub input_revision: u64,
    pub node_keys: Vec<String>,
    pub input_slot_writes: u32,
    pub instance_records_written: u32,
    pub dirty_words_written: u32,
    pub bytes_written: u64,
    pub bytes_for_full_upload: u64,
}

/// WGPU-owner adapter. A staged update is only made observable by
/// `activate_at_frame_boundary`, which prevents a partially uploaded program
/// or input revision from being rendered.
pub struct GpuUiProgramBackend {
    renderer_epoch: u64,
    artifacts: Option<ProgramArtifacts>,
    staged: Option<StagedProgram>,
    active: Option<StagedProgram>,
    frame_sequence: u64,
    diagnostics: Vec<UiDiagnostic>,
    last_timing: UiGpuPassTiming,
    last_readback: Option<UiGpuLayoutReadback>,
    upload_stats: UiGpuUploadStats,
}

impl GpuUiProgramBackend {
    pub fn new(renderer_epoch: u64) -> Self {
        Self {
            renderer_epoch,
            artifacts: None,
            staged: None,
            active: None,
            frame_sequence: 0,
            diagnostics: Vec::new(),
            last_timing: zero_timing(),
            last_readback: None,
            upload_stats: UiGpuUploadStats::default(),
        }
    }

    /// Full re-upload of a program revision and its resolved inputs.
    ///
    /// Returns whether this revision negotiated
    /// `ui.program.delta.v1`. When it did not, the caller must keep coming back
    /// here for every change, because the renderer may not claim a node-key to
    /// range index that was never agreed.
    pub fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        program: &UiProgram,
        inputs: &UiResolvedInputs,
        viewport: UiBounds,
    ) -> Result<bool, UiDiagnostic> {
        let started = Instant::now();
        if program.revision != inputs.program_revision {
            return Err(diagnostic(
                "ui_program_stale_input_revision",
                "input revision belongs to a different program",
                None,
                None,
                program.revision.revision,
            ));
        }
        let recreate = self.artifacts.as_ref().is_none_or(|artifacts| {
            artifacts.buffers.program_revision != program.revision
                || artifacts.buffers.capacity != program.resource_budget
        });
        if recreate {
            if !fits_budget(program) {
                let error = diagnostic(
                    "ui_program_capacity_overflow",
                    "program records exceed their declared resource budget",
                    None,
                    None,
                    program.revision.revision,
                );
                self.diagnostics.push(error.clone());
                return Err(error);
            }
            self.artifacts = Some(ProgramArtifacts::new(device, program, inputs));
        }
        let budget = program.resource_budget.clone();
        let input_started = Instant::now();
        let mut writes = StageWrites::default();
        let staged_program = {
            let artifacts = self.artifacts.as_mut().expect("created above");
            if recreate
                || artifacts
                    .input_layout
                    .is_stale_for(inputs, &inputs.changed_slots)
            {
                artifacts.input_layout = InputSlotLayout::build(inputs, &budget);
            }
            let buffers = &artifacts.buffers;
            if recreate {
                queue.write_buffer(
                    &buffers.node_buffer,
                    0,
                    &record_bytes(program.nodes.len(), RECORD_STRIDE as usize),
                );
                queue.write_buffer(
                    &buffers.binding_buffer,
                    0,
                    &record_bytes(program.binding_records.len(), RECORD_STRIDE as usize),
                );
                queue.write_buffer(
                    &buffers.branch_buffer,
                    0,
                    &record_bytes(program.branch_records.len(), DIRTY_WORD as usize),
                );
                writes.static_buffer_uploads += 3;
            }
            // Input buffer: full upload on first stage or program change,
            // slot-exact writes when only specific slots moved.
            if recreate {
                let bytes = pack_inputs(inputs, &budget);
                writes.bytes_written_by_full_uploads += bytes.len() as u64;
                queue.write_buffer(&buffers.input_buffer, 0, &bytes);
                writes.full_input_uploads += 1;
            } else if !inputs.changed_slots.is_empty() {
                let updates = pack_slots_for(
                    &inputs.changed_slots,
                    inputs,
                    &budget,
                    &artifacts.input_layout,
                );
                for (offset, slot_bytes) in updates {
                    queue.write_buffer(&buffers.input_buffer, offset, &slot_bytes);
                    writes.input_slot_writes += 1;
                    writes.bytes_written += slot_bytes.len() as u64;
                }
                writes.partial_input_uploads += 1;
            } else {
                writes.skipped_input_uploads += 1;
            }
            // The logical layout plane carries the program's resolved bounds and
            // is uploaded once per revision: an input publication can change a
            // node's presentation state, never its logical layout.
            if recreate {
                let bytes = pack_layout_records(program, &budget);
                writes.bytes_written_by_full_uploads += bytes.len() as u64;
                queue.write_buffer(&buffers.layout_buffer, 0, &bytes);
                writes.static_buffer_uploads += 1;
            }
            writes.dirty_words_written = write_dirty_slots(
                queue,
                buffers,
                &artifacts.input_layout,
                inputs,
                artifacts.dirty_words,
            );
            artifacts.dirty_words = writes.dirty_words_written.max(1);
            Rc::clone(&artifacts.program)
        };
        self.upload_stats.static_buffer_uploads += writes.static_buffer_uploads;
        self.upload_stats.full_input_uploads += writes.full_input_uploads;
        self.upload_stats.partial_input_uploads += writes.partial_input_uploads;
        self.upload_stats.skipped_input_uploads += writes.skipped_input_uploads;
        self.upload_stats.input_slot_writes += writes.input_slot_writes;
        self.upload_stats.bytes_written += writes.bytes_written;
        self.upload_stats.bytes_written_by_full_uploads += writes.bytes_written_by_full_uploads;
        self.last_timing.program_upload_us = started.elapsed().as_micros() as u64;
        self.last_timing.input_upload_us = input_started.elapsed().as_micros() as u64;
        self.staged = Some(StagedProgram {
            program: staged_program,
            inputs: inputs.clone(),
            viewport,
            dirty_slots: inputs.changed_slots.clone(),
        });
        Ok(program.revision.supports_delta_upload())
    }

    /// Narrow a resolved input publication to the buffer ranges the compile-time
    /// impact graph says it can reach.
    ///
    /// This is the delta half of the `ui.program.delta.v1` contract: the impact
    /// set carries stable node *keys*, this process resolves them to byte ranges,
    /// and only those ranges are written. Any mismatch in cause, revision,
    /// capability or coverage is reported as a diagnostic so the caller falls
    /// back to [`Self::stage`]; a delta is never silently applied to the wrong
    /// range.
    pub fn apply_frame_delta(
        &mut self,
        queue: &wgpu::Queue,
        program: &UiProgram,
        inputs: &UiResolvedInputs,
        impact: &UiImpactSet,
        delta: &UiFrameDelta,
    ) -> Result<UiGpuDeltaUpload, UiDiagnostic> {
        let revision = program.revision.revision;
        if !program.revision.supports_delta_upload() {
            self.upload_stats.delta_skipped_no_capability += 1;
            return Err(diagnostic(
                "ui_program_delta_capability_missing",
                "program revision did not negotiate ui.program.delta.v1",
                None,
                None,
                revision,
            ));
        }
        if program.revision != inputs.program_revision {
            return Err(diagnostic(
                "ui_program_stale_input_revision",
                "input revision belongs to a different program",
                None,
                None,
                revision,
            ));
        }
        if delta.cause != UiChangeCause::InputPublication {
            return Err(diagnostic(
                "ui_program_delta_unsupported_cause",
                "only an authoritative input publication may drive a program delta; \
                 interaction previews stay renderer-local",
                None,
                None,
                revision,
            ));
        }
        if delta.input_revision != inputs.input_revision {
            return Err(diagnostic(
                "ui_program_delta_stale",
                "frame delta was computed for a different input revision",
                None,
                None,
                revision,
            ));
        }
        let base = self
            .staged
            .as_ref()
            .or(self.active.as_ref())
            .ok_or_else(|| {
                diagnostic(
                    "ui_program_delta_without_base",
                    "no program revision is uploaded; a delta needs a full stage first",
                    None,
                    None,
                    revision,
                )
            })?;
        if base.program.revision != program.revision {
            return Err(diagnostic(
                "ui_program_delta_without_base",
                "uploaded program revision differs from the delta's program",
                None,
                None,
                revision,
            ));
        }
        if !delta.layout_unchanged {
            return Err(diagnostic(
                "ui_program_delta_needs_full_stage",
                "the change reaches logical layout, which this revision uploads in full",
                None,
                None,
                revision,
            ));
        }
        let base_viewport = base.viewport;
        let uncovered = {
            let artifacts = self.artifacts.as_ref().expect("base implies artifacts");
            let mut keys: Vec<String> = delta
                .changed_states
                .iter()
                .filter(|state| !artifacts.ranges.is_covered(&state.node_key))
                .map(|state| state.node_key.clone())
                .collect();
            keys.sort();
            keys.dedup();
            keys
        };
        if !uncovered.is_empty() {
            return Err(diagnostic(
                "ui_program_delta_uncovered_node",
                "delta reached a node with no range in this revision's buffers",
                Some(uncovered.join(",")),
                None,
                revision,
            ));
        }

        let touches_state = delta.domains_executed.iter().any(|domain| {
            matches!(
                domain,
                UiInvalidationDomain::NodeState
                    | UiInvalidationDomain::ColorInstances
                    | UiInvalidationDomain::DepthInstances
                    | UiInvalidationDomain::HitTarget
            )
        });
        let mut upload = UiGpuDeltaUpload {
            program_revision: revision.0,
            input_revision: delta.input_revision.0,
            node_keys: delta
                .changed_states
                .iter()
                .map(|state| state.node_key.clone())
                .collect(),
            ..UiGpuDeltaUpload::default()
        };
        upload.node_keys.sort();
        upload.node_keys.dedup();
        let program_rc = {
            let artifacts = self.artifacts.as_mut().expect("base implies artifacts");
            let budget = program.resource_budget.clone();
            let program_keys = union_input_keys(
                impact.input_keys.as_slice(),
                inputs.changed_slots.as_slice(),
            );
            upload.bytes_for_full_upload = artifacts.full_upload_bytes;
            if artifacts.input_layout.is_stale_for(inputs, &program_keys) {
                artifacts.input_layout = InputSlotLayout::build(inputs, &budget);
            }
            for (offset, slot_bytes) in
                pack_slots_for(&program_keys, inputs, &budget, &artifacts.input_layout)
            {
                queue.write_buffer(&artifacts.buffers.input_buffer, offset, &slot_bytes);
                upload.input_slot_writes += 1;
                upload.bytes_written += slot_bytes.len() as u64;
            }
            if touches_state {
                for state in &delta.changed_states {
                    let Some((offset, count)) =
                        artifacts.ranges.instances.get(&state.node_key).copied()
                    else {
                        continue;
                    };
                    let record = state_record_bytes(state);
                    let capacity = artifacts.buffers.instance_buffer.size() as usize;
                    if offset as usize + record.len() > capacity {
                        continue;
                    }
                    queue.write_buffer(&artifacts.buffers.instance_buffer, offset, &record);
                    upload.instance_records_written += count;
                    upload.bytes_written += record.len() as u64;
                }
            }
            let words = write_dirty_slots(
                queue,
                &artifacts.buffers,
                &artifacts.input_layout,
                inputs,
                artifacts.dirty_words,
            );
            upload.dirty_words_written = words;
            upload.bytes_written += u64::from(words) * DIRTY_WORD;
            artifacts.dirty_words = words.max(1);
            Rc::clone(&artifacts.program)
        };
        self.upload_stats.delta_uploads += 1;
        self.upload_stats.delta_instance_records += upload.instance_records_written as u64;
        self.upload_stats.input_slot_writes += upload.input_slot_writes as u64;
        self.upload_stats.bytes_written += upload.bytes_written;
        self.staged = Some(StagedProgram {
            program: program_rc,
            inputs: inputs.clone(),
            viewport: base_viewport,
            dirty_slots: inputs.changed_slots.clone(),
        });
        Ok(upload)
    }

    pub fn activate_at_frame_boundary(&mut self) -> Option<UiGpuFrameState> {
        let staged = self.staged.take()?;
        self.active = Some(staged);
        self.frame_sequence += 1;
        let active = self.active.as_ref().expect("set above");
        Some(UiGpuFrameState {
            renderer_epoch: self.renderer_epoch,
            program_revision: active.program.revision.clone(),
            input_revision: active.inputs.input_revision,
            dirty_slots: active.dirty_slots.clone(),
            frame_sequence: self.frame_sequence,
        })
    }

    /// Generates a versioned, explicitly asynchronous diagnostic sample. The
    /// record format mirrors the currently supported static/flex compatibility
    /// subset until compute layout dispatch is enabled for a later capability.
    pub fn sample_layout_readback(&mut self) -> Option<UiGpuLayoutReadback> {
        let active = self.active.as_ref()?;
        let started = Instant::now();
        let mut visibility = std::collections::BTreeMap::new();
        for node in &active.program.node_templates {
            visibility.insert(node.node_id.0.clone(), node.visible);
        }
        let binding_started = Instant::now();
        for binding in &active.program.binding_records {
            if binding.property == UiBoundProperty::Visible {
                if let Some(value) = active
                    .inputs
                    .values
                    .get(&binding.input_key)
                    .map(|value| &value.value)
                {
                    if let UiInputValue::Bool { value } = value {
                        visibility.insert(binding.node_key.clone(), *value);
                    }
                }
            }
        }
        for branch in &active.program.branch_records {
            let active_branch = match &branch.predicate {
                UiBranchPredicate::Bool {
                    input_key,
                    expected,
                } => {
                    matches!(active.inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Bool { value }) if value == expected)
                }
                UiBranchPredicate::EnumEquals { input_key, variant } => {
                    matches!(active.inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Enum { value }) if value == variant)
                }
                // Local NUI statechart state is resolved in the UI runtime before
                // a program reaches the renderer; GPU inputs cannot own it.
                UiBranchPredicate::MachineState { .. } => false,
            };
            if !active_branch {
                for node_key in &branch.node_range {
                    visibility.insert(node_key.clone(), false);
                }
            }
        }
        self.last_timing.binding_us = binding_started.elapsed().as_micros() as u64;
        let layout_started = Instant::now();
        let mut clips = std::collections::BTreeMap::new();
        let root = active.program.nodes.first().map(|node| node.key.as_str());
        let nodes = active
            .program
            .layout_records
            .iter()
            .map(|record| {
                let mut bounds = record.bounds;
                if Some(record.node_key.as_str()) == root {
                    bounds.width = bounds.width.min(active.viewport.width);
                    bounds.height = bounds.height.min(active.viewport.height);
                }
                let clip = record
                    .layout
                    .filter(|layout| layout.clip != neon_ui_schema::UiClipPolicy::None)
                    .map(|_| bounds);
                if let Some(clip) = clip {
                    clips.insert(record.node_key.clone(), clip);
                }
                UiGpuLayoutNode {
                    node_key: record.node_key.clone(),
                    bounds,
                    clip,
                    visible: visibility.get(&record.node_key).copied().unwrap_or(false),
                }
            })
            .collect();
        self.last_timing.layout_us = layout_started.elapsed().as_micros() as u64;
        self.last_timing.readback_us = started.elapsed().as_micros() as u64;
        let sample = UiGpuLayoutReadback {
            renderer_epoch: self.renderer_epoch,
            program_revision: active.program.revision.clone(),
            input_revision: active.inputs.input_revision,
            nodes,
            diagnostics: self.diagnostics.clone(),
            sampled_frame: self.frame_sequence,
            asynchronous: true,
        };
        self.last_readback = Some(sample.clone());
        Some(sample)
    }

    pub fn summary(&self) -> UiGpuBackendAdapter {
        let active = self.active.as_ref();
        UiGpuBackendAdapter {
            renderer_epoch: self.renderer_epoch,
            program_revision: active.map(|state| state.program.revision.clone()),
            input_revision: active.map(|state| state.inputs.input_revision),
            upload_status: if active.is_some() {
                UiGpuUploadStatus::Active
            } else if self.staged.is_some() {
                UiGpuUploadStatus::Staged
            } else {
                UiGpuUploadStatus::Empty
            },
            capacity: self
                .artifacts
                .as_ref()
                .map(|artifacts| artifacts.buffers.capacity.clone())
                .unwrap_or_else(empty_budget),
            diagnostics: self.diagnostics.clone(),
            last_timing: self.last_timing.clone(),
        }
    }

    pub fn last_readback(&self) -> Option<&UiGpuLayoutReadback> {
        self.last_readback.as_ref()
    }

    pub fn upload_stats(&self) -> &UiGpuUploadStats {
        &self.upload_stats
    }

    /// Differential diagnostic for the subset currently represented by the
    /// renderer adapter. Callers provide the CPU frame produced by the UI
    /// runtime; neither crate depends on the other.
    pub fn compare_cpu_frame(
        &self,
        cpu: &neon_ui_schema::UiCpuFrameOutput,
        tolerance: f32,
    ) -> Vec<UiDiagnostic> {
        let Some(gpu) = self.last_readback() else {
            return vec![diagnostic(
                "ui_gpu_readback_unavailable",
                "no GPU layout sample is available",
                None,
                None,
                cpu.program_revision.revision,
            )];
        };
        let mut differences = Vec::new();
        for cpu_layout in &cpu.logical_layout {
            let Some(gpu_node) = gpu
                .nodes
                .iter()
                .find(|node| node.node_key == cpu_layout.node_key)
            else {
                differences.push(diagnostic(
                    "ui_gpu_cpu_node_missing",
                    "GPU sample is missing a CPU layout node",
                    Some(cpu_layout.node_key.clone()),
                    None,
                    cpu.program_revision.revision,
                ));
                continue;
            };
            if !bounds_close(cpu_layout.bounds, gpu_node.bounds, tolerance) {
                differences.push(diagnostic(
                    "ui_gpu_cpu_layout_mismatch",
                    "GPU sampled logical bounds differ from CPU output",
                    Some(cpu_layout.node_key.clone()),
                    None,
                    cpu.program_revision.revision,
                ));
            }
        }
        differences
    }

    pub fn record_instance_timing(&mut self, elapsed: std::time::Duration) {
        self.last_timing.instance_us = elapsed.as_micros() as u64;
    }
    pub fn record_render_timing(&mut self, elapsed: std::time::Duration) {
        self.last_timing.render_us = elapsed.as_micros() as u64;
    }
}

fn create_buffers(device: &wgpu::Device, program: &UiProgram) -> UiGpuProgramBuffers {
    let budget = program.resource_budget.clone();
    UiGpuProgramBuffers {
        program_revision: program.revision.clone(),
        node_buffer: buffer(device, "ui-program-nodes", budget.max_nodes as u64 * 16),
        binding_buffer: buffer(
            device,
            "ui-program-bindings",
            budget.max_bindings as u64 * 16,
        ),
        input_buffer: buffer(
            device,
            "ui-program-inputs",
            (budget.max_bindings.max(1) as u64) * 16,
        ),
        dirty_buffer: buffer(
            device,
            "ui-program-dirty",
            (budget.max_bindings.max(1) as u64) * 4,
        ),
        branch_buffer: buffer(
            device,
            "ui-program-branches",
            (budget.max_nodes.max(1) as u64) * 4,
        ),
        layout_buffer: buffer(device, "ui-program-layout", budget.max_nodes as u64 * 16),
        clip_buffer: buffer(
            device,
            "ui-program-clips",
            (budget.max_clips.max(1) as u64) * 16,
        ),
        instance_buffer: buffer(
            device,
            "ui-program-instances",
            budget.max_instances as u64 * 16,
        ),
        diagnostic_buffer: buffer(
            device,
            "ui-program-diagnostics",
            (budget.max_nodes.max(1) as u64) * 4,
        ),
        capacity: budget,
    }
}
fn buffer(device: &wgpu::Device, label: &'static str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(4),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}
fn record_len(records: usize, stride: u64) -> u64 {
    ((records.max(1)) as u64 * stride).max(4)
}
fn record_bytes(records: usize, stride: usize) -> Vec<u8> {
    vec![0; record_len(records, stride as u64) as usize]
}

/// Bytes a full re-stage of this revision writes. Measured against the ranges
/// actually uploaded, so a delta's share is never inflated by buffers the
/// adapter leaves untouched.
fn full_upload_bytes(program: &UiProgram, inputs: &UiResolvedInputs) -> u64 {
    let budget = &program.resource_budget;
    record_len(program.nodes.len(), RECORD_STRIDE)
        + record_len(program.binding_records.len(), RECORD_STRIDE)
        + record_len(program.branch_records.len(), DIRTY_WORD)
        + record_len(program.layout_records.len(), RECORD_STRIDE)
        + record_len(inputs.changed_slots.len().max(1), DIRTY_WORD)
        + (budget.max_bindings.max(1) as u64) * RECORD_STRIDE
}

/// Builds the process-local node key to instance-range index for one revision.
///
/// Template nodes claim `max_instances` consecutive records because a shader
/// expands them at draw time; every other node claims exactly one presentation
/// state record. A node that does not fit the declared instance budget gets no
/// range, which makes `apply_frame_delta` reject it and fall back to a full
/// upload instead of writing somewhere arbitrary.
fn plan_node_ranges(program: &UiProgram) -> GpuNodeRanges {
    let capacity = program.resource_budget.max_instances as u64;
    let mut instances: BTreeMap<String, (u64, u32)> = BTreeMap::new();
    let mut cursor = 0u64;
    for record in &program.template_records {
        let count = record.max_instances.max(1) as u64;
        for key in &record.node_range {
            if cursor + count > capacity {
                continue;
            }
            instances.insert(key.clone(), (cursor * RECORD_STRIDE, count as u32));
            cursor += count;
        }
    }
    for node in &program.nodes {
        if cursor + 1 > capacity || instances.contains_key(&node.key) {
            continue;
        }
        instances.insert(node.key.clone(), (cursor * RECORD_STRIDE, 1));
        cursor += 1;
    }
    GpuNodeRanges { instances }
}

/// The 16-byte presentation state record a shader samples per instance:
/// state flags, numeric value, opacity and scroll offset.
fn state_record_bytes(state: &neon_ui_schema::UiCpuNodeState) -> [u8; RECORD_STRIDE as usize] {
    let flags = u32::from(state.visible)
        | (u32::from(state.enabled) << 1)
        | (u32::from(state.selected) << 2)
        | (u32::from(state.active) << 3);
    let mut bytes = [0u8; RECORD_STRIDE as usize];
    bytes[0..4].copy_from_slice(&flags.to_le_bytes());
    bytes[4..8].copy_from_slice(&state.numeric_value.unwrap_or(0.0).to_le_bytes());
    bytes[8..12].copy_from_slice(&state.opacity.to_le_bytes());
    bytes[12..16].copy_from_slice(&state.scroll_offset[0].to_le_bytes());
    bytes
}

/// Packs every logical layout record as one contiguous plane. Logical layout is
/// program data, not input state, so it is uploaded in full with the revision.
fn pack_layout_records(program: &UiProgram, budget: &UiResourceBudget) -> Vec<u8> {
    let mut bytes = vec![0u8; record_len(budget.max_nodes as usize, RECORD_STRIDE) as usize];
    for (index, record) in program.layout_records.iter().enumerate() {
        let offset = index * RECORD_STRIDE as usize;
        if offset + RECORD_STRIDE as usize > bytes.len() {
            break;
        }
        bytes[offset..offset + 16].copy_from_slice(&bounds_bytes(&record.bounds));
    }
    bytes
}

fn bounds_bytes(bounds: &UiBounds) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    for (index, value) in [bounds.x, bounds.y, bounds.width, bounds.height]
        .into_iter()
        .enumerate()
    {
        bytes[index * 4..(index + 1) * 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Writes `[count, slot indices..]` into the dirty plane and clears whatever the
/// previous frame left beyond the new list, so a consumer never reads a stale
/// tail as part of this frame.
fn write_dirty_slots(
    queue: &wgpu::Queue,
    buffers: &UiGpuProgramBuffers,
    layout: &InputSlotLayout,
    inputs: &UiResolvedInputs,
    previous_words: u32,
) -> u32 {
    let capacity_words = (buffers.dirty_buffer.size() / DIRTY_WORD).max(1);
    let mut slots = inputs
        .changed_slots
        .iter()
        .filter_map(|key| {
            layout
                .starts
                .get(key.split('.').next().unwrap_or(key.as_str()))
        })
        .map(|index| *index as u32)
        .collect::<Vec<u32>>();
    slots.sort_unstable();
    slots.dedup();
    slots.truncate((capacity_words - 1).max(1) as usize);
    let mut words = Vec::with_capacity(slots.len() + 1);
    words.push(slots.len() as u32);
    words.extend(slots.iter());
    let written = words.len() as u64;
    let previous_words = u64::from(previous_words).min(capacity_words);
    if written < previous_words {
        words.resize(previous_words as usize, 0);
    }
    let bytes = words
        .into_iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<u8>>();
    queue.write_buffer(&buffers.dirty_buffer, 0, &bytes);
    written as u32
}

/// Union of the keys the impact graph names and the keys the producer reports as
/// moved. Writing the union means a graph/producer disagreement can never leave
/// a stale slot on the GPU; the graph still decides which *nodes* are touched.
fn union_input_keys(impact_keys: &[String], changed_keys: &[String]) -> Vec<String> {
    let mut keys = impact_keys.to_vec();
    keys.extend(changed_keys.iter().cloned());
    keys.sort();
    keys.dedup();
    keys
}

fn pack_inputs(inputs: &UiResolvedInputs, budget: &UiResourceBudget) -> Vec<u8> {
    let mut bytes = vec![0; (budget.max_bindings.max(1) as usize) * 16];
    let mut slot_cursor = 0usize;
    for value in inputs.values.values() {
        if slot_cursor >= budget.max_bindings as usize {
            break;
        }
        for slot in flatten_value(&value.value) {
            if slot_cursor >= budget.max_bindings as usize {
                break;
            }
            bytes[slot_cursor * 16..slot_cursor * 16 + 16].copy_from_slice(&slot);
            slot_cursor += 1;
        }
    }
    bytes
}

/// Flattens a value into one or more 16-byte GPU slots.
fn flatten_value(value: &UiInputValue) -> Vec<[u8; 16]> {
    match value {
        UiInputValue::Struct { fields } => {
            let mut slots = Vec::with_capacity(fields.len());
            for field_value in fields.values() {
                slots.extend(flatten_value(field_value));
            }
            slots
        }
        UiInputValue::Array { elements, .. } => {
            let mut slots = Vec::with_capacity(elements.len());
            for element in elements {
                slots.extend(flatten_value(element));
            }
            slots
        }
        scalar => vec![pack_single_slot(scalar)],
    }
}

/// Packs a single input value into its 16-byte GPU slot.
fn pack_single_slot(value: &UiInputValue) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    match value {
        UiInputValue::Bool { value } => bytes[0] = u8::from(*value),
        UiInputValue::I32 { value } => bytes[0..4].copy_from_slice(&value.to_le_bytes()),
        UiInputValue::U32 { value } => bytes[0..4].copy_from_slice(&value.to_le_bytes()),
        UiInputValue::F32 { value } => bytes[0..4].copy_from_slice(&value.to_le_bytes()),
        UiInputValue::Vec2 { value } => {
            bytes[0..4].copy_from_slice(&value[0].to_le_bytes());
            bytes[4..8].copy_from_slice(&value[1].to_le_bytes());
        }
        UiInputValue::Vec4 { value } => {
            for i in 0..4 {
                bytes[i * 4..(i + 1) * 4].copy_from_slice(&value[i].to_le_bytes());
            }
        }
        UiInputValue::Color { value } => {
            for i in 0..4 {
                bytes[i * 4..(i + 1) * 4].copy_from_slice(&value[i].to_le_bytes());
            }
        }
        UiInputValue::TextHandle { value } => {
            bytes[0..8].copy_from_slice(&value.id.to_le_bytes());
            bytes[8..12].copy_from_slice(&value.generation.to_le_bytes());
        }
        UiInputValue::AssetHandle { id, generation } => {
            bytes[0..8].copy_from_slice(&id.to_le_bytes());
            bytes[8..12].copy_from_slice(&generation.to_le_bytes());
        }
        // Enum is resolved on the CPU side (branch predicates), not sampled in shaders.
        // CanvasData has no scalar GPU representation.
        UiInputValue::Enum { .. }
        | UiInputValue::CanvasData { .. }
        | UiInputValue::Struct { .. }
        | UiInputValue::Array { .. } => {}
    }
    bytes
}

/// Resolves a dotted path to a reference of the nested field value.
fn resolve_field_path<'a>(value: &'a UiInputValue, path: &str) -> Option<&'a UiInputValue> {
    let mut current = value;
    for segment in path.split('.') {
        match current {
            UiInputValue::Struct { fields } => {
                current = fields.get(segment)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Slot-exact writes for one frame's key list, resolved through a cached layout.
/// Building the layout scans every input, so the adapter keeps one per program
/// revision and re-derives it only when it goes stale.
fn pack_slots_for(
    keys: &[String],
    inputs: &UiResolvedInputs,
    budget: &UiResourceBudget,
    layout: &InputSlotLayout,
) -> Vec<(u64, [u8; 16])> {
    if keys.is_empty() {
        return Vec::new();
    }
    pack_slots_with_map(keys, inputs, budget, &layout.starts)
}

fn pack_slots_with_map(
    keys: &[String],
    inputs: &UiResolvedInputs,
    budget: &UiResourceBudget,
    index_map: &BTreeMap<String, usize>,
) -> Vec<(u64, [u8; 16])> {
    let mut updates = Vec::with_capacity(keys.len());
    for key in keys {
        let (top_key, field_path) = match key.split_once('.') {
            Some((top, rest)) => (top, Some(rest)),
            None => (key.as_str(), None),
        };
        let Some(&base_index) = index_map.get(top_key) else {
            continue;
        };
        let Some(resolved) = inputs.values.get(top_key) else {
            continue;
        };
        match field_path {
            None => {
                for (i, slot) in flatten_value(&resolved.value).iter().enumerate() {
                    let idx = base_index + i;
                    if idx < budget.max_bindings as usize {
                        updates.push(((idx * 16) as u64, *slot));
                    }
                }
            }
            Some(path) => {
                if let Some(field_value) = resolve_field_path(&resolved.value, path) {
                    let mut offset = 0usize;
                    let mut current = &resolved.value;
                    for segment in path.split('.') {
                        if let UiInputValue::Struct { fields } = current {
                            for (k, v) in fields {
                                if k == segment {
                                    current = v;
                                    break;
                                }
                                offset += flatten_value(v).len();
                            }
                        }
                    }
                    for (i, slot) in flatten_value(field_value).iter().enumerate() {
                        let idx = base_index + offset + i;
                        if idx < budget.max_bindings as usize {
                            updates.push(((idx * 16) as u64, *slot));
                        }
                    }
                }
            }
        }
    }
    updates
}
fn fits_budget(program: &UiProgram) -> bool {
    let budget = &program.resource_budget;
    program.nodes.len() <= budget.max_nodes as usize
        && program.binding_records.len() <= budget.max_bindings as usize
        && program.layout_records.len() <= budget.max_nodes as usize
        && program.literal_texts.len() <= budget.max_text_records as usize
        && program
            .template_records
            .iter()
            .try_fold(0u32, |total, record| {
                total.checked_add(
                    (record.node_range.len() as u32).saturating_mul(record.max_instances),
                )
            })
            .is_some_and(|count| count <= budget.max_instances)
}
fn diagnostic(
    code: &str,
    message: &str,
    node_key: Option<String>,
    input_key: Option<String>,
    revision: Revision,
) -> UiDiagnostic {
    UiDiagnostic {
        code: code.into(),
        severity: UiDiagnosticSeverity::Error,
        message: message.into(),
        node_key,
        input_key,
        source_span: None,
        revision,
    }
}
fn zero_timing() -> UiGpuPassTiming {
    UiGpuPassTiming {
        program_upload_us: 0,
        input_upload_us: 0,
        binding_us: 0,
        layout_us: 0,
        instance_us: 0,
        render_us: 0,
        readback_us: 0,
    }
}
fn empty_budget() -> UiResourceBudget {
    UiResourceBudget {
        max_nodes: 0,
        max_bindings: 0,
        max_instances: 0,
        max_text_records: 0,
        max_glyph_instances: 0,
        max_events: 0,
        max_clips: 0,
    }
}
fn bounds_close(left: UiBounds, right: UiBounds, tolerance: f32) -> bool {
    (left.x - right.x).abs() <= tolerance
        && (left.y - right.y).abs() <= tolerance
        && (left.width - right.width).abs() <= tolerance
        && (left.height - right.height).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::Revision;
    use neon_ui_schema::{
        UiInputValueSource, UiProgramCapability, UiProgramCapabilityOwner,
        UiProgramCapabilityStatus, UiProgramRevision, UiResolvedInputValue,
    };
    use std::collections::BTreeMap;

    fn test_budget(max_bindings: u32) -> UiResourceBudget {
        UiResourceBudget {
            max_nodes: 16,
            max_bindings,
            max_instances: 16,
            max_text_records: 16,
            max_glyph_instances: 16,
            max_events: 16,
            max_clips: 16,
        }
    }

    fn test_program_revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "test".into(),
            revision: Revision(1),
            schema_version: 1,
            capabilities: vec![UiProgramCapability {
                name: "static_layout".into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        }
    }

    fn resolved(value: UiInputValue) -> UiResolvedInputValue {
        UiResolvedInputValue {
            value,
            source: UiInputValueSource::Default,
            last_update_revision: Revision(0),
        }
    }

    fn make_inputs(pairs: Vec<(&str, UiInputValue)>) -> UiResolvedInputs {
        let mut values = BTreeMap::new();
        for (key, value) in pairs {
            values.insert(key.to_string(), resolved(value));
        }
        UiResolvedInputs {
            program_revision: test_program_revision(),
            input_revision: Revision(1),
            values,
            changed_slots: vec![],
        }
    }

    #[test]
    fn pack_inputs_f32_occupies_first_four_bytes() {
        let inputs = make_inputs(vec![("a", UiInputValue::F32 { value: 1.5 })]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        assert_eq!(bytes.len(), 64);
        assert_eq!(&bytes[0..4], &1.5f32.to_le_bytes());
        assert_eq!(&bytes[4..16], &[0u8; 12]);
    }

    #[test]
    fn pack_inputs_vec2_packs_xy_in_first_eight_bytes() {
        let inputs = make_inputs(vec![("pos", UiInputValue::Vec2 { value: [1.0, 2.0] })]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        assert_eq!(&bytes[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&bytes[4..8], &2.0f32.to_le_bytes());
        assert_eq!(&bytes[8..16], &[0u8; 8]);
    }

    #[test]
    fn pack_inputs_vec4_packs_xyzw_full_slot() {
        let inputs = make_inputs(vec![(
            "v",
            UiInputValue::Vec4 {
                value: [0.1, 0.2, 0.3, 0.4],
            },
        )]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        let expected: [f32; 4] = [0.1, 0.2, 0.3, 0.4];
        for i in 0..4 {
            assert_eq!(&bytes[i * 4..(i + 1) * 4], &expected[i].to_le_bytes());
        }
    }

    #[test]
    fn pack_inputs_color_packs_rgba_full_slot() {
        let inputs = make_inputs(vec![(
            "c",
            UiInputValue::Color {
                value: [1.0, 0.5, 0.0, 0.8],
            },
        )]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        let expected: [f32; 4] = [1.0, 0.5, 0.0, 0.8];
        for i in 0..4 {
            assert_eq!(&bytes[i * 4..(i + 1) * 4], &expected[i].to_le_bytes());
        }
    }

    #[test]
    fn pack_inputs_multiple_slots_are_16_byte_strided() {
        let inputs = make_inputs(vec![
            ("a", UiInputValue::F32 { value: 1.0 }),
            ("b", UiInputValue::Vec2 { value: [2.0, 3.0] }),
            (
                "c",
                UiInputValue::Vec4 {
                    value: [4.0, 5.0, 6.0, 7.0],
                },
            ),
        ]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        // BTreeMap sorts by key: a, b, c
        // slot 0 (a): f32 = 1.0
        assert_eq!(&bytes[0..4], &1.0f32.to_le_bytes());
        // slot 1 (b): vec2 = [2.0, 3.0]
        assert_eq!(&bytes[16..20], &2.0f32.to_le_bytes());
        assert_eq!(&bytes[20..24], &3.0f32.to_le_bytes());
        // slot 2 (c): vec4 = [4.0, 5.0, 6.0, 7.0]
        assert_eq!(&bytes[32..36], &4.0f32.to_le_bytes());
        assert_eq!(&bytes[36..40], &5.0f32.to_le_bytes());
        assert_eq!(&bytes[40..44], &6.0f32.to_le_bytes());
        assert_eq!(&bytes[44..48], &7.0f32.to_le_bytes());
    }

    #[test]
    fn pack_inputs_bool_packs_as_u8() {
        let inputs = make_inputs(vec![("flag", UiInputValue::Bool { value: true })]);
        let bytes = pack_inputs(&inputs, &test_budget(2));
        assert_eq!(bytes[0], 1u8);
        assert_eq!(&bytes[1..16], &[0u8; 15]);
    }

    #[test]
    fn pack_inputs_enum_leaves_slot_zero() {
        // Enum is CPU-side only; GPU slot should be zeroed.
        let inputs = make_inputs(vec![(
            "mode",
            UiInputValue::Enum {
                value: "compact".into(),
            },
        )]);
        let bytes = pack_inputs(&inputs, &test_budget(2));
        assert_eq!(&bytes[0..16], &[0u8; 16]);
    }

    /// Runs the cached-layout slot pack the adapter actually uses.
    fn pack_changed(inputs: &UiResolvedInputs, budget: &UiResourceBudget) -> Vec<(u64, [u8; 16])> {
        let layout = InputSlotLayout::build(inputs, budget);
        pack_slots_for(&inputs.changed_slots, inputs, budget, &layout)
    }

    fn make_inputs_with_changes(
        pairs: Vec<(&str, UiInputValue)>,
        changed: Vec<&str>,
    ) -> UiResolvedInputs {
        let mut inputs = make_inputs(pairs);
        inputs.changed_slots = changed.into_iter().map(str::to_string).collect();
        inputs
    }

    #[test]
    fn pack_slots_for_returns_empty_when_no_changes() {
        let inputs = make_inputs(vec![("a", UiInputValue::F32 { value: 1.0 })]);
        let updates = pack_changed(&inputs, &test_budget(4));
        assert!(updates.is_empty());
    }

    #[test]
    fn pack_slots_for_returns_correct_offset_and_bytes() {
        let inputs = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::Vec2 { value: [2.0, 3.0] }),
                ("c", UiInputValue::F32 { value: 4.0 }),
            ],
            vec!["b"],
        );
        let updates = pack_changed(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 1);
        // "b" is the second key in BTreeMap order -> index 1 -> offset 16
        assert_eq!(updates[0].0, 16);
        assert_eq!(&updates[0].1[0..4], &2.0f32.to_le_bytes());
        assert_eq!(&updates[0].1[4..8], &3.0f32.to_le_bytes());
    }

    #[test]
    fn pack_slots_for_handles_multiple_changes() {
        let inputs = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::F32 { value: 2.0 }),
                ("c", UiInputValue::F32 { value: 3.0 }),
            ],
            vec!["a", "c"],
        );
        let updates = pack_changed(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 2);
        // "a" -> index 0 -> offset 0, "c" -> index 2 -> offset 32
        assert_eq!(updates[0].0, 0);
        assert_eq!(updates[1].0, 32);
        assert_eq!(&updates[0].1[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&updates[1].1[0..4], &3.0f32.to_le_bytes());
    }

    #[test]
    fn pack_slots_for_ignores_unknown_keys() {
        let inputs = make_inputs_with_changes(
            vec![("a", UiInputValue::F32 { value: 1.0 })],
            vec!["nonexistent"],
        );
        let updates = pack_changed(&inputs, &test_budget(4));
        assert!(updates.is_empty());
    }

    #[test]
    fn pack_slots_for_matches_full_pack_for_changed_slot() {
        // The partial bytes for a changed slot must equal the corresponding
        // 16-byte region in the full pack.
        let inputs = make_inputs_with_changes(
            vec![
                (
                    "a",
                    UiInputValue::Vec4 {
                        value: [0.1, 0.2, 0.3, 0.4],
                    },
                ),
                (
                    "b",
                    UiInputValue::Color {
                        value: [1.0, 0.0, 0.0, 1.0],
                    },
                ),
            ],
            vec!["b"],
        );
        let full = pack_inputs(&inputs, &test_budget(4));
        let partial = pack_changed(&inputs, &test_budget(4));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].0, 16);
        assert_eq!(partial[0].1, &full[16..32]);
    }

    use std::collections::BTreeMap as TestBTreeMap;

    fn struct_value(fields: Vec<(&str, UiInputValue)>) -> UiInputValue {
        let mut map = TestBTreeMap::new();
        for (k, v) in fields {
            map.insert(k.to_string(), v);
        }
        UiInputValue::Struct { fields: map }
    }

    #[test]
    fn struct_flattens_into_consecutive_slots() {
        // Struct with 3 scalar fields occupies 3 consecutive 16-byte slots.
        let player = struct_value(vec![
            ("hp", UiInputValue::F32 { value: 0.8 }),
            ("level", UiInputValue::U32 { value: 42 }),
            (
                "name",
                UiInputValue::TextHandle {
                    value: neon_ui_schema::UiTextHandle {
                        id: 7,
                        generation: 1,
                    },
                },
            ),
        ]);
        let inputs = make_inputs(vec![("player", player)]);
        let bytes = pack_inputs(&inputs, &test_budget(8));
        // BTreeMap order: hp, level, name
        // slot 0: hp = 0.8
        assert_eq!(&bytes[0..4], &0.8f32.to_le_bytes());
        // slot 1: level = 42
        assert_eq!(&bytes[16..20], &42u32.to_le_bytes());
        // slot 2: name = TextHandle { id: 7, generation: 1 }
        assert_eq!(&bytes[32..40], &7u64.to_le_bytes());
        assert_eq!(&bytes[40..44], &1u32.to_le_bytes());
    }

    #[test]
    fn struct_follows_scalar_in_slot_layout() {
        // A scalar before a struct: scalar takes slot 0, struct fields start at slot 1.
        let player = struct_value(vec![
            ("x", UiInputValue::F32 { value: 1.0 }),
            ("y", UiInputValue::F32 { value: 2.0 }),
        ]);
        let inputs = make_inputs(vec![
            ("flag", UiInputValue::Bool { value: true }),
            ("player", player),
        ]);
        let bytes = pack_inputs(&inputs, &test_budget(8));
        // slot 0: flag
        assert_eq!(bytes[0], 1u8);
        // slot 1: player.x = 1.0
        assert_eq!(&bytes[16..20], &1.0f32.to_le_bytes());
        // slot 2: player.y = 2.0
        assert_eq!(&bytes[32..36], &2.0f32.to_le_bytes());
    }

    #[test]
    fn nested_struct_flattens_recursively() {
        let inner = struct_value(vec![
            ("a", UiInputValue::F32 { value: 10.0 }),
            ("b", UiInputValue::F32 { value: 20.0 }),
        ]);
        let outer = struct_value(vec![
            ("inner", inner),
            ("c", UiInputValue::F32 { value: 30.0 }),
        ]);
        let inputs = make_inputs(vec![("outer", outer)]);
        let bytes = pack_inputs(&inputs, &test_budget(8));
        // BTreeMap order: c, inner { a, b }  ("c" < "inner")
        // slot 0: outer.c = 30.0
        assert_eq!(&bytes[0..4], &30.0f32.to_le_bytes());
        // slot 1: outer.inner.a = 10.0
        assert_eq!(&bytes[16..20], &10.0f32.to_le_bytes());
        // slot 2: outer.inner.b = 20.0
        assert_eq!(&bytes[32..36], &20.0f32.to_le_bytes());
    }

    #[test]
    fn struct_field_partial_update() {
        let player = struct_value(vec![
            ("hp", UiInputValue::F32 { value: 0.8 }),
            ("mp", UiInputValue::F32 { value: 0.5 }),
        ]);
        let inputs = make_inputs_with_changes(vec![("player", player)], vec!["player.mp"]);
        let updates = pack_changed(&inputs, &test_budget(4));
        // player.mp is the second field -> offset 1 within struct -> absolute slot 1 -> offset 16
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, 16);
        assert_eq!(&updates[0].1[0..4], &0.5f32.to_le_bytes());
    }

    #[test]
    fn whole_struct_partial_update_updates_all_fields() {
        let player = struct_value(vec![
            ("hp", UiInputValue::F32 { value: 0.8 }),
            ("mp", UiInputValue::F32 { value: 0.5 }),
        ]);
        let inputs = make_inputs_with_changes(vec![("player", player)], vec!["player"]);
        let updates = pack_changed(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].0, 0); // hp
        assert_eq!(updates[1].0, 16); // mp
    }

    #[test]
    fn array_of_f32_flattens_to_contiguous_slots() {
        let arr = UiInputValue::Array {
            elements: vec![
                UiInputValue::F32 { value: 1.0 },
                UiInputValue::F32 { value: 2.0 },
                UiInputValue::F32 { value: 3.0 },
            ],
            element_kind: Box::new(neon_ui_schema::UiInputKind::F32),
        };
        let slots = flatten_value(&arr);
        assert_eq!(slots.len(), 3);
        assert_eq!(f32::from_le_bytes(slots[0][0..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(slots[1][0..4].try_into().unwrap()), 2.0);
        assert_eq!(f32::from_le_bytes(slots[2][0..4].try_into().unwrap()), 3.0);
    }

    #[test]
    fn array_of_struct_flattens_all_fields_contiguously() {
        let make_slot = |count: u32| {
            struct_value(vec![
                (
                    "item",
                    UiInputValue::TextHandle {
                        value: neon_ui_schema::UiTextHandle {
                            id: 0,
                            generation: 0,
                        },
                    },
                ),
                ("count", UiInputValue::U32 { value: count }),
            ])
        };
        let arr = UiInputValue::Array {
            elements: vec![make_slot(3), make_slot(5)],
            element_kind: Box::new(neon_ui_schema::UiInputKind::Struct {
                fields: [
                    ("item".into(), neon_ui_schema::UiInputKind::TextHandle),
                    ("count".into(), neon_ui_schema::UiInputKind::U32),
                ]
                .into_iter()
                .collect(),
            }),
        };
        let slots = flatten_value(&arr);
        assert_eq!(slots.len(), 4);
        // BTreeMap alphabetical: count before item
        // slot 0 = count[0], slot 1 = item[0], slot 2 = count[1], slot 3 = item[1]
        assert_eq!(u32::from_le_bytes(slots[0][0..4].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(slots[2][0..4].try_into().unwrap()), 5);
    }

    #[test]
    fn array_gpu_slot_count_matches_length() {
        use neon_ui_schema::UiInputKind;
        let kind = UiInputKind::Array {
            element_kind: Box::new(UiInputKind::F32),
            length: 24,
        };
        assert_eq!(kind.gpu_slot_count(), 24);
        let struct_kind = UiInputKind::Array {
            element_kind: Box::new(UiInputKind::Struct {
                fields: [
                    ("a".into(), UiInputKind::F32),
                    ("b".into(), UiInputKind::F32),
                ]
                .into_iter()
                .collect(),
            }),
            length: 10,
        };
        assert_eq!(struct_kind.gpu_slot_count(), 20);
    }

    const PLAN_FLOW: &str = "version 1
surface test.surface revision 1
budget nodes=16 bindings=16 instances=16 text=16 glyphs=64 events=16 clips=16
input left bool default false
input right bool default false
surface root row w 200 h 80
  panel left-panel visible $left w 80 h 40
  panel right-panel visible $right w 80 h 40
  panel filler-a w 20 h 20
  panel filler-b w 20 h 20
";

    fn program_from_flow(flow: &str) -> UiProgram {
        let document = neon_ui_runtime::parse_nui_flow(flow).expect("flow parses");
        let revision = UiProgramRevision {
            program_id: "test.surface".into(),
            revision: Revision(1),
            schema_version: 1,
            capabilities: vec![UiProgramCapability {
                name: "ui.program.v1".into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        };
        neon_ui_runtime::compile_nui_flow_program(&document, revision).expect("flow compiles")
    }

    #[test]
    fn every_node_gets_its_own_instance_range() {
        let program = program_from_flow(PLAN_FLOW);
        let ranges = plan_node_ranges(&program);
        let mut offsets: Vec<u64> = ranges
            .instances
            .values()
            .map(|(offset, _)| *offset)
            .collect();
        offsets.sort_unstable();
        let total = offsets.len();
        offsets.dedup();
        assert_eq!(offsets.len(), total, "instance ranges must not overlap");
        for node in &program.nodes {
            assert!(ranges.is_covered(&node.key), "{} uncovered", node.key);
            assert_eq!(
                ranges.instances[&node.key].1, 1,
                "a non-template node owns exactly one presentation record"
            );
        }
    }

    #[test]
    fn nodes_beyond_the_instance_budget_stay_uncovered() {
        let mut program = program_from_flow(PLAN_FLOW);
        program.resource_budget.max_instances = 2;
        let ranges = plan_node_ranges(&program);
        assert_eq!(ranges.instances.len(), 2);
        assert_eq!(
            program
                .nodes
                .iter()
                .filter(|node| ranges.is_covered(&node.key))
                .count(),
            2,
            "an uncovered node must send the delta back to a full upload"
        );
    }

    #[test]
    fn layout_records_are_packed_at_the_record_stride() {
        let program = program_from_flow(PLAN_FLOW);
        let bytes = pack_layout_records(&program, &program.resource_budget);
        assert_eq!(
            &bytes[0..4],
            &program.layout_records[0].bounds.x.to_le_bytes()
        );
        assert_eq!(
            &bytes[16..20],
            &program.layout_records[1].bounds.x.to_le_bytes()
        );
    }

    #[test]
    fn state_record_encodes_flags_numeric_and_opacity() {
        let state = neon_ui_schema::UiCpuNodeState {
            node_key: "n".into(),
            visible: true,
            enabled: true,
            selected: false,
            active: true,
            numeric_value: Some(2.5),
            state_token: None,
            text: None,
            image: None,
            opacity: 0.5,
            scroll_offset: [1.25, 0.0],
        };
        let bytes = state_record_bytes(&state);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0b1011);
        assert_eq!(f32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2.5);
        assert_eq!(f32::from_le_bytes(bytes[8..12].try_into().unwrap()), 0.5);
        assert_eq!(
            f32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            1.25,
            "the record stays one 16-byte slot"
        );
    }

    #[test]
    fn cached_slot_layout_only_rebuilds_for_keys_it_cannot_explain() {
        let budget = test_budget(4);
        let single = make_inputs(vec![("a", UiInputValue::F32 { value: 1.0 })]);
        let cached = InputSlotLayout::build(&single, &budget);
        let both = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::F32 { value: 2.0 }),
            ],
            vec!["b"],
        );
        assert!(
            cached.is_stale_for(&both, &both.changed_slots),
            "a key the cache never saw must invalidate it"
        );
        let unchanged = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::F32 { value: 2.0 }),
            ],
            vec!["a"],
        );
        let wide = InputSlotLayout::build(&both, &budget);
        assert!(
            !wide.is_stale_for(&unchanged, &unchanged.changed_slots),
            "a plain value change must not rescan the layout"
        );
        let grown = make_inputs_with_changes(
            vec![
                (
                    "a",
                    struct_value(vec![
                        ("x", UiInputValue::F32 { value: 1.0 }),
                        ("y", UiInputValue::F32 { value: 2.0 }),
                    ]),
                ),
                ("b", UiInputValue::F32 { value: 2.0 }),
            ],
            vec!["a"],
        );
        assert!(
            wide.is_stale_for(&grown, &grown.changed_slots),
            "a wider expansion under a known key must invalidate the cache"
        );
    }

    #[test]
    fn union_input_keys_sorts_and_dedups() {
        let keys = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_owned()).collect()
        };
        assert_eq!(
            union_input_keys(&keys(&["b", "a"]), &keys(&["a", "c"])),
            keys(&["a", "b", "c"])
        );
    }
}
