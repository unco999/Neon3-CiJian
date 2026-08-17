//! Depth-tested world-space UI rendered by the sole WGPU runtime owner.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

#[cfg_attr(not(test), allow(dead_code))]
const WORLD_UI_SHADER: &str = r#"
struct Camera {
    view_projection: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) model_0: vec4<f32>,
    @location(2) model_1: vec4<f32>,
    @location(3) model_2: vec4<f32>,
    @location(4) model_3: vec4<f32>,
    @location(5) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(input.model_0, input.model_1, input.model_2, input.model_3);
    var output: VertexOutput;
    output.position = camera.view_projection * model * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.uv = input.position.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

#[cfg_attr(not(test), allow(dead_code))]
const WORLD_UI_TEXTURE_SHADER: &str = r#"
struct Camera { view_projection: mat4x4<f32>, }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var panel_texture: texture_2d<f32>;
@group(1) @binding(1) var panel_sampler: sampler;
struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) model_0: vec4<f32>,
    @location(2) model_1: vec4<f32>, @location(3) model_2: vec4<f32>,
    @location(4) model_3: vec4<f32>, @location(5) color: vec4<f32>,
}
struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32>, @location(1) uv: vec2<f32>, }
@vertex fn vs_main(input: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(input.model_0, input.model_1, input.model_2, input.model_3);
    var output: VertexOutput;
    output.position = camera.view_projection * model * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.uv = input.position.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return output;
}
@fragment fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(panel_texture, panel_sampler, input.uv) * input.color;
}
"#;

#[cfg_attr(not(test), allow(dead_code))]
const MAX_WORLD_UI_QUADS: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct WorldUiCamera {
    /// Column-major view-projection matrix supplied by the world camera.
    pub view_projection: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(crate) struct WorldUiQuad {
    /// Column-major world transform. The unit quad is centered at the origin.
    pub model: [[f32; 4]; 4],
    pub color: [f32; 4],
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct WorldUiPipeline {
    pipeline: wgpu::RenderPipeline,
    textured_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    quad_buffer: wgpu::Buffer,
    texture_layout: wgpu::BindGroupLayout,
}

#[cfg_attr(not(test), allow(dead_code))]
impl WorldUiPipeline {
    pub(crate) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-world-ui-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-world-ui-camera"),
            size: std::mem::size_of::<WorldUiCamera>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-world-ui-camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("neon3-world-ui-quad-vertices"),
            contents: bytemuck::cast_slice(&[
                [-1.0f32, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [-1.0f32, 1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
            ]),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let quad_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-world-ui-quads"),
            size: (std::mem::size_of::<WorldUiQuad>() * MAX_WORLD_UI_QUADS) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-world-ui-shader"),
            source: wgpu::ShaderSource::Wgsl(WORLD_UI_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-world-ui-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout)],
            immediate_size: 0,
        });
        let vertex_buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                }],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<WorldUiQuad>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 0,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 2,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 32,
                        shader_location: 3,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 48,
                        shader_location: 4,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 64,
                        shader_location: 5,
                    },
                ],
            }),
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-world-ui-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-world-ui-panel-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let textured_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-world-ui-texture-shader"),
            source: wgpu::ShaderSource::Wgsl(WORLD_UI_TEXTURE_SHADER.into()),
        });
        let textured_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-world-ui-texture-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let textured_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-world-ui-texture-pipeline"),
            layout: Some(&textured_layout),
            vertex: wgpu::VertexState {
                module: &textured_shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_buffers,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &textured_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            textured_pipeline,
            camera_buffer,
            camera_bind_group,
            vertex_buffer,
            quad_buffer,
            texture_layout,
        }
    }

    /// Samples a renderer-private normal UI surface in the shared world depth pass.
    pub(crate) fn render_surface(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        camera: WorldUiCamera,
        surface: &wgpu::TextureView,
        quad: WorldUiQuad,
    ) {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-world-ui-panel-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-world-ui-panel-bind-group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(surface),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        queue.write_buffer(&self.quad_buffer, 0, bytemuck::bytes_of(&quad));
        pass.set_pipeline(&self.textured_pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, self.quad_buffer.slice(..));
        pass.draw(0..6, 0..1);
    }

    /// Draws into the caller's pass; its depth attachment must be the graph's shared depth view.
    pub(crate) fn render(
        &self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        camera: WorldUiCamera,
        quads: &[WorldUiQuad],
    ) {
        if quads.is_empty() {
            return;
        }
        assert!(
            quads.len() <= MAX_WORLD_UI_QUADS,
            "world UI quad capacity exceeded"
        );
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&camera));
        queue.write_buffer(&self.quad_buffer, 0, bytemuck::cast_slice(quads));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, self.quad_buffer.slice(..));
        pass.draw(0..6, 0..quads.len() as u32);
    }

    /// Renders a deterministic depth-tested showcase without involving the window surface.
    pub(crate) fn capture_lab(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        panel_surface: &wgpu::TextureView,
    ) -> Result<Vec<u8>, String> {
        let [width, height] = size;
        if width == 0 || height == 0 {
            return Err("world UI lab size must be non-zero".into());
        }
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-lab-color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-lab-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let camera = WorldUiCamera {
            view_projection: perspective_matrix(width as f32 / height as f32),
        };
        let panel = |x, y, z, sx, sy, color| WorldUiQuad {
            model: lab_transform(x, y, z, sx, sy),
            color,
        };
        let quads = [
            panel(0.0, -0.02, -6.12, 2.25, 1.38, [0.01, 0.02, 0.05, 1.0]),
            panel(0.0, 0.0, -6.0, 2.18, 1.32, [0.10, 0.13, 0.20, 1.0]),
            panel(0.0, 0.92, -5.92, 2.08, 0.29, [0.13, 0.42, 0.72, 1.0]),
            panel(0.0, -0.17, -5.93, 2.08, 0.88, [0.15, 0.18, 0.26, 1.0]),
            panel(-0.55, 0.18, -5.84, 1.28, 0.16, [0.16, 0.82, 0.43, 1.0]),
            panel(0.78, 0.18, -5.84, 0.54, 0.16, [0.94, 0.64, 0.19, 1.0]),
            panel(-1.30, -0.78, -5.82, 0.42, 0.18, [0.22, 0.76, 0.92, 1.0]),
            panel(-0.38, -0.78, -5.82, 0.42, 0.18, [0.85, 0.30, 0.45, 1.0]),
            panel(0.54, -0.78, -5.82, 0.42, 0.18, [0.67, 0.45, 0.94, 1.0]),
            panel(0.95, -0.30, -5.66, 0.72, 0.55, [0.92, 0.24, 0.18, 1.0]),
        ];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-world-ui-lab-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-world-ui-lab-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.render(queue, &mut pass, camera, &quads);
            self.render_surface(
                device,
                queue,
                &mut pass,
                camera,
                panel_surface,
                WorldUiQuad {
                    model: lab_transform(0.0, 0.0, -5.75, 2.08, 1.14),
                    color: [1.0; 4],
                },
            );
        }
        let row_bytes = width
            .checked_mul(4)
            .ok_or_else(|| "world UI lab row size overflowed".to_owned())?;
        let padded_row_bytes = row_bytes
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .checked_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .ok_or_else(|| "world UI lab row alignment overflowed".to_owned())?;
        let readback_size = u64::from(padded_row_bytes)
            .checked_mul(u64::from(height))
            .ok_or_else(|| "world UI lab readback size overflowed".to_owned())?;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-world-ui-lab-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        queue.submit(Some(encoder.finish()));
        let (mapped_tx, mapped_rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|error| format!("wait for world UI lab readback: {error}"))?;
        mapped_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "world UI lab readback mapping timed out".to_owned())?
            .map_err(|error| format!("map world UI lab readback: {error}"))?;
        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(|error| format!("read world UI lab mapping: {error}"))?;
        let mut rgba = Vec::with_capacity(row_bytes as usize * height as usize);
        for row in mapped.chunks_exact(padded_row_bytes as usize) {
            rgba.extend_from_slice(&row[..row_bytes as usize]);
        }
        drop(mapped);
        readback.unmap();
        Ok(rgba)
    }
}

fn lab_transform(x: f32, y: f32, z: f32, sx: f32, sy: f32) -> [[f32; 4]; 4] {
    let (sin_y, cos_y) = (-0.16f32).sin_cos();
    let (sin_x, cos_x) = 0.05f32.sin_cos();
    [
        [sx * cos_y, 0.0, -sx * sin_y, 0.0],
        [sy * sin_x * sin_y, sy * cos_x, sy * sin_x * cos_y, 0.0],
        [0.0, 0.0, cos_x, 0.0],
        [x, y, z, 1.0],
    ]
}

fn perspective_matrix(aspect: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (35.0f32.to_radians() * 0.5).tan();
    let near = 0.1;
    let far = 20.0;
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, (far + near) / (near - far), -1.0],
        [0.0, 0.0, 2.0 * near * far / (near - far), 0.0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

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
            label: Some("neon3-world-ui-depth-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .expect("a device is required")
    }

    fn transform(x: f32, z: f32) -> [[f32; 4]; 4] {
        [
            [0.5, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [x, 0.0, z, 1.0],
        ]
    }

    #[test]
    fn world_ui_depth_hides_behind_quad_and_keeps_front_quad_visible() {
        let (device, queue) = test_device();
        let size = 32u32;
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-depth-test-color"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-depth-test-depth"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&Default::default());
        let depth_view = depth.create_view(&Default::default());
        let pipeline = WorldUiPipeline::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let camera = WorldUiCamera {
            view_projection: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        };
        let quads = [
            WorldUiQuad {
                model: transform(-0.5, 0.2),
                color: [1.0, 0.0, 0.0, 1.0],
            },
            WorldUiQuad {
                model: transform(-0.5, 0.7),
                color: [0.0, 1.0, 0.0, 1.0],
            },
            WorldUiQuad {
                model: transform(0.5, 0.1),
                color: [0.0, 0.0, 1.0, 1.0],
            },
        ];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-world-ui-depth-test-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-world-ui-depth-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pipeline.render(&queue, &mut pass, camera, &quads);
        }
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-world-ui-depth-test-readback"),
            size: (size * 256) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut copy = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-world-ui-depth-test-copy"),
        });
        copy.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(size),
                },
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish(), copy.finish()]);
        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap()
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let pixels = readback.slice(..).get_mapped_range().unwrap();
        let pixel = |x: usize| &pixels[(size as usize / 2 * 256 + x * 4)..][..4];
        assert_eq!(pixel(8), [255, 0, 0, 255]);
        assert_eq!(pixel(24), [0, 0, 255, 255]);
        assert!(
            !pixels
                .chunks_exact(4)
                .any(|pixel| pixel == [0, 255, 0, 255])
        );
    }
}
