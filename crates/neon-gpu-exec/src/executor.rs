//! The GPU executor: compiled scene -> wgpu compute dispatches -> readback.

use std::collections::HashMap;

use neon_gpu::hal_map::{self, HalBackend, MappedBuffer};
use neon_gpu_script::ir::{IrNode, NodeKind};
use neon_gpu_script::CompiledScene;

use crate::codelet::{split_args, Codelet, ConstArg, FieldTy};
use crate::error::ExecError;

const WORKGROUP: u32 = 64;

/// A caller-provided scene input (a world resource bound to an input alias).
#[derive(Clone)]
pub struct InputField {
    pub buffer: wgpu::Buffer,
    /// Elements per entity (stats has 8, scalar fields 1).
    pub per_entity: u32,
    pub ty: FieldTy,
}

/// A kernel codelet plus a pipeline instance cache.
pub struct Executor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    backend: HalBackend,
    codelets: HashMap<String, Box<dyn Codelet>>,
    pipelines: HashMap<String, PipelineEntry>,
}

struct PipelineEntry {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl Executor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let backend = HalBackend::from_backend(device.adapter_info().backend)
            .expect("unsupported backend for hal mapping");
        Self {
            device,
            queue,
            backend,
            codelets: HashMap::new(),
            pipelines: HashMap::new(),
        }
    }

    pub fn register_codelet(&mut self, id: &str, codelet: Box<dyn Codelet>) {
        self.codelets.insert(id.to_string(), codelet);
    }

    /// Executes one scene end to end and returns the exported outputs keyed by
    /// their qualified world name (e.g. `target.hp`).
    pub fn run(
        &mut self,
        scene: &CompiledScene,
        inputs: &HashMap<String, InputField>,
    ) -> Result<HashMap<String, Vec<f32>>, ExecError> {
        let ir = &scene.ir;
        let n = self.infer_entity_count(ir, inputs)?;

        // ---- node buffers -------------------------------------------------
        let mut node_buffers: Vec<Option<wgpu::Buffer>> = vec![None; ir.nodes.len()];
        let mut node_types: Vec<FieldTy> = vec![FieldTy::F32; ir.nodes.len()];
        for (alias, id) in &ir.inputs {
            let field = inputs.get(alias).ok_or(ExecError::MissingInput {
                alias: alias.clone(),
            })?;
            if field.per_entity * field.ty.bytes() * n != field.buffer.size() as u32 {
                return Err(ExecError::Readback(format!(
                    "input `{alias}` buffer size does not match {n} entities * {} elements",
                    field.per_entity
                )));
            }
            node_buffers[*id] = Some(field.buffer.clone());
            node_types[*id] = field.ty;
        }

        // ---- record all dispatches into one encoder (GPU runs to completion)
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon-gpu-exec scene"),
        });

        for wave in &scene.waves {
            for &node_id in wave {
                let out = self.dispatch_node(
                    &ir.nodes[node_id],
                    &node_buffers,
                    &node_types,
                    n,
                    &mut encoder,
                )?;
                if let Some(buf) = out {
                    node_buffers[node_id] = Some(buf);
                }
            }
        }

        // ---- readback buffers for exports ---------------------------------
        let mut readbacks = Vec::new();
        for (target, node_id) in &ir.exports {
            let buf = node_buffers[*node_id].clone().ok_or(ExecError::MissingValueBuffer(*node_id))?;
            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("readback::{target}")),
                size: n as u64 * 4,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(&buf, 0, &readback, 0, n as u64 * 4);
            readbacks.push((target.clone(), readback));
        }

        self.queue.submit([encoder.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .map_err(|e| ExecError::Poll(e.to_string()))?;

        // ---- read back ----------------------------------------------------
        let mut outputs = HashMap::new();
        for (target, readback) in readbacks {
            let values = self.readback_f32(&readback, n)?;
            outputs.insert(target.to_string(), values);
        }
        Ok(outputs)
    }

    fn dispatch_node(
        &mut self,
        node: &IrNode,
        node_buffers: &[Option<wgpu::Buffer>],
        node_types: &[FieldTy],
        n: u32,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<Option<wgpu::Buffer>, ExecError> {
        let NodeKind::Kernel { kernel } = &node.kind else {
            return Ok(None); // input nodes are not dispatched
        };
        let codelet = self
            .codelets
            .get(kernel)
            .ok_or_else(|| ExecError::UnknownCodelet(kernel.clone()))?;

        let (values, consts) = split_args(&node.args);
        let const_keys: Vec<String> = consts.iter().map(|c| c.key.clone()).collect();
        if !codelet.accepts(values.len(), &const_keys) {
            for c in &consts {
                if !codelet.allowed_consts().contains(&c.key) {
                    return Err(ExecError::DisallowedConst {
                        name: kernel.clone(),
                        key: c.key.clone(),
                    });
                }
            }
            return Err(ExecError::ValueCount {
                name: kernel.clone(),
                expected: codelet.input_count(),
                actual: values.len(),
            });
        }

        let value_buffers: Vec<wgpu::Buffer> = values
            .iter()
            .map(|id| {
                node_buffers
                    .get(*id)
                    .and_then(Option::clone)
                    .ok_or(ExecError::MissingValueBuffer(*id))
            })
            .collect::<Result<_, _>>()?;
        let value_types: Vec<FieldTy> = values.iter().map(|id| node_types[*id]).collect();

        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("node::{}::out", node.id)),
            size: n as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let key = pipeline_key(kernel, n, values.len(), &consts);
        if !self.pipelines.contains_key(&key) {
            let wgsl = codelet.wgsl(&consts, n, &value_types);
            let built = self.build_pipeline(kernel, wgsl, values.len());
            self.pipelines.insert(key.clone(), built);
        }
        let entry = self.pipelines.get(&key).expect("pipeline just inserted");

        let mut bg_entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(value_buffers.len() + 1);
        for (i, buf) in value_buffers.iter().enumerate() {
            bg_entries.push(wgpu::BindGroupEntry {
                binding: i as u32,
                resource: buf.as_entire_binding(),
            });
        }
        bg_entries.push(wgpu::BindGroupEntry {
            binding: value_buffers.len() as u32,
            resource: out.as_entire_binding(),
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("bg::{kernel}")),
            layout: &entry.bind_group_layout,
            entries: &bg_entries,
        });

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&entry.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(n.div_ceil(WORKGROUP), 1, 1);

        Ok(Some(out))
    }

    fn build_pipeline(
        &self,
        kernel: &str,
        wgsl: String,
        value_count: usize,
    ) -> PipelineEntry {
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("shader::{kernel}")),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });
        let entries: Vec<wgpu::BindGroupLayoutEntry> = (0..=value_count)
            .map(|b| wgpu::BindGroupLayoutEntry {
                binding: b as u32,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: b != value_count,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let bind_group_layout = self.device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some(&format!("bgl::{kernel}")),
                entries: &entries,
            },
        );
        let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("layout::{kernel}")),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&format!("pipeline::{kernel}")),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        PipelineEntry { pipeline, bind_group_layout }
    }

    fn infer_entity_count(
        &self,
        ir: &neon_gpu_script::IrScene,
        inputs: &HashMap<String, InputField>,
    ) -> Result<u32, ExecError> {
        for (alias, _) in &ir.inputs {
            if !inputs.contains_key(alias) {
                return Err(ExecError::MissingInput {
                    alias: alias.clone(),
                });
            }
        }
        let mut n: Option<u32> = None;
        for (alias, id) in &ir.inputs {
            let field = &inputs[alias];
            let per = field.per_entity * field.ty.bytes();
            if per == 0 {
                return Err(ExecError::EmptyBuffer);
            }
            let count = field.buffer.size() as u32 / per;
            match n {
                None => n = Some(count),
                Some(prev) if prev != count => {
                    return Err(ExecError::Readback(format!(
                        "input `{alias}` has {count} entities, expected {prev}"
                    )));
                }
                _ => {}
            }
            let _ = id;
        }
        n.ok_or(ExecError::Readback("scene has no inputs".into()))
    }

    fn readback_f32(&self, buffer: &wgpu::Buffer, n: u32) -> Result<Vec<f32>, ExecError> {
        let size = n as u64 * 4;
        if size == 0 {
            return Err(ExecError::EmptyBuffer);
        }
        // SAFETY: the caller-created readback buffer has MAP_READ usage and is
        // not mapped through any other path.
        let mapping = unsafe { hal_map::map(&self.device, buffer, self.backend, 0..size) }
            .map_err(|e| ExecError::Readback(e.to_string()))?;
        let mapped = MappedBuffer {
            ptr: mapping.ptr,
            size,
            is_coherent: mapping.is_coherent,
            backend: self.backend,
        };
        // SAFETY: same buffer and range were just mapped.
        unsafe {
            hal_map::invalidate(&self.device, buffer, self.backend, [0..size].into_iter())
        }
        .map_err(|e| ExecError::Readback(e.to_string()))?;

        let words: &[f32] = bytemuck::cast_slice(unsafe {
            std::slice::from_raw_parts(mapped.ptr.as_ptr(), size as usize)
        });
        let out = words[..n as usize].to_vec();

        // SAFETY: the buffer is still mapped and alive.
        unsafe {
            hal_map::unmap(&self.device, buffer, self.backend)
        }
        .map_err(|e| ExecError::Readback(e.to_string()))?;
        Ok(out)
    }
}

fn pipeline_key(kernel: &str, n: u32, value_count: usize, consts: &[ConstArg]) -> String {
    let mut parts = String::new();
    for c in consts {
        parts.push_str(&format!("{}={};", c.key, const_display(&c.value)));
    }
    format!("{kernel}#{n}#{value_count}#{parts}")
}

fn const_display(v: &neon_gpu_script::ConstValue) -> String {
    match v {
        neon_gpu_script::ConstValue::Number(x) => format!("{x}"),
        neon_gpu_script::ConstValue::Str(s) => s.clone(),
    }
}