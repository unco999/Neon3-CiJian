//! Renderer-private adapter for the static UI program contract.
//!
//! This module is intentionally the only place that turns a `UiProgram` into
//! WGPU buffers.  Its sampled layout output is diagnostic data, never an input
//! or a replacement for the UI runtime's CPU execution backend.

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
        queue.write_buffer(
            &buffers.input_buffer,
            0,
            &pack_inputs(inputs, &program.resource_budget),
        );
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
    for (index, value) in inputs
        .values
        .values()
        .take(budget.max_bindings as usize)
        .enumerate()
    {
        let offset = index * 16;
        match &value.value {
            UiInputValue::Bool { value } => bytes[offset] = u8::from(*value),
            UiInputValue::I32 { value } => {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
            }
            UiInputValue::U32 { value } => {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
            }
            UiInputValue::F32 { value } => {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
            }
            UiInputValue::TextHandle { value } => {
                bytes[offset..offset + 8].copy_from_slice(&value.id.to_le_bytes());
                bytes[offset + 8..offset + 12].copy_from_slice(&value.generation.to_le_bytes());
            }
            _ => {}
        }
    }
    bytes
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
