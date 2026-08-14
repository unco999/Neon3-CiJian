//! GPU-only conversion from a resident height buffer to a sampled UI preview texture.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const HEIGHTMAP_PREVIEW_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    minimum: f32,
    inverse_range: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> heights: array<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.width || gid.y >= p.height) { return; }
    let index = gid.y * p.width + gid.x;
    let value = clamp((heights[index] - p.minimum) * p.inverse_range, 0.0, 1.0);
    textureStore(output, vec2<i32>(gid.xy), vec4<f32>(value, value, value, 1.0));
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PreviewParams {
    width: u32,
    height: u32,
    minimum: f32,
    inverse_range: f32,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct HeightmapPreviewConverter {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

#[cfg_attr(not(test), allow(dead_code))]
impl HeightmapPreviewConverter {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-heightmap-preview-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-heightmap-preview-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-heightmap-preview-shader"),
            source: wgpu::ShaderSource::Wgsl(HEIGHTMAP_PREVIEW_SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("neon3-heightmap-preview-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, bind_group_layout }
    }

    /// Converts the model's clamped `[-3, 3]` latent directly into a sampled texture.
    pub(crate) fn create_texture(&self, device: &wgpu::Device, size: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-heightmap-preview-texture"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    pub(crate) fn convert_into(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        heightmap: &wgpu::Buffer,
        size: u32,
        output: &wgpu::TextureView,
    ) {
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("neon3-heightmap-preview-params"),
            contents: bytemuck::bytes_of(&PreviewParams {
                width: size,
                height: size,
                minimum: -3.0,
                inverse_range: 1.0 / 6.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-heightmap-preview-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: heightmap.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(output),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-heightmap-preview-encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("neon3-heightmap-preview-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(size.div_ceil(8), size.div_ceil(8), 1);
        }
        queue.submit(Some(encoder.finish()));
    }

    pub(crate) fn convert(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        heightmap: &wgpu::Buffer,
        size: u32,
    ) -> wgpu::Texture {
        let texture = self.create_texture(device, size);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.convert_into(device, queue, heightmap, size, &view);
        texture
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("a headless adapter is required");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-heightmap-preview-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .expect("a device is required")
    }

    #[test]
    fn height_buffer_converts_to_rgba_texture_without_cpu_upload() {
        let (device, queue) = test_device();
        let size = 64u32;
        let heights = (0..size * size)
            .map(|index| -3.0 + 6.0 * (index % size) as f32 / (size - 1) as f32)
            .collect::<Vec<_>>();
        let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("neon3-heightmap-preview-test-source"),
            contents: bytemuck::cast_slice(&heights),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let converter = HeightmapPreviewConverter::new(&device);
        let texture = converter.convert(&device, &queue, &source, size);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-heightmap-preview-test-readback"),
            size: (size * size * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-heightmap-preview-test-copy"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 4),
                    rows_per_image: Some(size),
                },
            },
            wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let pixels = readback.slice(..).get_mapped_range().unwrap();
        let pixel = |x: usize| &pixels[x * 4..x * 4 + 4];
        assert_eq!(pixel(0), [0, 0, 0, 255]);
        assert!(pixel(32)[0] >= 128 && pixel(32)[0] <= 130);
        assert_eq!(pixel(63), [255, 255, 255, 255]);
    }
}
