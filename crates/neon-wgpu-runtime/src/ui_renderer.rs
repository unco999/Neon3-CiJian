//! Minimal GPU UI composition pass adapted from Neon2's instanced panel renderer.
//! It deliberately consumes only Neon3's public UI schema, not old ECS state.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bytemuck::{Pod, Zeroable};
use neon_protocol::{AssetBytes, AssetRef};
use neon_ui_schema::{TextRef, UiBounds, UiEasing, UiFragment, UiLayoutMode, UiNode, UiNodeKind, UiStyle, UiTransition};

const SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;

struct VsIn {
    @location(0) rect: vec4<f32>,
    @location(1) fill: vec4<f32>,
    @location(2) border: vec4<f32>,
    @location(3) params: vec4<f32>,
    @location(4) clip: vec4<f32>,
}

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) fill: vec4<f32>,
    @location(3) border: vec4<f32>,
    @location(4) params: vec4<f32>,
    @location(5) clip: vec4<f32>,
    @location(6) pixel: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let local = corners[vertex_index];
    let pixel = input.rect.xy + local * input.rect.zw;
    var output: VsOut;
    output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0);
    output.local = local;
    output.size = input.rect.zw;
    output.fill = input.fill;
    output.border = input.border;
    output.params = input.params;
    output.clip = input.clip;
    output.pixel = pixel;
    return output;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
    let radius = min(input.params.y, min(input.size.x, input.size.y) * 0.5);
    let point = input.local * input.size - input.size * 0.5;
    let extent = max(input.size * 0.5 - vec2<f32>(radius), vec2<f32>(0.0));
    let corner_distance = length(max(abs(point) - extent, vec2<f32>(0.0))) - radius;
    let shape_alpha = 1.0 - smoothstep(0.0, 1.0, corner_distance);
    let border_alpha = 1.0 - smoothstep(-input.params.x - 1.0, -input.params.x + 1.0, corner_distance);
    let color = mix(input.fill, input.border, border_alpha);
    return vec4<f32>(color.rgb, color.a * input.params.z * shape_alpha);
}
"#;

const HIT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
struct VsIn { @location(0) rect: vec4<f32>, @location(1) params: vec4<f32>, @location(2) hit_id: u32, @location(3) clip: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) size: vec2<f32>, @location(2) params: vec4<f32>, @location(3) @interpolate(flat) hit_id: u32, @location(4) clip: vec4<f32>, @location(5) pixel: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[vertex_index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.size = input.rect.zw; output.params = input.params; output.hit_id = input.hit_id; output.clip = input.clip; output.pixel = pixel; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) u32 {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
 let radius = min(input.params.y, min(input.size.x,input.size.y)*0.5); let point = input.local*input.size-input.size*0.5; let extent=max(input.size*0.5-vec2<f32>(radius),vec2<f32>(0.0)); let corner_distance=length(max(abs(point)-extent,vec2<f32>(0.0)))-radius;
 if (corner_distance > 0.0 || input.params.z <= 0.0) { discard; } return input.hit_id;
}
"#;

const HIT_CLEAR_SHADER: &str = r#"
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
 var vertices = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
 return vec4<f32>(vertices[index], 0.0, 1.0);
}
@fragment fn fs_main() -> @location(0) u32 { return 0xffffffffu; }
"#;

const IMAGE_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct VsIn { @location(0) rect: vec4<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.tint = input.tint; output.clip = input.clip; output.pixel = pixel; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
 return textureSample(image_texture, image_sampler, input.local) * input.tint;
}
"#;

const TEXT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(1) @binding(1) var glyph_sampler: sampler;
struct VsIn { @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) uv: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) color: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32>, @location(4) uv: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position=vec4<f32>(pixel.x/view.viewport.x*2.0-1.0,1.0-pixel.y/view.viewport.y*2.0,0.0,1.0); output.local=local; output.color=input.color; output.clip=input.clip; output.pixel=pixel; output.uv=input.uv.xy + local * input.uv.zw; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
 let coverage = textureSample(glyph_atlas, glyph_sampler, input.uv).a;
 if (coverage <= 0.001) { discard; }
 return vec4<f32>(input.color.rgb, input.color.a * coverage);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct UiInstance {
    rect: [f32; 4],
    fill: [f32; 4],
    border: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiView {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiHitInstance { rect: [f32; 4], params: [f32; 4], hit_id: u32, _pad: [u32; 3], clip: [f32; 4] }

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiImageInstance { rect: [f32; 4], tint: [f32; 4], clip: [f32; 4], asset_id: u32, _pad: [u32; 3] }

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiTextInstance { rect: [f32; 4], color: [f32; 4], clip: [f32; 4], uv: [f32; 4] }

struct ResidentImage { bind_group: wgpu::BindGroup }

struct ResidentFont {
    font: fontdue::Font,
    _atlas: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    glyphs: HashMap<char, AtlasGlyph>,
    next_x: u32,
    next_y: u32,
    row_height: u32,
}

#[derive(Clone, Copy)]
struct AtlasGlyph { uv: [f32; 4], width: f32, height: f32, xmin: f32, ymin: f32, advance: f32 }

const FONT_ATLAS_SIZE: u32 = 1024;
const FONT_RASTER_SIZE: f32 = 32.0;

const HIT_READBACK_BYTES_PER_ROW: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

struct HitReadbackSlot {
    buffer: wgpu::Buffer,
    completion: Option<Receiver<Result<(), wgpu::BufferAsyncError>>>,
    copy_submitted: bool,
}

struct HitReadbackRing {
    slots: Vec<HitReadbackSlot>,
    next_slot: usize,
}

impl HitReadbackRing {
    fn new(device: &wgpu::Device, capacity: usize) -> Self {
        Self {
            slots: (0..capacity).map(|index| HitReadbackSlot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("neon3-ui-hit-readback-{index}")),
                    size: HIT_READBACK_BYTES_PER_ROW as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }), completion: None, copy_submitted: false,
            }).collect(), next_slot: 0,
        }
    }

    fn enqueue(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::Texture, pixel: [u32; 2]) -> Option<usize> {
        let index = self.next_slot;
        let slot = &mut self.slots[index];
        if slot.completion.is_some() { return None; }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: target, mip_level: 0, origin: wgpu::Origin3d { x: pixel[0], y: pixel[1], z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &slot.buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(HIT_READBACK_BYTES_PER_ROW), rows_per_image: Some(1) } },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        slot.copy_submitted = true;
        self.next_slot = (index + 1) % self.slots.len();
        Some(index)
    }

    fn begin_mapping(&mut self, index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(index) else { return false; };
        if !slot.copy_submitted || slot.completion.is_some() { return false; }
        let (sender, receiver) = mpsc::channel();
        slot.buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = sender.send(result); });
        slot.completion = Some(receiver); slot.copy_submitted = false; true
    }

    fn try_complete(&mut self, index: usize) -> Option<Result<u32, wgpu::BufferAsyncError>> {
        let slot = self.slots.get_mut(index)?;
        match slot.completion.as_ref()?.try_recv() {
            Ok(Ok(())) => {
                let bytes = slot.buffer.slice(..).get_mapped_range();
                let hit_id = u32::from_ne_bytes(bytes[..4].try_into().expect("readback slot has four bytes"));
                drop(bytes); slot.buffer.unmap(); slot.completion = None; Some(Ok(hit_id))
            }
            Ok(Err(error)) => { slot.completion = None; Some(Err(error)) }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => { slot.completion = None; None }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct UiVisual {
    bounds: UiBounds,
    style: UiStyle,
    kind: UiNodeKind,
    enabled: bool,
    clip: UiBounds,
    image: Option<AssetRef>,
    text: Option<TextRef>,
}

#[derive(Clone, Debug)]
struct ActiveTransition {
    from: UiVisual,
    target: UiVisual,
    started_at_seconds: f32,
    transition: UiTransition,
}

pub struct UiWgpuRenderer {
    pipeline: wgpu::RenderPipeline,
    view_buffer: wgpu::Buffer,
    view_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    current: HashMap<String, UiVisual>,
    active: HashMap<String, ActiveTransition>,
    pointer_position: Option<[f32; 2]>,
    pressed_until_seconds: f32,
    hit_pipeline: wgpu::RenderPipeline,
    hit_clear_pipeline: wgpu::RenderPipeline,
    hit_buffer: wgpu::Buffer,
    hit_capacity: usize,
    hit_readbacks: HitReadbackRing,
    image_pipeline: wgpu::RenderPipeline,
    image_buffer: wgpu::Buffer,
    image_capacity: usize,
    resident_images: HashMap<(String, u64, u64), ResidentImage>,
    image_texture_layout: wgpu::BindGroupLayout,
    text_pipeline: wgpu::RenderPipeline,
    text_buffer: wgpu::Buffer,
    text_capacity: usize,
    _text_texture_layout: wgpu::BindGroupLayout,
    resident_font: Option<ResidentFont>,
}

impl UiWgpuRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-ui-view-layout"),
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
        let view_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-ui-view"),
            size: std::mem::size_of::<UiView>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-view-bind-group"),
            layout: &view_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-panel-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-ui-panel-layout"),
            bind_group_layouts: &[&view_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-panel-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 32,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 48,
                            shader_location: 3,
                        },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 64, shader_location: 4 },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let hit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("neon3-ui-hit-id-shader"), source: wgpu::ShaderSource::Wgsl(HIT_SHADER.into()) });
        let hit_clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("neon3-ui-hit-clear-shader"), source: wgpu::ShaderSource::Wgsl(HIT_CLEAR_SHADER.into()) });
        let hit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-hit-id-pipeline"), layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &hit_shader, entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiHitInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 32, shader_location: 2 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &hit_shader, entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::R32Uint, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None,
            multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
        });
        let hit_clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-hit-clear-pipeline"), layout: None,
            vertex: wgpu::VertexState { module: &hit_clear_shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &hit_clear_shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::R32Uint, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
        });
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("neon3-ui-image-shader"), source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()) });
        let image_texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("neon3-ui-image-texture-layout"), entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { multisampled: false, view_dimension: wgpu::TextureViewDimension::D2, sample_type: wgpu::TextureSampleType::Float { filterable: true } }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
        ] });
        let image_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("neon3-ui-image-layout"), bind_group_layouts: &[&view_layout, &image_texture_layout], push_constant_ranges: &[] });
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-image-pipeline"), layout: Some(&image_layout),
            vertex: wgpu::VertexState { module: &image_shader, entry_point: Some("vs_main"), buffers: &[wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<UiImageInstance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &[
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 },
            ] }], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &image_shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
        });
        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("neon3-ui-text-shader"), source: wgpu::ShaderSource::Wgsl(TEXT_SHADER.into()) });
        let text_texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-ui-text-atlas-layout"), entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { multisampled: false, view_dimension: wgpu::TextureViewDimension::D2, sample_type: wgpu::TextureSampleType::Float { filterable: true } }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ]
        });
        let text_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("neon3-ui-text-layout"), bind_group_layouts: &[&view_layout, &text_texture_layout], push_constant_ranges: &[] });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-text-pipeline"), layout: Some(&text_layout),
            vertex: wgpu::VertexState { module: &text_shader, entry_point: Some("vs_main"), buffers: &[wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<UiTextInstance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &[wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 }] }], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &text_shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }), primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
        });
        Self {
            pipeline,
            view_buffer,
            view_bind_group,
            instance_buffer: create_instance_buffer(device, 1),
            instance_capacity: 1,
            current: HashMap::new(),
            active: HashMap::new(),
            pointer_position: None,
            pressed_until_seconds: 0.0,
            hit_pipeline,
            hit_clear_pipeline,
            hit_buffer: create_hit_buffer(device, 1),
            hit_capacity: 1,
            hit_readbacks: HitReadbackRing::new(device, 3),
            image_pipeline,
            image_buffer: create_image_buffer(device, 1),
            image_capacity: 1,
            resident_images: HashMap::new(),
            image_texture_layout,
            text_pipeline,
            text_buffer: create_text_buffer(device, 1),
            text_capacity: 1,
            _text_texture_layout: text_texture_layout,
            resident_font: None,
        }
    }

    pub(crate) fn draw_hit_id<'a>(&'a mut self, device: &wgpu::Device, queue: &wgpu::Queue, pass: &mut wgpu::RenderPass<'a>, fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>, viewport_size: [u32; 2], time_seconds: f32) {
        pass.set_pipeline(&self.hit_clear_pipeline);
        pass.draw(0..3, 0..1);
        let mut nodes = flatten_fragments(fragments); nodes.sort_by(|left, right| left.0.cmp(&right.0));
        let instances = nodes.into_iter().enumerate().filter_map(|(index, (_, visual, transition))| {
            let sampled = self.sample("hit", visual, transition, time_seconds);
            (sampled.kind == UiNodeKind::Button && sampled.enabled && sampled.style.opacity > 0.0).then_some(UiHitInstance { rect: [sampled.bounds.x, sampled.bounds.y, sampled.bounds.width, sampled.bounds.height], params: [sampled.style.border_width, sampled.style.corner_radius, sampled.style.opacity, 0.0], hit_id: index as u32 + 1, _pad: [0; 3], clip: [sampled.clip.x, sampled.clip.y, sampled.clip.x + sampled.clip.width, sampled.clip.y + sampled.clip.height] })
        }).collect::<Vec<_>>();
        if instances.is_empty() { return; }
        if instances.len() > self.hit_capacity { self.hit_capacity = instances.len().next_power_of_two(); self.hit_buffer = create_hit_buffer(device, self.hit_capacity); }
        queue.write_buffer(&self.hit_buffer, 0, bytemuck::cast_slice(&instances));
        queue.write_buffer(&self.view_buffer, 0, bytemuck::bytes_of(&UiView { viewport: [viewport_size[0].max(1) as f32, viewport_size[1].max(1) as f32], _pad: [0.0; 2] }));
        pass.set_pipeline(&self.hit_pipeline); pass.set_bind_group(0, &self.view_bind_group, &[]); pass.set_vertex_buffer(0, self.hit_buffer.slice(..)); pass.draw(0..6, 0..instances.len() as u32);
    }

    pub(crate) fn enqueue_hit_readback(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::Texture, pixel: [u32; 2]) -> Option<usize> {
        self.hit_readbacks.enqueue(encoder, target, pixel)
    }

    pub(crate) fn try_complete_hit_readback(&mut self, slot: usize) -> Option<Result<u32, wgpu::BufferAsyncError>> {
        self.hit_readbacks.try_complete(slot)
    }

    pub(crate) fn begin_hit_readback_mapping(&mut self, slot: usize) -> bool { self.hit_readbacks.begin_mapping(slot) }

    pub fn set_pointer_position(&mut self, position: [f32; 2]) {
        self.pointer_position = Some(position);
    }

    pub fn press_hovered(&mut self, time_seconds: f32) {
        self.pressed_until_seconds = time_seconds + 0.14;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn preload_image(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, content: &AssetBytes) -> Result<(), &'static str> {
        if content.asset.kind != "image" || content.media_type != "application/x-neon-rgba8" { return Err("unsupported_image_format"); }
        let (Some(width), Some(height)) = (content.width, content.height) else { return Err("invalid_image_dimensions"); };
        if width == 0 || height == 0 || content.bytes.len() != width as usize * height as usize * 4 { return Err("invalid_image_bytes"); }
        let texture = device.create_texture(&wgpu::TextureDescriptor { label: Some("neon3-ui-resident-image"), size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &content.bytes, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) }, wgpu::Extent3d { width, height, depth_or_array_layers: 1 });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("neon3-ui-resident-image-sampler"), mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, ..Default::default() });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("neon3-ui-resident-image-bind-group"), layout: &self.image_texture_layout, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ] });
        self.resident_images.insert((content.asset.project_id.clone(), content.asset.asset_id, content.asset.revision.0), ResidentImage { bind_group });
        Ok(())
    }

    pub(crate) fn preload_font(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, content: &AssetBytes) -> Result<(), &'static str> {
        if content.asset.kind != "font" || content.bytes.is_empty() { return Err("invalid_font_content"); }
        let font = fontdue::Font::from_bytes(content.bytes.as_slice(), fontdue::FontSettings::default()).map_err(|_| "invalid_font_content")?;
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-font-atlas"), size: wgpu::Extent3d { width: FONT_ATLAS_SIZE, height: FONT_ATLAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[]
        });
        let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("neon3-ui-font-atlas-sampler"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("neon3-ui-font-atlas-bind-group"), layout: &self._text_texture_layout, entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ] });
        self.resident_font = Some(ResidentFont { font, _atlas: atlas, bind_group, glyphs: HashMap::new(), next_x: 1, next_y: 1, row_height: 0 });
        let font = self.resident_font.as_mut().unwrap();
        for ch in ' '..='~' { let _ = ensure_glyph(device, queue, font, ch); }
        Ok(())
    }

    pub(crate) fn draw<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport_size: [u32; 2],
        time_seconds: f32,
    ) {
        let mut nodes = flatten_fragments(fragments);
        nodes.sort_by(|left, right| left.0.cmp(&right.0));
        let live: HashSet<_> = nodes.iter().map(|(id, _, _)| id.clone()).collect();
        self.current.retain(|id, _| live.contains(id));
        self.active.retain(|id, _| live.contains(id));

        let visuals = nodes
            .into_iter()
            .map(|(id, visual, transition)| {
                let sampled = self.sample(id.as_str(), visual, transition, time_seconds);
                (id, sampled)
            })
            .collect::<Vec<_>>();
        let instances = visuals
            .iter()
            .filter(|(_, visual)| visual.kind != UiNodeKind::Image)
            .map(|(_, visual)| self.instance(visual, time_seconds))
            .collect::<Vec<_>>();
        if instances.len() > self.instance_capacity {
            self.instance_capacity = instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
        }
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));
        let view = UiView {
            viewport: [
                viewport_size[0].max(1) as f32,
                viewport_size[1].max(1) as f32,
            ],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.view_buffer, 0, bytemuck::bytes_of(&view));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..instances.len() as u32);
        let images = visuals.iter().filter_map(|(_, visual)| {
            let asset = visual.image.as_ref()?;
            let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
            self.resident_images.contains_key(&key).then_some((key, UiImageInstance {
            rect: [visual.bounds.x, visual.bounds.y, visual.bounds.width, visual.bounds.height],
            tint: [visual.style.background_color[0], visual.style.background_color[1], visual.style.background_color[2], visual.style.background_color[3] * visual.style.opacity],
            clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height],
            asset_id: asset.asset_id as u32, _pad: [0; 3],
        })) }).collect::<Vec<_>>();
        if !images.is_empty() {
            if images.len() > self.image_capacity { self.image_capacity = images.len().next_power_of_two(); self.image_buffer = create_image_buffer(device, self.image_capacity); }
            pass.set_pipeline(&self.image_pipeline); pass.set_bind_group(0, &self.view_bind_group, &[]);
            for (index, (key, image)) in images.iter().enumerate() {
                queue.write_buffer(&self.image_buffer, 0, bytemuck::bytes_of(image));
                pass.set_bind_group(1, &self.resident_images[key].bind_group, &[]); pass.set_vertex_buffer(0, self.image_buffer.slice(..)); pass.draw(0..6, 0..1);
                let _ = index;
            }
        }
        let texts = self.resident_font.as_mut().map(|font| {
            visuals.iter().filter_map(|(_, visual)| {
                let text = visual.text.as_ref().and_then(text_ref_value);
                if !matches!(visual.kind, UiNodeKind::Label | UiNodeKind::Button) || text.is_none() { return None; }
                let text = text.unwrap();
                let mut x = visual.bounds.x;
                let baseline = visual.bounds.y + FONT_RASTER_SIZE;
                let mut first = true;
                let mut result = Vec::new();
                for ch in text.chars() {
                    if ch == '\n' { x = visual.bounds.x; continue; }
                    let glyph = ensure_glyph(device, queue, font, ch).ok()?;
                    let y = baseline + glyph.ymin;
                    result.push(UiTextInstance { rect: [x + glyph.xmin, y, glyph.width, glyph.height], color: [0.86, 0.95, 0.98, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv: glyph.uv });
                    x += glyph.advance;
                    first = false;
                }
                let _ = first;
                Some(result)
            }).flatten().collect::<Vec<_>>()
        }).unwrap_or_default();
        if !texts.is_empty() {
            if texts.len() > self.text_capacity { self.text_capacity = texts.len().next_power_of_two(); self.text_buffer = create_text_buffer(device, self.text_capacity); }
            queue.write_buffer(&self.text_buffer, 0, bytemuck::cast_slice(&texts));
            pass.set_pipeline(&self.text_pipeline); pass.set_bind_group(0, &self.view_bind_group, &[]); pass.set_bind_group(1, &self.resident_font.as_ref().unwrap().bind_group, &[]); pass.set_vertex_buffer(0, self.text_buffer.slice(..)); pass.draw(0..6, 0..texts.len() as u32);
        }
    }

    fn sample(
        &mut self,
        id: &str,
        target: UiVisual,
        transition: Option<UiTransition>,
        time_seconds: f32,
    ) -> UiVisual {
        if let Some(active) = self.active.get(id)
            && active.target == target
        {
            let sampled = sample_transition(active, time_seconds);
            self.current.insert(id.to_owned(), sampled.clone());
            return sampled;
        }
        let source = self.current.get(id).cloned();
        let sampled = match transition {
            Some(transition) if transition.duration_ms > 0 => {
                let from = source.unwrap_or_else(|| transition_source(&target, &transition));
                let active = ActiveTransition {
                    from,
                    target,
                    started_at_seconds: time_seconds,
                    transition,
                };
                let sampled = sample_transition(&active, time_seconds);
                self.active.insert(id.to_owned(), active);
                sampled
            }
            _ => target,
        };
        self.current.insert(id.to_owned(), sampled.clone());
        sampled
    }

    fn instance(&self, visual: &UiVisual, time_seconds: f32) -> UiInstance {
        let mut fill = visual.style.background_color;
        let bounds = visual.bounds;
        if visual.kind == UiNodeKind::Button
            && self
                .pointer_position
                .is_some_and(|position| contains(bounds, position))
        {
            let factor = if time_seconds < self.pressed_until_seconds {
                1.28
            } else {
                1.14
            };
            fill[0] = (fill[0] * factor).min(1.0);
            fill[1] = (fill[1] * factor).min(1.0);
            fill[2] = (fill[2] * factor).min(1.0);
        }
        UiInstance {
            rect: [bounds.x, bounds.y, bounds.width, bounds.height],
            fill,
            border: visual.style.border_color,
            params: [
                visual.style.border_width,
                visual.style.corner_radius,
                visual.style.opacity,
                0.0,
            ],
            clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height],
        }
    }
}

#[cfg(test)]
pub(crate) fn render_offscreen_for_test(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    size: [u32; 2],
    time_seconds: f32,
    resident_images: &[AssetBytes],
) -> Vec<u8> {
    let mut renderer = UiWgpuRenderer::new(device, format);
    for asset in resident_images {
        if asset.asset.kind == "image" { renderer.preload_image(device, queue, asset).unwrap(); }
        if asset.asset.kind == "font" { renderer.preload_font(device, queue, asset).unwrap(); }
    }
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("neon3-ui-offscreen-target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-offscreen-readback"),
        size: (size[0] * size[1] * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("neon3-ui-offscreen-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("neon3-ui-offscreen-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.draw(device, queue, &mut pass, fragments, size, time_seconds);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 4),
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let (sender, receiver) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
    device.poll(wgpu::Maintain::Wait);
    receiver.recv().unwrap().unwrap();
    let pixels = readback.slice(..).get_mapped_range();
    pixels.to_vec()
}

#[cfg(test)]
pub(crate) fn render_hit_ids_for_test(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    size: [u32; 2],
) -> Vec<u32> {
    let mut renderer = UiWgpuRenderer::new(device, wgpu::TextureFormat::Rgba8Unorm);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("neon3-ui-hit-id-test-target"),
        size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes_per_row = size[0] * 4;
    assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &vec![0xff; (bytes_per_row * size[1]) as usize],
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(size[1]) },
        wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
    );
    let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("neon3-ui-hit-id-readback"), size: (bytes_per_row * size[1]) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("neon3-ui-hit-id-test-encoder") });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("neon3-ui-hit-id-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
        });
        renderer.draw_hit_id(device, queue, &mut pass, fragments, size, 1.0);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(size[1]) } },
        wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));
    let (sender, receiver) = std::sync::mpsc::channel();
    readback.slice(..).map_async(wgpu::MapMode::Read, move |result| { sender.send(result).unwrap() });
    device.poll(wgpu::Maintain::Wait); receiver.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range();
    bytes.chunks_exact(4).map(|pixel| u32::from_ne_bytes(pixel.try_into().unwrap())).collect()
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-instances"),
        size: (capacity * std::mem::size_of::<UiInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_hit_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some("neon3-ui-hit-instances"), size: (capacity * std::mem::size_of::<UiHitInstance>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn create_image_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some("neon3-ui-image-instances"), size: (capacity * std::mem::size_of::<UiImageInstance>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn create_text_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor { label: Some("neon3-ui-text-instances"), size: (capacity * std::mem::size_of::<UiTextInstance>()) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
}

fn text_ref_value(text: &TextRef) -> Option<&str> {
    match text {
        TextRef::Key { key, .. } => (!key.trim().is_empty()).then_some(key.as_str()),
        TextRef::Literal { value } => (!value.is_empty()).then_some(value.as_str()),
    }
}

fn ensure_glyph(_device: &wgpu::Device, queue: &wgpu::Queue, font: &mut ResidentFont, ch: char) -> Result<AtlasGlyph, &'static str> {
    if let Some(glyph) = font.glyphs.get(&ch).copied() { return Ok(glyph); }
    let (metrics, bitmap) = font.font.rasterize(ch, FONT_RASTER_SIZE);
    let width = metrics.width as u32;
    let height = metrics.height as u32;
    if width == 0 || height == 0 {
        let glyph = AtlasGlyph { uv: [0.0, 0.0, 0.0, 0.0], width: 0.0, height: 0.0, xmin: metrics.xmin as f32, ymin: metrics.ymin as f32, advance: metrics.advance_width };
        font.glyphs.insert(ch, glyph);
        return Ok(glyph);
    }
    let padding = 1;
    if font.next_x + width + padding >= FONT_ATLAS_SIZE {
        font.next_x = 1;
        font.next_y = font.next_y.saturating_add(font.row_height + padding);
        font.row_height = 0;
    }
    if font.next_y + height + padding >= FONT_ATLAS_SIZE { return Err("font_atlas_full"); }
    let x = font.next_x;
    let y = font.next_y;
    let padded_bytes_per_row = (width * 4).div_ceil(256) * 256;
    let mut upload = vec![0_u8; (padded_bytes_per_row * height) as usize];
    for row in 0..height as usize {
        for column in 0..width as usize {
            let coverage = bitmap[row * width as usize + column];
            let offset = row * padded_bytes_per_row as usize + column * 4;
            upload[offset..offset + 4].copy_from_slice(&[255, 255, 255, coverage]);
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &font._atlas, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
        &upload,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_bytes_per_row), rows_per_image: Some(height) },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    font.next_x = x + width + padding;
    font.row_height = font.row_height.max(height);
    let atlas = FONT_ATLAS_SIZE as f32;
    let glyph = AtlasGlyph { uv: [x as f32 / atlas, y as f32 / atlas, width as f32 / atlas, height as f32 / atlas], width: width as f32, height: height as f32, xmin: metrics.xmin as f32, ymin: metrics.ymin as f32, advance: metrics.advance_width };
    font.glyphs.insert(ch, glyph);
    Ok(glyph)
}

fn flatten_fragments(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
) -> Vec<(String, UiVisual, Option<UiTransition>)> {
    let mut ordered = fragments.values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.fragment_id.0.cmp(&right.fragment_id.0));
    let mut result = Vec::new();
    for fragment in ordered {
        flatten_node(
            &mut result,
            &fragment.fragment_id.0,
            &fragment.root,
            [0.0, 0.0],
            None,
        );
    }
    result
}

fn flatten_node(
    out: &mut Vec<(String, UiVisual, Option<UiTransition>)>,
    fragment_id: &str,
    node: &UiNode,
    parent_offset: [f32; 2],
    inherited_clip: Option<UiBounds>,
) {
    let bounds = UiBounds {
        x: parent_offset[0] + node.bounds.x,
        y: parent_offset[1] + node.bounds.y,
        width: node.bounds.width,
        height: node.bounds.height,
    };
    let node_layout = node.layout.unwrap_or_default();
    let own_clip = if node_layout.clip { Some(bounds) } else { inherited_clip };
    let effective_clip = own_clip.unwrap_or(UiBounds { x: -1_000_000.0, y: -1_000_000.0, width: 2_000_000.0, height: 2_000_000.0 });
    if node.visible && node.style.opacity > 0.0 {
        out.push((
            format!("{fragment_id}/{}", node.node_id.0),
            UiVisual {
                bounds,
                style: node.style,
                kind: node.kind.clone(),
                enabled: node.enabled,
                clip: effective_clip,
                image: node.image.clone(),
                text: node.text.clone(),
            },
            node.enter_transition.clone(),
        ));
    }
    let inner = UiBounds { x: bounds.x + node_layout.padding[3], y: bounds.y + node_layout.padding[0], width: (bounds.width - node_layout.padding[1] - node_layout.padding[3]).max(0.0), height: (bounds.height - node_layout.padding[0] - node_layout.padding[2]).max(0.0) };
    let mut cursor = [inner.x - node_layout.scroll_offset[0], inner.y - node_layout.scroll_offset[1]];
    for child in &node.children {
        let child_offset = [bounds.x - node_layout.scroll_offset[0], bounds.y - node_layout.scroll_offset[1]];
        let child_layout = child.layout.unwrap_or_default();
        let offset = match node_layout.mode {
            UiLayoutMode::Row => {
                let offset = [cursor[0] - child.bounds.x + child_layout.margin[3], cursor[1] - child.bounds.y + child_layout.margin[0]];
                cursor[0] += child.bounds.width + child_layout.margin[1] + node_layout.gap; offset
            }
            UiLayoutMode::Column => {
                let offset = [cursor[0] - child.bounds.x + child_layout.margin[3], cursor[1] - child.bounds.y + child_layout.margin[0]];
                cursor[1] += child.bounds.height + child_layout.margin[2] + node_layout.gap; offset
            }
            UiLayoutMode::Absolute | UiLayoutMode::Overlay => child_offset,
        };
        flatten_node(out, fragment_id, child, offset, own_clip);
    }
}

fn transition_source(target: &UiVisual, transition: &UiTransition) -> UiVisual {
    let from = transition.from;
    UiVisual {
        bounds: from.bounds.unwrap_or(target.bounds),
        style: UiStyle {
            background_color: from
                .background_color
                .unwrap_or(target.style.background_color),
            border_color: from.border_color.unwrap_or(target.style.border_color),
            border_width: from.border_width.unwrap_or(target.style.border_width),
            corner_radius: from.corner_radius.unwrap_or(target.style.corner_radius),
            opacity: from.opacity.unwrap_or(target.style.opacity),
        },
        kind: target.kind.clone(),
        enabled: target.enabled,
        clip: target.clip,
        image: target.image.clone(),
        text: target.text.clone(),
    }
}

fn sample_transition(active: &ActiveTransition, time_seconds: f32) -> UiVisual {
    let elapsed_ms = ((time_seconds - active.started_at_seconds) * 1000.0).max(0.0);
    let progress = ((elapsed_ms - active.transition.delay_ms as f32)
        / active.transition.duration_ms as f32)
        .clamp(0.0, 1.0);
    let t = ease(progress, active.transition.easing);
    UiVisual {
        bounds: lerp_bounds(active.from.bounds, active.target.bounds, t),
        style: UiStyle {
            background_color: lerp4(
                active.from.style.background_color,
                active.target.style.background_color,
                t,
            ),
            border_color: lerp4(
                active.from.style.border_color,
                active.target.style.border_color,
                t,
            ),
            border_width: lerp(
                active.from.style.border_width,
                active.target.style.border_width,
                t,
            ),
            corner_radius: lerp(
                active.from.style.corner_radius,
                active.target.style.corner_radius,
                t,
            ),
            opacity: lerp(active.from.style.opacity, active.target.style.opacity, t),
        },
        kind: active.target.kind.clone(),
        enabled: active.target.enabled,
        clip: active.target.clip,
        image: active.target.image.clone(),
        text: active.target.text.clone(),
    }
}

fn lerp_bounds(from: UiBounds, to: UiBounds, t: f32) -> UiBounds {
    UiBounds {
        x: lerp(from.x, to.x, t),
        y: lerp(from.y, to.y, t),
        width: lerp(from.width, to.width, t),
        height: lerp(from.height, to.height, t),
    }
}

fn lerp4(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp(from[0], to[0], t),
        lerp(from[1], to[1], t),
        lerp(from[2], to[2], t),
        lerp(from[3], to[3], t),
    ]
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn ease(t: f32, easing: UiEasing) -> f32 {
    match easing {
        UiEasing::Linear => t,
        UiEasing::EaseIn => t * t,
        UiEasing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        UiEasing::EaseInOut => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) * 0.5
            }
        }
    }
}

fn contains(bounds: UiBounds, position: [f32; 2]) -> bool {
    position[0] >= bounds.x
        && position[0] <= bounds.x + bounds.width
        && position[1] >= bounds.y
        && position[1] <= bounds.y + bounds.height
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::Revision;
    use neon_ui_schema::{TextRef, UiFragmentId, UiNodeId, UiTransitionState};

    fn node() -> UiNode {
        UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 80.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            style: UiStyle::default(),
            enter_transition: Some(UiTransition {
                delay_ms: 0,
                duration_ms: 200,
                easing: UiEasing::EaseOut,
                from: UiTransitionState {
                    opacity: Some(0.0),
                    bounds: Some(UiBounds {
                        x: 10.0,
                        y: 40.0,
                        width: 100.0,
                        height: 80.0,
                    }),
                    ..UiTransitionState::default()
                },
            }),
            children: Vec::new(),
        }
    }

    #[test]
    fn flatten_resolves_child_bounds_and_fragment_paint_order() {
        let mut root = node();
        root.children.push(UiNode {
            node_id: UiNodeId("child".into()),
            kind: UiNodeKind::Button,
            bounds: UiBounds {
                x: 8.0,
                y: 6.0,
                width: 40.0,
                height: 24.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            style: UiStyle::default(),
            enter_transition: None,
            children: Vec::new(),
        });
        let fragments = HashMap::from([(
            UiFragmentId("fragment".into()),
            UiFragment {
                fragment_id: UiFragmentId("fragment".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let nodes = flatten_fragments(&fragments);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes[1].1.bounds,
            UiBounds {
                x: 18.0,
                y: 26.0,
                width: 40.0,
                height: 24.0
            }
        );
    }

    #[test]
    fn owner_font_and_text_ref_produce_glyph_pixels_without_background_fill() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })).or_else(|| pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))).expect("a headless adapter is required");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-ui-text-acceptance"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
        }, None)).expect("a device is required");
        let font = AssetBytes {
            asset: AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() },
            media_type: "font/ttf".into(), width: None, height: None, bytes: vec![0, 1, 0, 0],
        };
        let text = UiNode {
            node_id: UiNodeId("text".into()), kind: UiNodeKind::Label,
            bounds: UiBounds { x: 4.0, y: 4.0, width: 56.0, height: 24.0 }, layout: None,
            visible: true, enabled: true, text_key: None,
            text: Some(TextRef::Literal { value: "A".into() }), image: None,
            style: UiStyle { background_color: [0.0; 4], border_color: [0.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 },
            enter_transition: None, children: Vec::new(),
        };
        let fragment = UiFragment { fragment_id: UiFragmentId("text".into()), revision: Revision(1), root: text, effects: Vec::new() };
        let pixels = render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("text".into()), fragment)]), [64, 32], 1.0, &[font]);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0), "text must produce glyph alpha without a panel background");
    }

    #[test]
    fn text_wraps_within_label_width_and_respects_parent_clip() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        })).or_else(|| pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))).expect("a headless adapter is required");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-ui-text-wrap-clip"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
        }, None)).expect("a device is required");
        let font = AssetBytes {
            asset: AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() },
            media_type: "font/ttf".into(), width: None, height: None, bytes: vec![0, 1, 0, 0],
        };
        let label = UiNode {
            node_id: UiNodeId("wrapped-text".into()), kind: UiNodeKind::Label,
            bounds: UiBounds { x: 4.0, y: 0.0, width: 24.0, height: 96.0 }, layout: None,
            visible: true, enabled: true, text_key: None,
            text: Some(TextRef::Literal { value: "AAA".into() }), image: None,
            style: UiStyle { background_color: [0.0; 4], border_color: [0.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 },
            enter_transition: None, children: Vec::new(),
        };
        let root = UiNode {
            node_id: UiNodeId("clip-root".into()), kind: UiNodeKind::Panel,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 32.0, height: 64.0 },
            layout: Some(neon_ui_schema::UiLayout { clip: true, ..neon_ui_schema::UiLayout::default() }),
            visible: true, enabled: true, text_key: None, text: None, image: None,
            style: UiStyle { background_color: [0.0; 4], border_color: [0.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 },
            enter_transition: None, children: vec![label],
        };
        let pixels = render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("wrap-clip".into()), UiFragment { fragment_id: UiFragmentId("wrap-clip".into()), revision: Revision(1), root, effects: Vec::new() })]), [64, 96], 1.0, &[font]);
        let has_alpha_in_rows = |from: usize, until: usize| pixels.chunks_exact(4).enumerate().any(|(index, pixel)| index / 64 >= from && index / 64 < until && pixel[3] > 0);
        assert!(has_alpha_in_rows(8, 36), "first wrapped line must produce glyph coverage");
        assert!(has_alpha_in_rows(40, 64), "second wrapped line must produce glyph coverage");
        assert!(!has_alpha_in_rows(64, 96), "parent clip must discard glyph coverage outside its bounds");
    }

    #[test]
    fn flatten_uses_declared_column_layout_and_scroll_offset() {
        let mut root = node();
        root.bounds = UiBounds { x: 0.0, y: 0.0, width: 100.0, height: 100.0 };
        root.layout = Some(neon_ui_schema::UiLayout { mode: UiLayoutMode::Column, padding: [4.0; 4], gap: 2.0, scroll_offset: [0.0, 3.0], ..neon_ui_schema::UiLayout::default() });
        root.children = vec![
            UiNode { node_id: UiNodeId("first".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 10.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
            UiNode { node_id: UiNodeId("second".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 10.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
        ];
        let fragments = HashMap::from([(UiFragmentId("layout".into()), UiFragment { fragment_id: UiFragmentId("layout".into()), revision: Revision(1), root, effects: Vec::new() })]);
        let nodes = flatten_fragments(&fragments);
        assert_eq!(nodes[1].1.bounds.y, 1.0);
        assert_eq!(nodes[2].1.bounds.y, 13.0);
    }

    #[test]
    fn transition_uses_declared_entry_state_and_easing() {
        let target = UiVisual {
            bounds: UiBounds {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 80.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds { x: -1_000_000.0, y: -1_000_000.0, width: 2_000_000.0, height: 2_000_000.0 },
            image: None,
            text: None,
        };
        let transition = node().enter_transition.unwrap();
        let active = ActiveTransition {
            from: transition_source(&target, &transition),
            target: target.clone(),
            started_at_seconds: 1.0,
            transition,
        };
        assert_eq!(sample_transition(&active, 1.0).style.opacity, 0.0);
        let midpoint = sample_transition(&active, 1.1);
        assert!(midpoint.style.opacity > 0.5 && midpoint.style.opacity < 1.0);
        assert!(midpoint.bounds.y < 40.0 && midpoint.bounds.y > 20.0);
        assert_eq!(sample_transition(&active, 1.2).bounds, target.bounds);
    }

    #[test]
    fn transition_samples_current_state_for_updates() {
        let original = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds { x: -1_000_000.0, y: -1_000_000.0, width: 2_000_000.0, height: 2_000_000.0 },
            image: None,
            text: None,
        };
        let target = UiVisual {
            bounds: UiBounds {
                x: 100.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds { x: -1_000_000.0, y: -1_000_000.0, width: 2_000_000.0, height: 2_000_000.0 },
            image: None,
            text: None,
        };
        let active = ActiveTransition {
            from: original,
            target,
            started_at_seconds: 0.0,
            transition: UiTransition {
                delay_ms: 0,
                duration_ms: 100,
                easing: UiEasing::Linear,
                from: UiTransitionState::default(),
            },
        };
        assert_eq!(sample_transition(&active, 0.05).bounds.x, 50.0);
    }

    #[test]
    fn hit_readback_ring_copies_one_r32uint_texel_asynchronously() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower, compatible_surface: None, force_fallback_adapter: true,
        })).or_else(|| pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower, compatible_surface: None, force_fallback_adapter: false,
        }))).expect("a headless adapter is required for hit readback testing");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-hit-readback-test"), required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::downlevel_defaults(), memory_hints: wgpu::MemoryHints::MemoryUsage,
        }, None)).expect("a device is required for hit readback testing");
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-hit-readback-source"), size: wgpu::Extent3d { width: 64, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::R32Uint,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST, view_formats: &[],
        });
        let mut pixels = vec![0_u8; HIT_READBACK_BYTES_PER_ROW as usize];
        pixels[4..8].copy_from_slice(&37_u32.to_ne_bytes());
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &pixels, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(HIT_READBACK_BYTES_PER_ROW), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 64, height: 1, depth_or_array_layers: 1 },
        );
        let mut ring = HitReadbackRing::new(&device, 2);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("neon3-hit-readback-encoder") });
        let slot = ring.enqueue(&mut encoder, &target, [1, 0]).expect("a ring slot must be available");
        queue.submit(Some(encoder.finish()));
        assert!(ring.begin_mapping(slot));
        device.poll(wgpu::Maintain::Wait);
        assert_eq!(ring.try_complete(slot).unwrap().unwrap(), 37);
    }
}
