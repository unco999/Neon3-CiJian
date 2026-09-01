//! GPU ECS runtime context: buffer management, pipelines and frame passes.
//!
//! `GpuEcsCtx` never creates a `wgpu::Instance`, adapter or device. The sole
//! caller (`neon-wgpu-runtime`) injects its device/queue clones. The context
//! allocates every SoA and sorting buffer, builds one shader module with all
//! entry points, and records the sorting / system passes.

pub mod init;

use crate::generator::{self, bind_layout};
use crate::ir::EcsIr;
use crate::EcsError;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

/// The injected, fully initialised world context.
pub struct GpuEcsCtx {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub ir: EcsIr,
    /// Entity table capacity (all buffers are sized for this).
    pub max_entities: u32,
    /// Structural command ring capacity.
    pub command_capacity: u32,

    shader_module: wgpu::ShaderModule,
    bind_group_layouts: [wgpu::BindGroupLayout; 2],
    /// One group 0 per command ring; index 0/1 select the write ring.
    pub bind_group0: [wgpu::BindGroup; 2],
    pub bind_group1: wgpu::BindGroup,
    pipelines: HashMap<String, wgpu::ComputePipeline>,

    pub entity_active: wgpu::Buffer,
    pub query_counts: wgpu::Buffer,
    pub query_cursors: wgpu::Buffer,
    pub frame_prep: wgpu::Buffer,
    pub compacted_ids: wgpu::Buffer,
    /// Written by the scan kernel as STORAGE.
    pub indirect_args: wgpu::Buffer,
    /// Dedicated INDIRECT-only copy of `indirect_args`; indirect dispatch must
    /// not read a buffer that is also bound as STORAGE in the same pass.
    pub indirect_exec: wgpu::Buffer,
    /// Ping-pong structural command rings. Frame N's systems write
    /// `cmd_buffers[phase]`; at the start of the NEXT frame that ring is read
    /// back, replayed on the CPU and its count reset. Two rings mean the ring
    /// being read back is never the one the current frame writes.
    cmd_buffers: [wgpu::Buffer; 2],
    cmd_counts: [wgpu::Buffer; 2],
    /// Which ring the current frame's systems write into.
    cmd_phase: Cell<usize>,
    /// Frames executed so far (0 = nothing to replay yet).
    frame_count: Cell<u64>,
    /// CPU truth of entity activity, mirroring `entityActive` plus replayed
    /// structural changes (used for spawn slot allocation).
    active_entities: RefCell<Vec<bool>>,
    /// Free entity slots (deleted entities), lowest first.
    free_slots: RefCell<VecDeque<u32>>,
    /// Per component: (data, current version, baseline version).
    pub component_buffers: Vec<(wgpu::Buffer, wgpu::Buffer, wgpu::Buffer)>,
    /// Per resource id: uniform buffer.
    pub resource_buffers: Vec<wgpu::Buffer>,
    pub render_instances: wgpu::Buffer,
}

impl GpuEcsCtx {
    /// Validate the IR, generate the shader, check device limits and allocate
    /// everything. Fails fast with [`EcsError::Limits`] when the device
    /// cannot hold the group 0 bindings.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        ir: EcsIr,
        max_entities: u32,
        command_capacity: u32,
    ) -> Result<Self, EcsError> {
        ir.validate()?;
        generator::check_schedule_conflicts(&ir)?;
        generator::check_limits(&ir, device.limits().max_storage_buffers_per_shader_stage)?;

        let wgsl = generator::generate_wgsl(&ir)?;
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon-gpu-ecs world module"),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
        });

        // ---- bind group layouts -------------------------------------------
        let n_group0 = bind_layout::group0_storage_bindings(ir.components.len() as u32);
        let group0_entries: Vec<wgpu::BindGroupLayoutEntry> = (0..n_group0)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ecs group 0"),
            entries: &group0_entries,
        });

        // Group 1: uniform resources at their slots + renderInstances at 30.
        let mut group1_entries: Vec<wgpu::BindGroupLayoutEntry> = ir
            .resources
            .iter()
            .map(|res| wgpu::BindGroupLayoutEntry {
                binding: res.binding_slot,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        group1_entries.push(wgpu::BindGroupLayoutEntry {
            binding: bind_layout::RENDER_INSTANCES_BINDING,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        let layout1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ecs group 1"),
            entries: &group1_entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ecs pipeline layout"),
            bind_group_layouts: &[Some(&layout0), Some(&layout1)],
            immediate_size: 0,
        });

        // ---- pipelines -----------------------------------------------------
        let mut entry_points = vec![
            "system_prep_count".to_string(),
            "system_prep_scan".to_string(),
            "system_prep_fill".to_string(),
        ];
        for system in &ir.systems {
            entry_points.push(format!("system_{}", system.name));
        }
        let mut pipelines = HashMap::new();
        for entry in entry_points {
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&format!("ecs {entry}")),
                layout: Some(&pipeline_layout),
                module: &shader_module,
                entry_point: Some(&entry),
                compilation_options: Default::default(),
                cache: None,
            });
            pipelines.insert(entry, pipeline);
        }

        // ---- buffers -------------------------------------------------------
        let n_queries = ir.queries.len() as u64;
        let me = max_entities as u64;
        let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;

        let entity_active = create_buffer(&device, "ecs entityActive", me * 4, storage);
        let query_counts = create_buffer(&device, "ecs queryCounts", n_queries * 4, storage);
        let query_cursors = create_buffer(&device, "ecs queryCursors", n_queries * 4, storage);
        let frame_prep = create_buffer(&device, "ecs framePrepBuffer", n_queries * 8, storage);
        // Compacted ids hold the concatenation of ALL queries' matches, so
        // capacity is max_entities * n_queries (queries overlap on entities).
        let compacted_ids = create_buffer(
            &device,
            "ecs compactedEntityIds",
            me * 4 * n_queries.max(1),
            storage,
        );
        // `array<vec3u>` has a 16-byte stride (vec3 alignment).
        let indirect_args = create_buffer(
            &device,
            "ecs indirectArgs",
            n_queries * 16,
            storage,
        );
        // Dedicated indirect-dispatch source. Indirect dispatch and a STORAGE
        // binding of the same buffer cannot share one compute dispatch's usage
        // scope, so the scan result is copied here and systems dispatch from it.
        let indirect_exec = create_buffer(
            &device,
            "ecs indirectExec",
            n_queries * 16,
            wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
        );
        // Structural command rings: ping-pong pair. Systems write one ring per
        // frame; at the next frame start the ring is read back (via staging
        // copies + map_async), replayed on the CPU and its count reset.
        let cmd_usage = storage;
        let cmd_buffers = [
            create_buffer(&device, "ecs commandBuffer A", command_capacity as u64 * 16, cmd_usage),
            create_buffer(&device, "ecs commandBuffer B", command_capacity as u64 * 16, cmd_usage),
        ];
        let cmd_counts = [
            create_buffer(&device, "ecs commandCount A", 4, cmd_usage),
            create_buffer(&device, "ecs commandCount B", 4, cmd_usage),
        ];

        let mut component_buffers = Vec::new();
        for comp in &ir.components {
            let stride = comp.ty.wgsl_array_stride() as u64;
            let data = create_buffer(&device, &format!("ecs c_{}", comp.name), me * stride, storage);
            // Version buffers are rewritten by structural replay (write_buffer).
            let version_usage = storage;
            let version = create_buffer(&device, &format!("ecs cv_{}", comp.name), me * 4, version_usage);
            let baseline = create_buffer(&device, &format!("ecs cb_{}", comp.name), me * 4, version_usage);
            component_buffers.push((data, version, baseline));
        }

        let mut resource_buffers = Vec::new();
        for res in &ir.resources {
            let buf = create_buffer(
                &device,
                &format!("ecs r_{}", res.name),
                res.ty.byte_size() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            );
            resource_buffers.push(buf);
        }

        let render_instances = create_buffer(
            &device,
            "ecs renderInstances",
            me * 32,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );

        // ---- bind groups ----------------------------------------------------
        // One group 0 per command ring; the only difference is the command
        // buffer/count bindings.
        let mut bind_group0 = Vec::with_capacity(2);
        for ring in 0..2usize {
            let mut group0_bindings: Vec<wgpu::BindGroupEntry> =
                Vec::with_capacity(8 + ir.components.len() * 3);
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::ENTITY_ACTIVE_BINDING, resource: entity_active.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::QUERY_COUNTS_BINDING, resource: query_counts.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::QUERY_CURSORS_BINDING, resource: query_cursors.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::FRAME_PREP_BINDING, resource: frame_prep.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::COMPACTED_IDS_BINDING, resource: compacted_ids.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::INDIRECT_ARGS_BINDING, resource: indirect_args.as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::COMMAND_BUFFER_BINDING, resource: cmd_buffers[ring].as_entire_binding() });
            group0_bindings.push(wgpu::BindGroupEntry { binding: bind_layout::COMMAND_COUNT_BINDING, resource: cmd_counts[ring].as_entire_binding() });
            for (id, (data, version, baseline)) in component_buffers.iter().enumerate() {
                group0_bindings.push(wgpu::BindGroupEntry {
                    binding: bind_layout::component_data_binding(id as u32),
                    resource: data.as_entire_binding(),
                });
                group0_bindings.push(wgpu::BindGroupEntry {
                    binding: bind_layout::component_version_binding(id as u32),
                    resource: version.as_entire_binding(),
                });
                group0_bindings.push(wgpu::BindGroupEntry {
                    binding: bind_layout::component_baseline_binding(id as u32),
                    resource: baseline.as_entire_binding(),
                });
            }
            bind_group0.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("ecs group 0 ring {ring}")),
                layout: &layout0,
                entries: &group0_bindings,
            }));
        }
        let bind_group0: [wgpu::BindGroup; 2] = bind_group0.try_into().unwrap();

        let mut group1_bindings: Vec<wgpu::BindGroupEntry> = ir
            .resources
            .iter()
            .zip(resource_buffers.iter())
            .map(|(res, buf)| wgpu::BindGroupEntry {
                binding: res.binding_slot,
                resource: buf.as_entire_binding(),
            })
            .collect();
        group1_bindings.push(wgpu::BindGroupEntry {
            binding: bind_layout::RENDER_INSTANCES_BINDING,
            resource: render_instances.as_entire_binding(),
        });
        let bind_group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ecs group 1 bind"),
            layout: &layout1,
            entries: &group1_bindings,
        });

        Ok(Self {
            device,
            queue,
            ir,
            max_entities,
            command_capacity,
            shader_module,
            bind_group_layouts: [layout0, layout1],
            bind_group0,
            bind_group1,
            pipelines,
            entity_active,
            query_counts,
            query_cursors,
            frame_prep,
            compacted_ids,
            indirect_args,
            indirect_exec,
            cmd_buffers,
            cmd_counts,
            cmd_phase: Cell::new(0),
            frame_count: Cell::new(0),
            active_entities: RefCell::new(vec![false; max_entities as usize]),
            free_slots: RefCell::new(VecDeque::new()),
            component_buffers,
            resource_buffers,
            render_instances,
        })
    }

    /// Upload the initial prototype population, version seeds (baseline ==
    /// current) and resource defaults. Call once before the first frame.
    pub fn seed_initial(&self) {
        self.queue
            .write_buffer(&self.entity_active, 0, &init::initial_entity_active(&self.ir, self.max_entities));
        for (id, (data, version, baseline)) in self.component_buffers.iter().enumerate() {
            let bytes = init::initial_component_bytes(&self.ir, id as u32, self.max_entities);
            self.queue.write_buffer(data, 0, &bytes);
            let versions = init::initial_version_bytes(&self.ir, id as u32, self.max_entities);
            self.queue.write_buffer(version, 0, &versions);
            self.queue.write_buffer(baseline, 0, &versions);
        }
        for (res, buf) in self.ir.resources.iter().zip(self.resource_buffers.iter()) {
            self.queue.write_buffer(buf, 0, &res.default_value);
        }
        // CPU-side entity bookkeeping mirrors the GPU seed.
        let total = init::prototype_entity_total(&self.ir);
        let mut active = self.active_entities.borrow_mut();
        for e in 0..total as usize {
            active[e] = true;
        }
        let mut free = self.free_slots.borrow_mut();
        free.clear();
        for e in total..self.max_entities {
            free.push_back(e);
        }
    }

    fn pipeline(&self, entry: &str) -> &wgpu::ComputePipeline {
        self.pipelines
            .get(entry)
            .unwrap_or_else(|| panic!("pipeline '{entry}' must exist"))
    }

    /// Record the count → scan → fill passes into an existing encoder.
    fn record_sort_pass(&self, encoder: &mut wgpu::CommandEncoder) {
        let workgroups = self.max_entities.div_ceil(64);
        let ring = self.cmd_phase.get();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("ecs sort pass"),
            timestamp_writes: None,
        });
        pass.set_bind_group(0, &self.bind_group0[ring], &[]);
        pass.set_bind_group(1, &self.bind_group1, &[]);
        pass.set_pipeline(self.pipeline("system_prep_count"));
        pass.dispatch_workgroups(workgroups, 1, 1);
        pass.set_pipeline(self.pipeline("system_prep_scan"));
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(self.pipeline("system_prep_fill"));
        pass.dispatch_workgroups(workgroups, 1, 1);
    }

    /// Record and submit one sorting pass: count → scan → fill.
    /// Dispatches are ordered within one compute pass, so the implicit
    /// barriers between them hold.
    pub fn run_sort(&self) {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ecs sort"),
        });
        self.record_sort_pass(&mut encoder);
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Upload new bytes for a uniform resource (e.g. `DeltaTime`) before the
    /// next [`Self::run_frame`].
    ///
    /// `write_buffer` stages the data; when a resource must be visible to the
    /// very next submission (e.g. right after `seed_initial`), call
    /// [`Self::flush`] once so the staged write lands before any dispatch
    /// reads it.
    pub fn set_resource(&self, resource_id: u32, bytes: &[u8]) {
        let res = &self.ir.resources[resource_id as usize];
        assert_eq!(
            bytes.len(),
            res.ty.byte_size(),
            "resource '{}' expects {} bytes",
            res.name,
            res.ty.byte_size()
        );
        self.queue
            .write_buffer(&self.resource_buffers[resource_id as usize], 0, bytes);
    }

    /// Submit an empty batch so pending `write_buffer` staging data is
    /// flushed to the buffers before the next real submission reads them.
    pub fn flush(&self) {
        let encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ecs flush"),
        });
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Execute one full frame:
    ///
    /// 1. replay the structural commands recorded in the previous frame's
    ///    ring (readback + CPU execution + ring count reset),
    /// 2. sorting pass over the freshly updated world (compares current
    ///    versions against the baseline left by the previous frame),
    /// 3. baseline snapshot (`current → baseline`),
    /// 4. copy the scan's indirect args into the dedicated INDIRECT-source
    ///    buffer, then systems, stage by stage, dispatched indirectly;
    ///    structural calls append to the current frame's ring.
    ///
    /// The ping-pong rings guarantee the ring being replayed is never the one
    /// the current frame writes. `Changed(c)` means "written since the
    /// previous frame's sorting point", `Added(c)` "first present since then".
    pub fn run_frame(&self) {
        // Step 1: replay the ring written by the previous frame.
        if self.frame_count.get() > 0 {
            let replay_ring = 1 - self.cmd_phase.get();
            self.replay_commands(replay_ring);
        }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ecs frame"),
        });
        self.record_sort_pass(&mut encoder);

        // Baseline snapshot: copy current version numbers to baselines.
        let version_bytes = self.max_entities as u64 * 4;
        for (_data, version, baseline) in &self.component_buffers {
            encoder.copy_buffer_to_buffer(version, 0, baseline, 0, version_bytes);
        }

        // Move the scan result into the dedicated INDIRECT-source buffer so
        // the indirect dispatch never shares a usage scope with a STORAGE
        // binding of the same buffer.
        let n_queries = self.ir.queries.len() as u64;
        encoder.copy_buffer_to_buffer(&self.indirect_args, 0, &self.indirect_exec, 0, n_queries * 16);

        // Systems, stage by stage.
        let ring = self.cmd_phase.get();
        for stage in &self.ir.schedule.stages {
            for sid in &stage.system_ids {
                let system = &self.ir.systems[*sid as usize];
                let entry = format!("system_{}", system.name);
                let offset = 16u64 * system.query_id as u64;
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(&entry),
                    timestamp_writes: None,
                });
                pass.set_bind_group(0, &self.bind_group0[ring], &[]);
                pass.set_bind_group(1, &self.bind_group1, &[]);
                pass.set_pipeline(self.pipeline(&entry));
                pass.dispatch_workgroups_indirect(&self.indirect_exec, offset);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        // The ring just written gets replayed at the start of the next frame.
        self.cmd_phase.set(1 - ring);
        self.frame_count.set(self.frame_count.get() + 1);
    }

    /// Read back one command ring, replay every structural change on the CPU,
    /// reset the ring count and clear the GPU-side record. Returns the number
    /// of commands replayed. Spawns with no free slot are dropped (the world
    /// is at `max_entities` capacity).
    fn replay_commands(&self, ring: usize) -> u32 {
        // Command count for this ring.
        let count_bytes = self.read_buffer_blocking(&self.cmd_counts[ring], 4);
        let count = u32::from_le_bytes(count_bytes[0..4].try_into().unwrap());
        let count = count.min(self.command_capacity);

        let mut replayed = 0u32;
        if count > 0 {
            let bytes = self.read_buffer_blocking(&self.cmd_buffers[ring], count as usize * 16);
            for chunk in bytes.chunks_exact(16) {
                let cmd = *bytemuck::from_bytes::<bind_layout::StructuralCommand>(chunk);
                self.apply_structural_command(cmd);
                replayed += 1;
            }
        }

        // Reset the ring for its next use: count back to zero. Slot contents
        // are gated by the count, so they need no clearing.
        self.queue
            .write_buffer(&self.cmd_counts[ring], 0, &0u32.to_le_bytes());
        self.flush();
        replayed
    }

    /// Apply one structural change to the GPU buffers and the CPU bookkeeping.
    fn apply_structural_command(&self, cmd: bind_layout::StructuralCommand) {
        match cmd.kind {
            bind_layout::COMMAND_KIND_SPAWN => self.replay_spawn(cmd.a),
            bind_layout::COMMAND_KIND_DELETE => self.replay_delete(cmd.a),
            bind_layout::COMMAND_KIND_ADD_COMPONENT => {
                self.replay_set_component_version(cmd.a, cmd.b, true);
            }
            bind_layout::COMMAND_KIND_REMOVE_COMPONENT => {
                self.replay_set_component_version(cmd.a, cmd.b, false);
            }
            _ => {}
        }
    }

    /// Spawn: allocate a free slot, activate it and write the prototype's
    /// initial component values (versions set to 1).
    fn replay_spawn(&self, prototype_index: u32) {
        let Some(proto) = self.ir.initial_entities.get(prototype_index as usize) else {
            return;
        };
        let Some(slot) = self.free_slots.borrow_mut().pop_front() else {
            // World at capacity: drop the spawn (documented overflow policy).
            return;
        };

        // Activate.
        self.queue
            .write_buffer(&self.entity_active, slot as u64 * 4, &1u32.to_le_bytes());

        // Component values + versions.
        for (pos, cid) in proto.component_ids.iter().enumerate() {
            let comp = &self.ir.components[*cid as usize];
            let value = match &proto.initial_values {
                Some(values) => values[pos].as_slice(),
                None => comp.default_value.as_slice(),
            };
            let stride = comp.ty.wgsl_array_stride() as u64;
            self.queue.write_buffer(
                &self.component_buffers[*cid as usize].0,
                slot as u64 * stride,
                value,
            );
            self.queue
                .write_buffer(&self.component_buffers[*cid as usize].1, slot as u64 * 4, &1u32.to_le_bytes());
            // Baseline starts equal so the fresh entity is not flagged
            // Changed until a system actually writes it.
            self.queue
                .write_buffer(&self.component_buffers[*cid as usize].2, slot as u64 * 4, &1u32.to_le_bytes());
        }

        self.active_entities.borrow_mut()[slot as usize] = true;
    }

    /// Delete: deactivate and clear every component version (so no query can
    /// match the slot and no stale Changed fires after a respawn).
    fn replay_delete(&self, entity: u32) {
        if entity >= self.max_entities || !self.active_entities.borrow()[entity as usize] {
            return;
        }
        self.queue
            .write_buffer(&self.entity_active, entity as u64 * 4, &0u32.to_le_bytes());
        for (_data, version, baseline) in &self.component_buffers {
            self.queue
                .write_buffer(version, entity as u64 * 4, &0u32.to_le_bytes());
            self.queue
                .write_buffer(baseline, entity as u64 * 4, &0u32.to_le_bytes());
        }
        self.active_entities.borrow_mut()[entity as usize] = false;
        self.free_slots.borrow_mut().push_back(entity);
    }

    /// Add/remove one component on an entity: version 1 (with default data)
    /// or 0.
    fn replay_set_component_version(&self, entity: u32, component_id: u32, present: bool) {
        if entity >= self.max_entities || component_id as usize >= self.ir.components.len() {
            return;
        }
        if !self.active_entities.borrow()[entity as usize] {
            return;
        }
        let comp = &self.ir.components[component_id as usize];
        if present {
            let stride = comp.ty.wgsl_array_stride() as u64;
            self.queue.write_buffer(
                &self.component_buffers[component_id as usize].0,
                entity as u64 * stride,
                &comp.default_value,
            );
        }
        let word = if present { 1u32 } else { 0u32 };
        self.queue
            .write_buffer(&self.component_buffers[component_id as usize].1, entity as u64 * 4, &word.to_le_bytes());
        self.queue
            .write_buffer(&self.component_buffers[component_id as usize].2, entity as u64 * 4, &word.to_le_bytes());
    }

    /// Blocking readback of a buffer's full contents into a fresh Vec.
    pub fn read_buffer_blocking(&self, buffer: &wgpu::Buffer, size: usize) -> Vec<u8> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ecs staging"),
            size: size as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ecs readback"),
        });
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size as u64);
        self.queue.submit(std::iter::once(encoder.finish()));

        let (tx, rx) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("device poll must succeed during readback");
        rx.recv()
            .expect("readback channel must not close")
            .expect("readback mapping must succeed");
        let view = staging
            .slice(..)
            .get_mapped_range()
            .expect("staging buffer must be mapped after map_async success");
        let bytes = view.to_vec();
        drop(view);
        staging.unmap();
        bytes
    }

    /// Read back a component's full data buffer (with WGSL stride padding).
    pub fn read_component_data(&self, component_id: u32) -> Vec<u8> {
        let stride = self.ir.components[component_id as usize].ty.wgsl_array_stride();
        self.read_buffer_blocking(
            &self.component_buffers[component_id as usize].0,
            self.max_entities as usize * stride,
        )
    }

    /// Read back a component's current version buffer.
    pub fn read_component_versions(&self, component_id: u32) -> Vec<u32> {
        let bytes = self.read_buffer_blocking(
            &self.component_buffers[component_id as usize].1,
            self.max_entities as usize * 4,
        );
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }
    /// Read back the per-query `{start, count}` table.
    pub fn read_frame_prep(&self) -> Vec<bind_layout::QueryRange> {
        let bytes = self.read_buffer_blocking(&self.frame_prep, self.ir.queries.len() * 8);
        bytes
            .chunks_exact(8)
            .map(|c| bind_layout::QueryRange {
                start: u32::from_le_bytes(c[0..4].try_into().unwrap()),
                count: u32::from_le_bytes(c[4..8].try_into().unwrap()),
            })
            .collect()
    }

    /// Read back the compacted entity id list.
    pub fn read_compacted_ids(&self) -> Vec<u32> {
        let n_queries = self.ir.queries.len().max(1);
        let bytes = self.read_buffer_blocking(
            &self.compacted_ids,
            self.max_entities as usize * 4 * n_queries,
        );
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    /// Read back the indirect dispatch arguments (workgroup x per query).
    ///
    /// WGSL `array<vec3u>` elements are 16-byte aligned, so each slot
    /// occupies 16 bytes (12 bytes of data + 4 padding). Dispatch offsets
    /// must use the same stride: `dispatch_workgroups_indirect(buf, 16 * q)`.
    pub fn read_indirect_args(&self) -> Vec<[u32; 3]> {
        let bytes = self.read_buffer_blocking(&self.indirect_args, self.ir.queries.len() * 16);
        bytes
            .chunks_exact(16)
            .map(|c| {
                [
                    u32::from_le_bytes(c[0..4].try_into().unwrap()),
                    u32::from_le_bytes(c[4..8].try_into().unwrap()),
                    u32::from_le_bytes(c[8..12].try_into().unwrap()),
                ]
            })
            .collect()
    }

    /// Expose the shader module for diagnostics.
    pub fn shader_module(&self) -> &wgpu::ShaderModule {
        &self.shader_module
    }

    /// Expose layouts for diagnostics.
    pub fn bind_group_layouts(&self) -> &[wgpu::BindGroupLayout; 2] {
        &self.bind_group_layouts
    }
}

fn create_buffer(device: &wgpu::Device, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(4), // WebGPU forbids zero-sized buffers
        usage,
        mapped_at_creation: false,
    })
}
