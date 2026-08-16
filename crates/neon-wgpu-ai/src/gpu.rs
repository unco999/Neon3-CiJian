//! GPU context, pipeline cache, scratch-buffer arena and readback helpers.
//! This crate never creates a device itself; the caller (neon-wgpu-runtime)
//! owns the device and hands it in, preserving the single-GPU-owner rule.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wgpu::util::DeviceExt;

use crate::AiError;

pub const SHADER_ELEM: &str = include_str!("shaders/elem.wgsl");
pub const SHADER_CONV2D: &str = include_str!("shaders/conv2d.wgsl");
pub const SHADER_GROUPNORM: &str = include_str!("shaders/groupnorm.wgsl");
pub const SHADER_RESIZE: &str = include_str!("shaders/resize.wgsl");
pub const SHADER_MATMUL: &str = include_str!("shaders/matmul.wgsl");
pub const SHADER_SOFTMAX: &str = include_str!("shaders/softmax.wgsl");
pub const SHADER_RANDN: &str = include_str!("shaders/randn.wgsl");
pub const SHADER_COND: &str = include_str!("shaders/cond.wgsl");
pub const SHADER_TIMEFREQ: &str = include_str!("shaders/timefreq.wgsl");
pub const SHADER_TRANSPOSE: &str = include_str!("shaders/transpose.wgsl");

/// Scratch arena; buffers returned by `acquire` are returned to the pool when
/// the `Buf` is dropped. Not `Send`; each worker thread owns its `GpuCtx`.
struct ArenaInner {
    device: wgpu::Device,
    free: RefCell<HashMap<u64, Vec<wgpu::Buffer>>>,
}

#[derive(Clone)]
pub struct Arena(Rc<ArenaInner>);

impl Arena {
    fn new(device: &wgpu::Device) -> Self {
        Self(Rc::new(ArenaInner {
            device: device.clone(),
            free: RefCell::new(HashMap::new()),
        }))
    }

    fn acquire(&self, bytes: u64) -> wgpu::Buffer {
        let rounded = bytes.div_ceil(4096) * 4096;
        if let Some(slot) = self.0.free.borrow_mut().get_mut(&rounded)
            && let Some(buffer) = slot.pop()
        {
            return buffer;
        }
        self.0.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon-ai-scratch"),
            size: rounded,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    }

    fn release(&self, buffer: &wgpu::Buffer) {
        self.0
            .free
            .borrow_mut()
            .entry(buffer.size())
            .or_default()
            .push(buffer.clone());
    }
}

/// A scratch buffer with automatic return to the arena pool on drop.
pub struct Buf {
    pub buffer: wgpu::Buffer,
    arena: Arena,
    pooled: bool,
}

impl Buf {
    pub fn into_inner(mut self) -> wgpu::Buffer {
        self.pooled = false;
        self.buffer.clone()
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        if self.pooled {
            self.arena.release(&self.buffer);
        }
    }
}

/// Compute pipeline + bind group layout for one shader entry point.
struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

/// Device-owning compute context. `device` and `queue` are clones of the ones
/// created by neon-wgpu-runtime.
pub struct GpuCtx {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub arena: Arena,
    pipelines: HashMap<&'static str, Pipeline>,
    pending_encoder: Option<wgpu::CommandEncoder>,
    /// Zero buffer used to satisfy read-only bindings a shader does not use.
    dummy: wgpu::Buffer,
    /// Accumulated GPU submission time for diagnostics.
    pub elapsed_ms: f64,
    submission_count: u64,
    /// Total bytes of resident model weights (diagnostics).
    pub resident_bytes: u64,
}

impl GpuCtx {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let dummy = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon-ai-dummy"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let arena = Arena::new(&device);
        let mut ctx = Self {
            device,
            queue,
            arena,
            pipelines: HashMap::new(),
            pending_encoder: None,
            dummy,
            elapsed_ms: 0.0,
            submission_count: 0,
            resident_bytes: 0,
        };
        ctx.build_pipelines();
        ctx
    }

    fn pipeline(&self, key: &'static str) -> &Pipeline {
        self.pipelines.get(key).expect("pipeline must exist")
    }

    fn build_pipelines(&mut self) {
        macro_rules! add {
            ($key:literal, $src:expr, $entry:literal, $rw:expr, $bindings:expr) => {
                // Binding 0 is the uniform; `$rw` lists the read-write storage
                // bindings (indexes); every other storage binding is read-only,
                // matching the module declarations exactly (wgpu requires the
                // layout access flags to match the shader usage).
                let entries: Vec<wgpu::BindGroupLayoutEntry> = (0..$bindings)
                    .map(|binding| wgpu::BindGroupLayoutEntry {
                        binding,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: if binding == 0 {
                                wgpu::BufferBindingType::Uniform
                            } else {
                                wgpu::BufferBindingType::Storage {
                                    read_only: !$rw.contains(&binding),
                                }
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    })
                    .collect();
                let layout = self
                    .device
                        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                            label: None,
                            entries: &entries,
                        });
                let pipeline_layout = self
                    .device
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: None,
                            bind_group_layouts: &[Some(&layout)],
                            immediate_size: 0,
                        });
                let module = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some($key),
                        source: wgpu::ShaderSource::Wgsl($src.into()),
                    });
                let pipeline = self
                    .device
                        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                            label: Some($key),
                            layout: Some(&pipeline_layout),
                            module: &module,
                            entry_point: Some($entry),
                            compilation_options: Default::default(),
                            cache: None,
                        });
                self.pipelines.insert(
                    $key,
                    Pipeline {
                        pipeline,
                        layout,
                    },
                );
            };
        }
        // bindings: uniform(0) + storage(1..n). `&[..]` lists the read-write storage bindings.
        add!("silu", SHADER_ELEM, "main_silu", &[4], 5);
        add!("add", SHADER_ELEM, "main_add", &[4], 5);
        add!("mul", SHADER_ELEM, "main_mul", &[4], 5);
        add!("cfg", SHADER_ELEM, "main_cfg", &[4], 5);
        add!("ddim", SHADER_ELEM, "main_ddim", &[4], 5);
        add!("film", SHADER_ELEM, "main_film", &[4], 5);
        add!("transpose", SHADER_TRANSPOSE, "main", &[2], 3);
        add!("conv2d", SHADER_CONV2D, "main", &[4], 5);
        add!("gn_reduce", SHADER_GROUPNORM, "main_reduce", &[4], 7);
        add!("gn_finalize", SHADER_GROUPNORM, "main_finalize", &[4, 5], 7);
        add!("gn_apply", SHADER_GROUPNORM, "main_apply", &[5, 6], 7);
        add!("avgpool", SHADER_RESIZE, "main_avgpool", &[3], 4);
        add!("upsample", SHADER_RESIZE, "main_upsample", &[3], 4);
        add!("concat", SHADER_RESIZE, "main_concat", &[3], 4);
        add!("matmul", SHADER_MATMUL, "main", &[3], 4);
        add!("softmax", SHADER_SOFTMAX, "main", &[1], 2);
        add!("randn", SHADER_RANDN, "main", &[1], 2);
        add!("timefreq", SHADER_TIMEFREQ, "main_timefreq", &[1], 2);
        add!("gather", SHADER_COND, "main_gather", &[4], 5);
    }

    /// Create a uniform buffer from a POD value.
    pub fn uniform<T: bytemuck::Pod>(&self, value: &T) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("neon-ai-uniform"),
                contents: bytemuck::bytes_of(value),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// Dispatch one compute shader. `uniform` binds at 0; `inputs` bind at
    /// 1..=n in order. The pipeline's `rw` binding must be present in inputs
    /// and is bound as read-write.
    pub fn run(
        &mut self,
        key: &'static str,
        uniform: &wgpu::Buffer,
        inputs: &[&wgpu::Buffer],
        workgroups: [u32; 3],
    ) {
        let (compute_pipeline, layout) = {
            let pipeline = self.pipeline(key);
            (pipeline.pipeline.clone(), pipeline.layout.clone())
        };
        let mut entries = Vec::with_capacity(inputs.len() + 1);
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        });
        let mut used = 0usize;
        for binding in 1..=inputs.len() as u32 {
            let buffer = inputs.get(used).copied().unwrap_or(&self.dummy);
            entries.push(wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            });
            used += 1;
        }
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &entries,
        });
        let started = std::time::Instant::now();
        if let Some(encoder) = self.pending_encoder.as_mut() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(key),
                timestamp_writes: None,
            });
            pass.set_pipeline(&compute_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        } else {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(key),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&compute_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
            }
            self.queue.submit(Some(encoder.finish()));
            self.submission_count += 1;
        }
        self.elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
    }

    /// Record subsequent compute passes into one command encoder. The caller
    /// must submit or discard the batch before readback or waiting.
    pub fn begin_batch(&mut self) {
        assert!(self.pending_encoder.is_none(), "GPU compute batch already active");
        self.pending_encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("neon-ai-ddim-step"),
                }),
        );
    }

    /// Submit the active compute batch, if any.
    pub fn submit_batch(&mut self) {
        if let Some(encoder) = self.pending_encoder.take() {
            let started = std::time::Instant::now();
            self.queue.submit(Some(encoder.finish()));
            self.submission_count += 1;
            self.elapsed_ms += started.elapsed().as_secs_f64() * 1000.0;
        }
    }

    /// Drop an incomplete batch after an error without submitting partial work.
    pub fn discard_batch(&mut self) {
        self.pending_encoder = None;
    }

    pub fn submission_count(&self) -> u64 {
        self.submission_count
    }

    /// Wait for all queued inference work once at a public operation boundary.
    pub fn wait(&self) -> Result<(), AiError> {
        assert!(self.pending_encoder.is_none(), "cannot wait with an active GPU compute batch");
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| AiError::Gpu(format!("device poll failed: {error}")))?;
        Ok(())
    }

    /// Acquire a scratch buffer of at least `bytes`.
    pub fn scratch(&self, bytes: u64) -> Buf {
        Buf {
            buffer: self.arena.acquire(bytes),
            arena: self.arena.clone(),
            pooled: true,
        }
    }

    /// Upload raw bytes as a storage buffer (model weights).
    pub fn upload(&mut self, bytes: &[u8], label: &str) -> wgpu::Buffer {
        self.resident_bytes += bytes.len() as u64;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
            })
    }

    /// Synchronous buffer readback (copy + map + poll). Diagnostic/test path.
    pub fn readback(&self, buffer: &wgpu::Buffer, bytes: u64) -> Result<Vec<u8>, AiError> {
        assert!(self.pending_encoder.is_none(), "cannot read back with an active GPU compute batch");
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon-ai-staging"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, bytes);
        self.queue.submit(Some(encoder.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| AiError::Gpu(format!("device poll failed: {error}")))?;
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| AiError::Gpu("readback timed out".into()))?
            .map_err(|error| AiError::Gpu(format!("readback map failed: {error}")))?;
        let data = slice
            .get_mapped_range()
            .map_err(|error| AiError::Gpu(format!("mapped range failed: {error}")))?
            .to_vec();
        staging.unmap();
        Ok(data)
    }

    /// Read a GPU f32 buffer back into a Vec<f32>. Diagnostic/test path.
    pub fn readback_f32(&self, buffer: &wgpu::Buffer, count: usize) -> Result<Vec<f32>, AiError> {
        let bytes = self.readback(buffer, count as u64 * 4)?;
        if bytes.len() != count * 4 {
            return Err(AiError::Gpu("readback size mismatch".into()));
        }
        let mut out = Vec::with_capacity(count);
        for chunk in bytes.chunks_exact(4) {
            out.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        Ok(out)
    }
}
