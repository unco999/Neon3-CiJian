//! Renderer-private adapter for the static UI program contract.
//!
//! This module is intentionally the only place that turns a `UiProgram` into
//! WGPU buffers.  Its sampled layout output is diagnostic data, never an input
//! or a replacement for the UI runtime's CPU execution backend.

use std::collections::BTreeMap;
use std::time::Instant;

use neon_protocol::Revision;
use neon_ui_schema::{
    UiBoundProperty, UiBounds, UiBranchPredicate, UiDiagnostic, UiDiagnosticSeverity,
    UiGpuBackendAdapter, UiGpuFrameState, UiGpuLayoutNode, UiGpuLayoutReadback, UiGpuPassTiming,
    UiGpuUploadStatus, UiInputValue, UiProgram, UiProgramRevision, UiResolvedInputs,
    UiResourceBudget,
};

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

#[derive(Clone)]
struct StagedProgram {
    program: UiProgram,
    inputs: UiResolvedInputs,
    viewport: UiBounds,
    dirty_slots: Vec<String>,
}

/// WGPU-owner adapter. A staged update is only made observable by
/// `activate_at_frame_boundary`, which prevents a partially uploaded program
/// or input revision from being rendered.
pub struct GpuUiProgramBackend {
    renderer_epoch: u64,
    buffers: Option<UiGpuProgramBuffers>,
    staged: Option<StagedProgram>,
    active: Option<StagedProgram>,
    frame_sequence: u64,
    diagnostics: Vec<UiDiagnostic>,
    last_timing: UiGpuPassTiming,
    last_readback: Option<UiGpuLayoutReadback>,
}

impl GpuUiProgramBackend {
    pub fn new(renderer_epoch: u64) -> Self {
        Self {
            renderer_epoch,
            buffers: None,
            staged: None,
            active: None,
            frame_sequence: 0,
            diagnostics: Vec::new(),
            last_timing: zero_timing(),
            last_readback: None,
        }
    }

    pub fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        program: &UiProgram,
        inputs: &UiResolvedInputs,
        viewport: UiBounds,
    ) -> Result<(), UiDiagnostic> {
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
        let recreate = self.buffers.as_ref().is_none_or(|current| {
            current.program_revision != program.revision
                || current.capacity != program.resource_budget
        });
        if recreate {
            self.buffers = Some(create_buffers(device, program));
        }
        let buffers = self.buffers.as_ref().expect("created above");
        let program_upload = started.elapsed().as_micros() as u64;
        let input_started = Instant::now();
        queue.write_buffer(
            &buffers.node_buffer,
            0,
            &record_bytes(program.nodes.len(), 16),
        );
        queue.write_buffer(
            &buffers.binding_buffer,
            0,
            &record_bytes(program.binding_records.len(), 16),
        );
        queue.write_buffer(
            &buffers.branch_buffer,
            0,
            &record_bytes(program.branch_records.len(), 4),
        );
        // Input buffer: full upload on first stage / program change, partial
        // upload when only specific slots changed and the buffer already exists.
        if recreate || inputs.changed_slots.is_empty() {
            queue.write_buffer(
                &buffers.input_buffer,
                0,
                &pack_inputs(inputs, &program.resource_budget),
            );
        } else {
            for (offset, slot_bytes) in pack_changed_slots(inputs, &program.resource_budget) {
                queue.write_buffer(&buffers.input_buffer, offset, &slot_bytes);
            }
        }
        queue.write_buffer(
            &buffers.dirty_buffer,
            0,
            &record_bytes(inputs.changed_slots.len(), 4),
        );
        self.last_timing.program_upload_us = program_upload;
        self.last_timing.input_upload_us = input_started.elapsed().as_micros() as u64;
        self.staged = Some(StagedProgram {
            program: program.clone(),
            inputs: inputs.clone(),
            viewport,
            dirty_slots: inputs.changed_slots.clone(),
        });
        Ok(())
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
                .buffers
                .as_ref()
                .map(|buffers| buffers.capacity.clone())
                .unwrap_or_else(empty_budget),
            diagnostics: self.diagnostics.clone(),
            last_timing: self.last_timing.clone(),
        }
    }

    pub fn last_readback(&self) -> Option<&UiGpuLayoutReadback> {
        self.last_readback.as_ref()
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
fn record_bytes(records: usize, stride: usize) -> Vec<u8> {
    vec![0; (records.max(1) * stride).max(4)]
}
fn pack_inputs(inputs: &UiResolvedInputs, budget: &UiResourceBudget) -> Vec<u8> {
    let mut bytes = vec![0; (budget.max_bindings.max(1) as usize) * 16];
    let mut slot_cursor = 0usize;
    for value in inputs.values.values() {
        if slot_cursor >= budget.max_bindings as usize { break; }
        for slot in flatten_value(&value.value) {
            if slot_cursor >= budget.max_bindings as usize { break; }
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
            for field_value in fields.values() { slots.extend(flatten_value(field_value)); }
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
        UiInputValue::Enum { .. } | UiInputValue::CanvasData { .. } | UiInputValue::Struct { .. } => {}
    }
    bytes
}

/// Builds a key -> starting-slot-index map, accounting for Struct multi-slot expansion.
fn slot_index_map(inputs: &UiResolvedInputs, budget: &UiResourceBudget) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    let mut cursor = 0usize;
    for (key, value) in inputs.values.iter() {
        if cursor >= budget.max_bindings as usize { break; }
        map.insert(key.clone(), cursor);
        cursor += flatten_value(&value.value).len();
    }
    map
}

/// Resolves a dotted path to a reference of the nested field value.
fn resolve_field_path<'a>(value: &'a UiInputValue, path: &str) -> Option<&'a UiInputValue> {
    let mut current = value;
    for segment in path.split('.') {
        match current {
            UiInputValue::Struct { fields } => { current = fields.get(segment)?; }
            _ => return None,
        }
    }
    Some(current)
}

/// Returns (offset, 16-byte slot) pairs for changed slots. Supports "key" and "key.field" paths.
fn pack_changed_slots(
    inputs: &UiResolvedInputs,
    budget: &UiResourceBudget,
) -> Vec<(u64, [u8; 16])> {
    if inputs.changed_slots.is_empty() { return Vec::new(); }
    let index_map = slot_index_map(inputs, budget);
    let mut updates = Vec::with_capacity(inputs.changed_slots.len());
    for key in &inputs.changed_slots {
        let (top_key, field_path) = match key.split_once('.') {
            Some((top, rest)) => (top, Some(rest)),
            None => (key.as_str(), None),
        };
        let Some(&base_index) = index_map.get(top_key) else { continue; };
        let Some(resolved) = inputs.values.get(top_key) else { continue; };
        match field_path {
            None => {
                for (i, slot) in flatten_value(&resolved.value).iter().enumerate() {
                    let idx = base_index + i;
                    if idx < budget.max_bindings as usize { updates.push(((idx * 16) as u64, *slot)); }
                }
            }
            Some(path) => {
                if let Some(field_value) = resolve_field_path(&resolved.value, path) {
                    let mut offset = 0usize;
                    let mut current = &resolved.value;
                    for segment in path.split('.') {
                        if let UiInputValue::Struct { fields } = current {
                            for (k, v) in fields {
                                if k == segment { current = v; break; }
                                offset += flatten_value(v).len();
                            }
                        }
                    }
                    for (i, slot) in flatten_value(field_value).iter().enumerate() {
                        let idx = base_index + offset + i;
                        if idx < budget.max_bindings as usize { updates.push(((idx * 16) as u64, *slot)); }
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
        let inputs = make_inputs(vec![("v", UiInputValue::Vec4 { value: [0.1, 0.2, 0.3, 0.4] })]);
        let bytes = pack_inputs(&inputs, &test_budget(4));
        let expected: [f32; 4] = [0.1, 0.2, 0.3, 0.4];
        for i in 0..4 {
            assert_eq!(&bytes[i * 4..(i + 1) * 4], &expected[i].to_le_bytes());
        }
    }

    #[test]
    fn pack_inputs_color_packs_rgba_full_slot() {
        let inputs = make_inputs(vec![("c", UiInputValue::Color { value: [1.0, 0.5, 0.0, 0.8] })]);
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
            ("c", UiInputValue::Vec4 { value: [4.0, 5.0, 6.0, 7.0] }),
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
        let inputs = make_inputs(vec![("mode", UiInputValue::Enum { value: "compact".into() })]);
        let bytes = pack_inputs(&inputs, &test_budget(2));
        assert_eq!(&bytes[0..16], &[0u8; 16]);
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
    fn pack_changed_slots_returns_empty_when_no_changes() {
        let inputs = make_inputs(vec![("a", UiInputValue::F32 { value: 1.0 })]);
        let updates = pack_changed_slots(&inputs, &test_budget(4));
        assert!(updates.is_empty());
    }

    #[test]
    fn pack_changed_slots_returns_correct_offset_and_bytes() {
        let inputs = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::Vec2 { value: [2.0, 3.0] }),
                ("c", UiInputValue::F32 { value: 4.0 }),
            ],
            vec!["b"],
        );
        let updates = pack_changed_slots(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 1);
        // "b" is the second key in BTreeMap order -> index 1 -> offset 16
        assert_eq!(updates[0].0, 16);
        assert_eq!(&updates[0].1[0..4], &2.0f32.to_le_bytes());
        assert_eq!(&updates[0].1[4..8], &3.0f32.to_le_bytes());
    }

    #[test]
    fn pack_changed_slots_handles_multiple_changes() {
        let inputs = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::F32 { value: 1.0 }),
                ("b", UiInputValue::F32 { value: 2.0 }),
                ("c", UiInputValue::F32 { value: 3.0 }),
            ],
            vec!["a", "c"],
        );
        let updates = pack_changed_slots(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 2);
        // "a" -> index 0 -> offset 0, "c" -> index 2 -> offset 32
        assert_eq!(updates[0].0, 0);
        assert_eq!(updates[1].0, 32);
        assert_eq!(&updates[0].1[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&updates[1].1[0..4], &3.0f32.to_le_bytes());
    }

    #[test]
    fn pack_changed_slots_ignores_unknown_keys() {
        let inputs = make_inputs_with_changes(
            vec![("a", UiInputValue::F32 { value: 1.0 })],
            vec!["nonexistent"],
        );
        let updates = pack_changed_slots(&inputs, &test_budget(4));
        assert!(updates.is_empty());
    }

    #[test]
    fn pack_changed_slots_matches_full_pack_for_changed_slot() {
        // The partial bytes for a changed slot must equal the corresponding
        // 16-byte region in the full pack.
        let inputs = make_inputs_with_changes(
            vec![
                ("a", UiInputValue::Vec4 { value: [0.1, 0.2, 0.3, 0.4] }),
                ("b", UiInputValue::Color { value: [1.0, 0.0, 0.0, 1.0] }),
            ],
            vec!["b"],
        );
        let full = pack_inputs(&inputs, &test_budget(4));
        let partial = pack_changed_slots(&inputs, &test_budget(4));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].0, 16);
        assert_eq!(partial[0].1, &full[16..32]);
    }

    use std::collections::BTreeMap as TestBTreeMap;

    fn struct_value(fields: Vec<(&str, UiInputValue)>) -> UiInputValue {
        let mut map = TestBTreeMap::new();
        for (k, v) in fields { map.insert(k.to_string(), v); }
        UiInputValue::Struct { fields: map }
    }

    #[test]
    fn struct_flattens_into_consecutive_slots() {
        // Struct with 3 scalar fields occupies 3 consecutive 16-byte slots.
        let player = struct_value(vec![
            ("hp", UiInputValue::F32 { value: 0.8 }),
            ("level", UiInputValue::U32 { value: 42 }),
            ("name", UiInputValue::TextHandle { value: neon_ui_schema::UiTextHandle { id: 7, generation: 1 } }),
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
        let updates = pack_changed_slots(&inputs, &test_budget(4));
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
        let updates = pack_changed_slots(&inputs, &test_budget(4));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].0, 0);  // hp
        assert_eq!(updates[1].0, 16); // mp
    }
}