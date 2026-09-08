//! Focused native GPU proof for the general composition-layer contract.
//!
//! The probe is intentionally transport-free. It parses NUI Flow, drives the
//! same UiWgpuRenderer used by the runtime into two independent GPU targets,
//! then consumes those targets through a separate composite/blur render pass.

use std::io::{self, Read};

use neon_protocol::Revision;
use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow};
use neon_ui_schema::{UiFragment, UiFragmentId};
use neon_wgpu_runtime::{UiDrawMode, UiWgpuRenderer};
use serde::Deserialize;

const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Clone, Debug, Deserialize)]
struct FrameInput {
    frame_sequence: u64,
    time_seconds: f32,
}

fn texture(device: &wgpu::Device, label: &str, usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage,
        view_formats: &[],
    })
}

fn readback(device: &wgpu::Device, queue: &wgpu::Queue, source: &wgpu::Texture) -> Result<Vec<u8>, String> {
    let row_bytes = WIDTH * 4;
    let padded = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("composition-layer-probe-readback"),
        size: u64::from(padded) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("composition-layer-probe-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) })
        .map_err(|error| format!("probe readback poll: {error}"))?;
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "probe readback timeout".to_owned())?
        .map_err(|error| format!("probe readback map: {error}"))?;
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(|error| format!("probe readback range: {error}"))?;
    let mut result = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in mapped.chunks_exact(padded as usize) {
        result.extend_from_slice(&row[..row_bytes as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(result)
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * WIDTH + x) * 4) as usize;
    [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]
}

fn distance(left: [u8; 4], right: [u8; 4]) -> u32 {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| u32::from(a.abs_diff(*b)))
        .sum()
}

fn build_fragment() -> Result<(UiFragment, Vec<neon_ui_schema::UiShaderPackage>), String> {
    let source = r#"
version 1
surface composition-probe revision 1
shader probe-material version 1 fallback standard_ui
surface root w 256 h 128 fill #00000000
  panel behind x 12 y 12 w 96 h 96 fill #D82048B0 composition_layer behind_glass
    text behind-label x 8 y 8 value "BEHIND"
    material probe-material
  panel top x 164 y 12 w 80 h 96 fill #20D070FF composition_layer overlay
    text top-label x 8 y 8 value "TOP"
"#;
    let mut document = parse_nui_flow(source)
        .map_err(|error| format!("probe Flow parse: {:?}", error.diagnostics))?;
    let package = document
        .ir
        .shader_packages
        .first_mut()
        .ok_or_else(|| "probe shader package missing".to_owned())?;
    package.source_digest = "sha256:composition-layer-probe".into();
    package.source_bytes = br#"
fn material(input: MaterialInput) -> vec4<f32> {
    return vec4<f32>(0.95, 0.08 + 0.65 * fract(view.time_seconds * 0.37), 0.04, 0.82);
}
"#
    .to_vec();
    let packages = document.ir.shader_packages.clone();
    let fragment = UiFragment {
        fragment_id: UiFragmentId("composition-probe".into()),
        revision: Revision(1),
        root: document.ir.root.clone(),
        effects: lower_nui_flow_effects(&document),
    };
    fragment
        .validate()
        .map_err(|error| format!("probe fragment validation: {error:?}"))?;
    Ok((fragment, packages))
}

fn composite_pipeline(
    device: &wgpu::Device,
    backdrop: &wgpu::TextureView,
    behind: &wgpu::TextureView,
    normal: &wgpu::TextureView,
) -> (wgpu::RenderPipeline, wgpu::BindGroup) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("composition-layer-probe-composite-layout"),
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
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("composition-layer-probe-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composition-layer-probe-composite-bind-group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(backdrop) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(behind) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(normal) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("composition-layer-probe-composite-shader"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var backdrop_tex: texture_2d<f32>;
@group(0) @binding(1) var behind_tex: texture_2d<f32>;
@group(0) @binding(2) var normal_tex: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;

@vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[index], 0.0, 1.0);
}

@fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / vec2<f32>(256.0, 128.0);
    let px = vec2<f32>(1.0 / 256.0, 1.0 / 128.0);
    var blurred = vec4<f32>(0.0);
    for (var i: i32 = -2; i <= 2; i = i + 1) {
        let sample_uv = uv + vec2<f32>(f32(i) * px.x * 2.0, f32(i) * px.y * 1.5);
        let backdrop = textureSample(backdrop_tex, linear_sampler, sample_uv);
        let behind = textureSample(behind_tex, linear_sampler, sample_uv);
        blurred += vec4<f32>(behind.rgb + backdrop.rgb * (1.0 - behind.a), max(behind.a, backdrop.a));
    }
    blurred = blurred / 5.0;
    let normal = textureSample(normal_tex, linear_sampler, uv);
    return vec4<f32>(normal.rgb + blurred.rgb * (1.0 - normal.a), max(normal.a, blurred.a));
}
"#
            .into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("composition-layer-probe-composite-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("composition-layer-probe-composite-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bind_group)
}

fn main() -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("probe JSONL input: {error}"))?;
    let mut frames = input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<FrameInput>(line).map_err(|error| format!("probe input line: {error}")))
        .collect::<Result<Vec<_>, _>>()?;
    if frames.is_empty() {
        frames = vec![
            FrameInput { frame_sequence: 1, time_seconds: 1.0 },
            FrameInput { frame_sequence: 2, time_seconds: 2.0 },
        ];
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        #[cfg(windows)]
        backends: wgpu::Backends::DX12,
        #[cfg(not(windows))]
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    eprintln!("probe: instance created");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|error| format!("probe adapter: {error}"))?;
    eprintln!("probe: adapter acquired");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("composition-layer-probe-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|error| format!("probe device: {error}"))?;
    eprintln!("probe: device acquired");

    let (fragment, packages) = build_fragment()?;
    eprintln!("probe: fragment built");
    let fragments = std::collections::HashMap::from([(fragment.fragment_id.clone(), fragment)]);
    let mut behind_renderer = UiWgpuRenderer::new(&device, FORMAT);
    let mut normal_renderer = UiWgpuRenderer::new(&device, FORMAT);
    behind_renderer.sync_material_packages(&device, &packages);
    normal_renderer.sync_material_packages(&device, &packages);
    eprintln!("probe: renderers ready");
    let backdrop = texture(&device, "composition-layer-probe-backdrop", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
    let behind = texture(&device, "composition-layer-probe-behind-surface-g1", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
    let normal = texture(&device, "composition-layer-probe-normal-surface-g1", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
    let consumer = texture(&device, "composition-layer-probe-consumer-final", wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
    let backdrop_view = backdrop.create_view(&Default::default());
    let behind_view = behind.create_view(&Default::default());
    let normal_view = normal.create_view(&Default::default());
    let consumer_view = consumer.create_view(&Default::default());
    let (pipeline, bind_group) = composite_pipeline(&device, &backdrop_view, &behind_view, &normal_view);
    let backdrop_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("composition-layer-probe-backdrop-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let backdrop_time = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("composition-layer-probe-backdrop-time"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let backdrop_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composition-layer-probe-backdrop-bind-group"),
        layout: &backdrop_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: backdrop_time.as_entire_binding(),
        }],
    });
    let backdrop_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("composition-layer-probe-backdrop-shader"),
        source: wgpu::ShaderSource::Wgsl(r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}
@group(0) @binding(0) var<uniform> frame_time: f32;
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = p.xy / vec2<f32>(256.0, 128.0);
    return vec4<f32>(0.05 + uv.x * 0.75 + frame_time * 0.05, 0.08 + uv.y * 0.65, 0.25 + (1.0 - uv.x) * 0.55, 1.0);
}
"#.into()),
    });
    let backdrop_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("composition-layer-probe-backdrop-pipeline"),
         layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&backdrop_layout)], immediate_size: 0 })),
        vertex: wgpu::VertexState { module: &backdrop_shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState { module: &backdrop_shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
        primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview_mask: None, cache: None,
    });

    let mut previous_pixels: Option<([u8; 4], [u8; 4])> = None;
    let mut failed = false;
    for frame in frames {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("composition-layer-probe-frame") });
        queue.write_buffer(&backdrop_time, 0, bytemuck::bytes_of(&frame.time_seconds));
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composition-layer-probe-backdrop-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &backdrop_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&backdrop_pipeline);
            pass.set_bind_group(0, &backdrop_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composition-layer-probe-behind-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &behind_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            behind_renderer.draw(&device, &queue, &mut pass, &fragments, [WIDTH, HEIGHT], [WIDTH as f32, HEIGHT as f32], frame.time_seconds, UiDrawMode::BehindGlass);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composition-layer-probe-normal-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &normal_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            normal_renderer.draw(&device, &queue, &mut pass, &fragments, [WIDTH, HEIGHT], [WIDTH as f32, HEIGHT as f32], frame.time_seconds, UiDrawMode::Screen);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composition-layer-probe-consumer-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &consumer_view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).map_err(|error| format!("probe frame poll: {error}"))?;
        eprintln!("probe: frame {} poll complete", frame.frame_sequence);
        eprintln!("probe: reading back producer");
        let producer = readback(&device, &queue, &behind)?;
        let final_pixels = readback(&device, &queue, &consumer)?;
        let producer_pixel = pixel(&producer, 48, 48);
        let consumer_pixel = pixel(&final_pixels, 48, 48);
        let top_pixel = pixel(&final_pixels, 200, 48);
        let changed = distance(producer_pixel, consumer_pixel) > 8;
        let top_present = top_pixel[1] > top_pixel[0] && top_pixel[1] > top_pixel[2] && top_pixel[3] > 180;
        let missing = producer_pixel[3] < 32 || consumer_pixel[3] < 32;
        let stale = previous_pixels.is_some_and(|(previous_producer, previous_consumer)| {
            frame.frame_sequence > 1
                && previous_producer == producer_pixel
                && previous_consumer == consumer_pixel
        });
        let coords = top_present && consumer_pixel[0] > top_pixel[0];
        let direction = if changed { "blurred_consumer_differs_from_producer" } else { "missing_or_unblurred" };
        let record = serde_json::json!({
            "probe": "composition-layer",
            "frame_sequence": frame.frame_sequence,
            "time_seconds": frame.time_seconds,
            "buffer_id": {"producer": format!("behind-surface:g1:f{}", frame.frame_sequence), "consumer": format!("final-composite:f{}", frame.frame_sequence)},
            "producer_unblurred_pixels": {"behind_48_48": producer_pixel},
            "consumer_blurred_final_pixels": {"behind_48_48": consumer_pixel, "top_200_48": top_pixel},
            "compare_direction": direction,
            "missing": missing,
            "stale": stale,
            "coords": coords,
            "status": !missing && !stale && changed && coords,
        });
        println!("{}", record);
        failed |= missing || stale || !changed || !coords;
        previous_pixels = Some((producer_pixel, consumer_pixel));
    }
    if failed { Err("composition layer probe failed pixel assertions".into()) } else { Ok(()) }
}
