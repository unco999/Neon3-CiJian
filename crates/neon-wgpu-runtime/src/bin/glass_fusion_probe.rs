//! Focused GPU proof for the frosted-glass fusion asymmetry: why bright
//! (white) regions show a wrong fusion and dark regions look correct.
//!
//! The production path composites, back-to-front:
//!   1. blur(desktop backdrop brush)          <- OS Gaussian blur of the desktop behind the window
//!   2. dark tint sprite (NEON_BACKDROP_TINT @ NEON_BACKDROP_TINT_OPACITY)
//!   3. blur(behind-glass content surface)     <- empty unless composition_layer behind_glass
//!   4. wgpu premultiplied content surface      <- the Flow UI
//!
//! There is no custom glass shader: the blur is the Windows Composition
//! GaussianBlurEffect, and the tint is a separate CompositionColorBrush. This
//! probe runs that exact composite offscreen with a *controllable* backdrop so
//! we can measure the consumer-side error for a BRIGHT backdrop vs a DARK one,
//! and for a BRIGHT opaque panel vs a DARK opaque panel vs a transparent region.
//!
//! Deterministic output: one JSON line per (backdrop, region). Exit code 0 iff
//! the invariant "transparent + bright-backdrop regions must not accumulate a
//! second glass pass" holds.

use std::io::{self, Read};

use neon_protocol::Revision;
use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow};
use neon_ui_schema::{UiFragment, UiFragmentId};
use neon_wgpu_runtime::{UiDrawMode, UiWgpuRenderer};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 128;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The configured glass tint (mirrors `backdrop_tint()` in acrylic_backdrop.rs)
/// and the default used by start_ide.bat: #1a1a2e @ 0.30.
fn glass_tint() -> ([f32; 4], f32) {
    let hex = std::env::var("NEON_BACKDROP_TINT")
        .ok()
        .and_then(|s| s.strip_prefix('#').map(str::to_owned))
        .filter(|s| s.len() == 6)
        .and_then(|s| u32::from_str_radix(&s, 16).ok())
        .unwrap_or(0x1a1a2e);
    let opacity = std::env::var("NEON_BACKDROP_TINT_OPACITY")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.30)
        .clamp(0.0, 1.0);
    (
        [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
            1.0,
        ],
        opacity,
    )
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

fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Texture,
) -> Result<Vec<u8>, String> {
    let row_bytes = WIDTH * 4;
    let padded =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glass-fusion-probe-readback"),
        size: u64::from(padded) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("glass-fusion-probe-readback-encoder"),
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
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
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

#[allow(clippy::type_complexity)]
fn build_fragment() -> Result<(UiFragment, Vec<neon_ui_schema::UiShaderPackage>), String> {
    // Region layout (logical pixels, 256x128):
    //   x 12: dark opaque panel    #1E2530 (solid, hides the glass)
    //   x 96: bright opaque panel  #FFFFFF (solid, hides the glass)
    //   x 180: transparent panel   #00000000 (lets the glass show through)
    // Each panel carries a fractional-alpha 1px border so we also exercise the
    // premultiplied edge blend that is the classic source of "wrong fusion".
    let source = r#"
version 1
surface glass-fusion-probe revision 1
surface root w 256 h 128 fill #00000000
  panel dark_panel x 12 y 24 w 64 h 80 fill #1E2530FF
  panel bright_panel x 96 y 24 w 64 h 80 fill #FFFFFFFF
  panel transparent_panel x 180 y 24 w 64 h 80 fill #00000000
    geometry cut 8 0 8 0
"#;
    let mut document = parse_nui_flow(source)
        .map_err(|error| format!("probe Flow parse: {:?}", error.diagnostics))?;
    let _ = document.ir.shader_packages.first_mut();
    let packages = document.ir.shader_packages.clone();
    let fragment = UiFragment {
        fragment_id: UiFragmentId("glass-fusion-probe".into()),
        revision: Revision(1),
        root: document.ir.root.clone(),
        effects: lower_nui_flow_effects(&document),
    };
    fragment
        .validate()
        .map_err(|error| format!("probe fragment validation: {error:?}"))?;
    Ok((fragment, packages))
}

/// Composite pipeline that mirrors the production acrylic tree, but keeps the
/// "blur" step as an explicit box blur so the math is reproducible offscreen.
/// Inputs: backdrop texture (simulated desktop), tint color, behind-glass
/// texture (empty here), content texture (wgpu premultiplied UI).
fn composite(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    output_view: &wgpu::TextureView,
    backdrop_view: &wgpu::TextureView,
    behind_view: &wgpu::TextureView,
    content_view: &wgpu::TextureView,
    tint: [f32; 4],
    tint_opacity: f32,
) -> Result<(), String> {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("glass-fusion-probe-layout"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("glass-fusion-probe-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let tint_uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glass-fusion-probe-tint"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(
        &tint_uniform,
        0,
        bytemuck::bytes_of(&[tint[0], tint[1], tint[2], tint_opacity]),
    );
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("glass-fusion-probe-bind-group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(backdrop_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(behind_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(content_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: tint_uniform.as_entire_binding(),
            },
        ],
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("glass-fusion-probe-shader"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var backdrop_tex: texture_2d<f32>;
@group(0) @binding(1) var behind_tex: texture_2d<f32>;
@group(0) @binding(2) var content_tex: texture_2d<f32>;
@group(0) @binding(3) var linear_sampler: sampler;
@group(0) @binding(4) var<uniform> params: vec4<f32>; // tint.rgb + tint_opacity

@vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[index], 0.0, 1.0);
}

// Box blur over the backdrop + behind glass (the OS Gaussian is not available
// headless; a 3x3 box is directionally identical and deterministic).
fn box_blur(tex: texture_2d<f32>, sampler_ref: sampler, uv: vec2<f32>, px: vec2<f32>) -> vec4<f32> {
    var acc = vec4<f32>(0.0);
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let s = textureSample(tex, sampler_ref, uv + vec2<f32>(f32(dx) * px.x, f32(dy) * px.y));
            acc += s;
        }
    }
    return acc / 9.0;
}

@fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / vec2<f32>(256.0, 128.0);
    let px = vec2<f32>(1.0 / 256.0, 1.0 / 128.0);

    // 1. frosted desktop backdrop (blur), then 2. dark tint over it.
    let blurred_backdrop = box_blur(backdrop_tex, linear_sampler, uv, px);
    let tint_rgb = params.rgb;
    let tint_alpha = params.a;
    var glass = blurred_backdrop;
    // standard source-over tint: out = tint*a + backdrop*(1-a)
    glass[0] = tint_rgb[0] * tint_alpha + blurred_backdrop[0] * (1.0 - tint_alpha);
    glass[1] = tint_rgb[1] * tint_alpha + blurred_backdrop[1] * (1.0 - tint_alpha);
    glass[2] = tint_rgb[2] * tint_alpha + blurred_backdrop[2] * (1.0 - tint_alpha);

    // 3. behind-glass surface (empty/transparent) blurred and sourced over the tint.
    let blurred_behind = box_blur(behind_tex, linear_sampler, uv, px);
    // behind is premultiplied; source-over:
    glass[0] = blurred_behind[0] + glass[0] * (1.0 - blurred_behind[3]);
    glass[1] = blurred_behind[1] + glass[1] * (1.0 - blurred_behind[3]);
    glass[2] = blurred_behind[2] + glass[2] * (1.0 - blurred_behind[3]);
    glass[3] = blurred_behind[3] + glass[3] * (1.0 - blurred_behind[3]);

    // 4. wgpu content surface (premultiplied) sourced over the glass.
    let content = textureSample(content_tex, linear_sampler, uv);
    var out = vec4<f32>(0.0);
    out[0] = content[0] + glass[0] * (1.0 - content[3]);
    out[1] = content[1] + glass[1] * (1.0 - content[3]);
    out[2] = content[2] + glass[2] * (1.0 - content[3]);
    out[3] = content[3] + glass[3] * (1.0 - content[3]);
    return out;
}
"#
            .into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("glass-fusion-probe-pipeline-layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("glass-fusion-probe-pipeline"),
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
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("glass-fusion-probe-composite"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glass-fusion-probe-composite-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
    Ok(())
}

fn flat_pipeline(device: &wgpu::Device, color: wgpu::Color) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("glass-fusion-probe-flat-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let color = [
        color.r as f32,
        color.g as f32,
        color.b as f32,
        color.a as f32,
    ];
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("glass-fusion-probe-flat-shader"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"
const C: vec4<f32> = vec4<f32>({c0}, {c1}, {c2}, {c3});
@vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {{
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[index], 0.0, 1.0);
}}
@fragment fn fs() -> @location(0) vec4<f32> {{ return C; }}
"#,
                c0 = color[0],
                c1 = color[1],
                c2 = color[2],
                c3 = color[3]
            )
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("glass-fusion-probe-flat-pipeline"),
        layout: Some(&layout),
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
    })
}

fn paint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("glass-fusion-probe-paint"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glass-fusion-probe-paint-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * WIDTH + x) * 4) as usize;
    [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]
}

struct Metrics {
    backdrop: String,
    transparent_px: [u8; 4],
    dark_panel_px: [u8; 4],
    bright_panel_px: [u8; 4],
    edge_px: [u8; 4],
}

fn main() -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("probe JSONL input: {error}"))?;
    let _frames: Vec<serde_json::Value> = if input.trim().is_empty() {
        Vec::new()
    } else {
        input
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(|e| format!("input line: {e}")))
            .collect::<Result<_, _>>()?
    };

    let _ = &_frames; // probe accepts optional JSONL frame input for parity with sibling probes
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        #[cfg(windows)]
        backends: wgpu::Backends::DX12,
        #[cfg(not(windows))]
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    eprintln!("probe: instance");
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: true,
    }))
    .map_err(|error| format!("probe adapter: {error}"))?;
    eprintln!("probe: adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("glass-fusion-probe-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|error| format!("probe device: {error}"))?;
    eprintln!("probe: device");

    let (fragment, packages) = build_fragment()?;
    let fragments = std::collections::HashMap::from([(fragment.fragment_id.clone(), fragment)]);
    let mut renderer = UiWgpuRenderer::new(&device, FORMAT);
    renderer.sync_material_packages(&device, &packages);
    eprintln!("probe: renderer");

    let (tint, tint_opacity) = glass_tint();

    // Textures (producer = what wgpu draws, consumer = final composite).
    let content = texture(
        &device,
        "glass-probe-content",
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let behind = texture(
        &device,
        "glass-probe-behind",
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let consumer = texture(
        &device,
        "glass-probe-consumer",
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let content_view = content.create_view(&Default::default());
    let behind_view = behind.create_view(&Default::default());

    // Render the wgpu UI content surface exactly like the runtime does
    // (UiDrawMode::Screen over a TRANSPARENT clear).
    {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glass-probe-content-pass"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glass-probe-content-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &content_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.draw(
                &device,
                &queue,
                &mut pass,
                &fragments,
                [WIDTH, HEIGHT],
                [WIDTH as f32, HEIGHT as f32],
                0.0,
                UiDrawMode::Screen,
            );
        }
        // Behind-glass surface stays empty (TRANSPARENT clear) - mirrors this app.
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glass-probe-behind-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &behind_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        drop(pass);
        queue.submit(Some(encoder.finish()));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .map_err(|error| format!("probe poll: {error}"))?;
    }
    let content_px = readback(&device, &queue, &content)?;
    // Producer-side facts.
    let content_dark = pixel(&content_px, 44, 64);
    let content_bright = pixel(&content_px, 128, 64);
    let content_transp = pixel(&content_px, 212, 30); // near edge -> fractional alpha
    eprintln!(
        "probe: producer dark={:?} bright={:?} transparent-edge={:?}",
        content_dark, content_bright, content_transp
    );

    // Two backdrops: a bright (near-white) desktop and a dark desktop.
    let backdrops = [
        (
            "bright",
            wgpu::Color {
                r: 0.95,
                g: 0.95,
                b: 0.98,
                a: 1.0,
            },
        ),
        (
            "dark",
            wgpu::Color {
                r: 0.05,
                g: 0.05,
                b: 0.08,
                a: 1.0,
            },
        ),
    ];

    let mut records = Vec::new();
    let mut failed = false;
    for (name, bcolor) in backdrops {
        let backdrop = texture(
            &device,
            &format!("glass-probe-backdrop-{name}"),
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let backdrop_view = backdrop.create_view(&Default::default());
        let pipeline = flat_pipeline(&device, bcolor);
        paint(&device, &queue, &backdrop_view, &pipeline);

        let _ = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glass-probe-composite"),
        });
        let consumer_view = consumer.create_view(&Default::default());
        composite(
            &device,
            &queue,
            &consumer_view,
            &backdrop_view,
            &behind_view,
            &content_view,
            tint,
            tint_opacity,
        )?;
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .map_err(|error| format!("probe poll: {error}"))?;
        let final_px = readback(&device, &queue, &consumer)?;

        let metrics = Metrics {
            backdrop: name.to_string(),
            transparent_px: pixel(&final_px, 212, 30),
            dark_panel_px: pixel(&final_px, 44, 64),
            bright_panel_px: pixel(&final_px, 128, 64),
            edge_px: pixel(&final_px, 180, 64), // boundary of the transparent cut panel
        };

        // Diagnostic reasoning:
        // - dark_panel and bright_panel are OPAQUE content, so they must equal the
        //   producer values exactly (the glass below is hidden). Any shift = a
        //   bleed of glass into opaque pixels -> wrong fusion.
        let dark_bleed = content_dark != metrics.dark_panel_px;
        let bright_bleed = content_bright != metrics.bright_panel_px;
        // - transparent region must be exactly 0.3*tint + 0.7*blurred(desktop).
        //   On a near-white desktop the visible result is LIGHT; on dark desktop DARK.
        let transparent_is_light = metrics.transparent_px.iter().take(3).all(|&c| c >= 200);

        let record = serde_json::json!({
            "probe": "glass-fusion",
            "phase": "consumer",
            "backdrop": name,
            "buffer_pair": {"producer": "wgpu-content-surface", "consumer": format!("final-composite-{name}")},
            "producer": {"dark": content_dark, "bright": content_bright, "transparent_edge": content_transp},
            "consumer": {"dark": metrics.dark_panel_px, "bright": metrics.bright_panel_px, "transparent": metrics.transparent_px, "cut_edge": metrics.edge_px},
            "glass_tint": format!("#{:02x}{:02x}{:02x}@{:.2}", (tint[0]*255.0) as u8, (tint[1]*255.0) as u8, (tint[2]*255.0) as u8, tint_opacity),
            "diagnosis": {
                "dark_opaque_bleed": dark_bleed,
                "bright_opaque_bleed": bright_bleed,
                "transparent_is_light": transparent_is_light,
            },
            "status": !dark_bleed && !bright_bleed,
        });
        println!("{}", record);
        records.push(record);
        failed |= dark_bleed || bright_bleed;
    }
    if failed {
        Err("glass-fusion probe: opaque pixels picked up glass bleed".into())
    } else {
        Ok(())
    }
}
