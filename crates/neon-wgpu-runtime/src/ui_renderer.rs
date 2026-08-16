//! Minimal GPU UI composition pass adapted from Neon2's instanced panel renderer.
//! It deliberately consumes only Neon3's public UI schema, not old ECS state.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bytemuck::{Pod, Zeroable};
use neon_protocol::{AssetBytes, AssetRef};
use serde_json::{json, Value};
use neon_ui_schema::{
    RenderSurfaceRef, TextRef, UiAlignItems, UiBounds, UiClipPolicy, UiDragAxis, UiDragBinding,
    UiControlPresentation, UiDragBoundary, UiDropPlacement, UiEasing, UiFragment, UiFragmentRevision, UiIntent,
    UiJustifyContent, UiLayout, UiLayoutMode, UiNode, UiNodeKind, UiSemanticPayloadValue, UiStyle, UiTransition,
};

const SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;

fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
    let low = value / 12.92;
    let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(low, high, value > vec3<f32>(0.04045));
}
fn outside_clip(pixel: vec2<f32>, clip: vec4<f32>, radius: f32) -> bool {
    if (pixel.x < clip.x || pixel.y < clip.y || pixel.x > clip.z || pixel.y > clip.w) { return true; }
    if (radius <= 0.0) { return false; }
    let size = clip.zw - clip.xy; let r = min(radius, min(size.x, size.y) * 0.5);
    let point = pixel - (clip.xy + size * 0.5); let extent = max(size * 0.5 - vec2<f32>(r), vec2<f32>(0.0));
    return length(max(abs(point) - extent, vec2<f32>(0.0))) > r;
}

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
    if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
    let radius = min(input.params.y, min(input.size.x, input.size.y) * 0.5);
    let point = input.local * input.size - input.size * 0.5;
    let extent = max(input.size * 0.5 - vec2<f32>(radius), vec2<f32>(0.0));
    let corner_distance = length(max(abs(point) - extent, vec2<f32>(0.0))) - radius;
    let shape_alpha = 1.0 - smoothstep(0.0, 1.0, corner_distance);
    let border_alpha = 1.0 - smoothstep(-input.params.x - 1.0, -input.params.x + 1.0, corner_distance);
    let color = mix(input.fill, input.border, border_alpha);
    return vec4<f32>(srgb_to_linear(color.rgb), color.a * input.params.z * shape_alpha);
}
"#;

const HIT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
fn outside_clip(pixel: vec2<f32>, clip: vec4<f32>, radius: f32) -> bool { if (pixel.x < clip.x || pixel.y < clip.y || pixel.x > clip.z || pixel.y > clip.w) { return true; } if (radius <= 0.0) { return false; } let size=clip.zw-clip.xy; let r=min(radius,min(size.x,size.y)*0.5); let point=pixel-(clip.xy+size*0.5); let extent=max(size*0.5-vec2<f32>(r),vec2<f32>(0.0)); return length(max(abs(point)-extent,vec2<f32>(0.0)))>r; }
struct VsIn { @location(0) rect: vec4<f32>, @location(1) params: vec4<f32>, @location(2) hit_id: u32, @location(3) clip: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) size: vec2<f32>, @location(2) params: vec4<f32>, @location(3) @interpolate(flat) hit_id: u32, @location(4) clip: vec4<f32>, @location(5) pixel: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[vertex_index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.size = input.rect.zw; output.params = input.params; output.hit_id = input.hit_id; output.clip = input.clip; output.pixel = pixel; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) u32 {
  if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
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
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
 let low = value / 12.92; let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)); return select(low, high, value > vec3<f32>(0.04045));
}
struct VsIn { @location(0) rect: vec4<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.tint = input.tint; output.clip = input.clip; output.pixel = pixel; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
  let sample = textureSample(image_texture, image_sampler, input.local);
  return vec4<f32>(sample.rgb * srgb_to_linear(input.tint.rgb), sample.a * input.tint.a);
}
"#;

const TEXT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(1) @binding(1) var glyph_sampler: sampler;
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
 let low = value / 12.92; let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)); return select(low, high, value > vec3<f32>(0.04045));
}
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
  return vec4<f32>(srgb_to_linear(input.color.rgb), input.color.a * coverage);
}
"#;

const BUILTIN_UI_FONT: &[u8] = include_bytes!("../../../assets/fonts/SarasaUiSC-Light.ttf");

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
struct UiHitInstance {
    rect: [f32; 4],
    params: [f32; 4],
    hit_id: u32,
    _pad: [u32; 3],
    clip: [f32; 4],
}

#[derive(Clone, Debug)]
pub(crate) struct UiHitBinding {
    pub node_path: String,
    pub fragment: UiFragmentRevision,
    pub intent: Option<UiIntent>,
    pub text_input: Option<UiTextInputBinding>,
}

#[derive(Clone, Debug)]
pub(crate) struct UiTextInputBinding {
    pub node_path: String,
    pub max_length: u32,
    pub bounds: UiBounds,
}

#[derive(Clone, Debug)]
struct RendererDrag {
    binding: UiDragBinding,
    fragment: UiFragmentRevision,
    source_path: String,
    source_bounds: UiBounds,
    boundary_bounds: Option<UiBounds>,
    start: [f32; 2],
    origin: [f32; 2],
    moved: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct UiResolvedDragDrop {
    pub fragment: UiFragmentRevision,
    pub intent: UiIntent,
    pub source_key: String,
    pub target_key: String,
    pub placement: UiDropPlacement,
    pub presentation_template_key: Option<String>,
}

/// Process-local state for immediate IME composition. It never crosses the UI RPC boundary.
#[derive(Clone, Debug, Default)]
pub(crate) struct UiTextEditingState {
    pub node_path: Option<String>,
    pub committed: String,
    pub preedit: String,
    pub max_length: u32,
    pub cursor: usize,
    pub selection_anchor: usize,
    pub horizontal_scroll: f32,
}

#[derive(Clone, Debug)]
struct UiValueGesture {
    node_path: String,
    kind: UiNodeKind,
    bounds: UiBounds,
    min: f32,
    max: f32,
}

impl UiTextEditingState {
    fn rendered_text(&self) -> String {
        let split = char_byte_index(&self.committed, self.cursor);
        format!(
            "{}{}{}",
            &self.committed[..split],
            self.preedit,
            &self.committed[split..]
        )
    }
    pub fn focus(&mut self, binding: UiTextInputBinding, initial_value: String) {
        self.node_path = Some(binding.node_path);
        self.committed = initial_value;
        self.preedit.clear();
        self.max_length = binding.max_length;
        self.cursor = self.committed.chars().count();
        self.selection_anchor = self.cursor;
        self.horizontal_scroll = 0.0;
    }
    pub fn clear(&mut self) {
        self.node_path = None;
        self.preedit.clear();
        self.cursor = 0;
        self.selection_anchor = 0;
        self.horizontal_scroll = 0.0;
    }
    pub fn set_preedit(&mut self, value: String) {
        self.preedit = value;
    }
    pub fn commit(&mut self, value: &str) -> Option<String> {
        self.delete_selection();
        let available = self
            .max_length
            .saturating_sub(self.committed.chars().count() as u32) as usize;
        let filtered: String = value
            .chars()
            .filter(|ch| !ch.is_control() && *ch != '\n' && *ch != '\r')
            .take(available)
            .collect();
        self.preedit.clear();
        if filtered.is_empty() {
            return None;
        }
        let split = char_byte_index(&self.committed, self.cursor);
        self.committed.insert_str(split, &filtered);
        self.cursor += filtered.chars().count();
        self.selection_anchor = self.cursor;
        Some(self.committed.clone())
    }
    pub fn backspace(&mut self) -> Option<String> {
        self.preedit.clear();
        if self.has_selection() {
            self.delete_selection();
            return Some(self.committed.clone());
        }
        if self.cursor == 0 {
            return None;
        }
        let start = char_byte_index(&self.committed, self.cursor - 1);
        let end = char_byte_index(&self.committed, self.cursor);
        self.committed.replace_range(start..end, "");
        self.cursor -= 1;
        self.selection_anchor = self.cursor;
        Some(self.committed.clone())
    }
    pub fn delete(&mut self) -> Option<String> {
        self.preedit.clear();
        if self.has_selection() {
            self.delete_selection();
            return Some(self.committed.clone());
        }
        if self.cursor >= self.committed.chars().count() {
            return None;
        }
        let start = char_byte_index(&self.committed, self.cursor);
        let end = char_byte_index(&self.committed, self.cursor + 1);
        self.committed.replace_range(start..end, "");
        Some(self.committed.clone())
    }
    pub fn move_cursor(&mut self, delta: isize, extend_selection: bool) {
        self.preedit.clear();
        self.cursor = (self.cursor as isize + delta)
            .clamp(0, self.committed.chars().count() as isize) as usize;
        if !extend_selection {
            self.selection_anchor = self.cursor;
        }
    }
    pub fn move_to_edge(&mut self, end: bool, extend_selection: bool) {
        self.preedit.clear();
        self.cursor = if end {
            self.committed.chars().count()
        } else {
            0
        };
        if !extend_selection {
            self.selection_anchor = self.cursor;
        }
    }
    fn selection_range(&self) -> std::ops::Range<usize> {
        self.cursor.min(self.selection_anchor)..self.cursor.max(self.selection_anchor)
    }
    fn has_selection(&self) -> bool {
        self.cursor != self.selection_anchor
    }
    fn delete_selection(&mut self) {
        let range = self.selection_range();
        if range.is_empty() {
            return;
        }
        self.committed.replace_range(
            char_byte_index(&self.committed, range.start)
                ..char_byte_index(&self.committed, range.end),
            "",
        );
        self.cursor = range.start;
        self.selection_anchor = range.start;
    }
}

fn char_byte_index(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map_or(value.len(), |(offset, _)| offset)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiImageInstance {
    rect: [f32; 4],
    tint: [f32; 4],
    clip: [f32; 4],
    asset_id: u32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiTextInstance {
    rect: [f32; 4],
    color: [f32; 4],
    clip: [f32; 4],
    uv: [f32; 4],
}

struct ResidentImage {
    bind_group: wgpu::BindGroup,
}

struct ResidentRenderSurface {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: Option<[u32; 2]>,
}

struct ResidentFont {
    font: fontdue::Font,
    _atlas: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    glyphs: HashMap<char, AtlasGlyph>,
    ascent: f32,
    line_height: f32,
    next_x: u32,
    next_y: u32,
    row_height: u32,
}

#[derive(Clone, Copy)]
struct AtlasGlyph {
    uv: [f32; 4],
    width: f32,
    height: f32,
    xmin: f32,
    plane_min_y: f32,
    advance: f32,
}

const FONT_ATLAS_SIZE: u32 = 2048;
const FONT_RASTER_SIZE: f32 = 16.0;
const TEXT_INPUT_INSET: f32 = 6.0;
const CARET_WIDTH: f32 = 2.0;

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
            slots: (0..capacity)
                .map(|index| HitReadbackSlot {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(&format!("neon3-ui-hit-readback-{index}")),
                        size: HIT_READBACK_BYTES_PER_ROW as u64,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    completion: None,
                    copy_submitted: false,
                })
                .collect(),
            next_slot: 0,
        }
    }

    fn enqueue(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        pixel: [u32; 2],
    ) -> Option<usize> {
        let index = self.next_slot;
        let slot = &mut self.slots[index];
        if slot.completion.is_some() {
            return None;
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: pixel[0],
                    y: pixel[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(HIT_READBACK_BYTES_PER_ROW),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        slot.copy_submitted = true;
        self.next_slot = (index + 1) % self.slots.len();
        Some(index)
    }

    fn begin_mapping(&mut self, index: usize) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        if !slot.copy_submitted || slot.completion.is_some() {
            return false;
        }
        let (sender, receiver) = mpsc::channel();
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        slot.completion = Some(receiver);
        slot.copy_submitted = false;
        true
    }

    fn try_complete(&mut self, index: usize) -> Option<Result<u32, wgpu::BufferAsyncError>> {
        let slot = self.slots.get_mut(index)?;
        match slot.completion.as_ref()?.try_recv() {
            Ok(Ok(())) => {
                let bytes = slot
                    .buffer
                    .slice(..)
                    .get_mapped_range()
                    .expect("mapped readback range");
                let hit_id = u32::from_ne_bytes(
                    bytes[..4].try_into().expect("readback slot has four bytes"),
                );
                drop(bytes);
                slot.buffer.unmap();
                slot.completion = None;
                Some(Ok(hit_id))
            }
            Ok(Err(error)) => {
                slot.completion = None;
                Some(Err(error))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                slot.completion = None;
                None
            }
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
    clip_radius: f32,
    image: Option<AssetRef>,
    surface: Option<RenderSurfaceRef>,
    text: Option<TextRef>,
    presentation: Option<UiControlPresentation>,
}

#[derive(Clone, Debug)]
struct ActiveTransition {
    from: UiVisual,
    target: UiVisual,
    started_at_seconds: f32,
    transition: UiTransition,
}

struct PlannedNode {
    id: String,
    parent_id: Option<String>,
    target: UiVisual,
    transition: Option<UiTransition>,
    instance_index: Option<usize>,
}

pub struct UiWgpuRenderer {
    pipeline: wgpu::RenderPipeline,
    view_buffer: wgpu::Buffer,
    view_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    popup_instance_buffer: wgpu::Buffer,
    popup_instance_capacity: usize,
    plan_revisions: HashMap<neon_ui_schema::UiFragmentId, neon_protocol::Revision>,
    plan: Vec<PlannedNode>,
    sampled: Vec<UiVisual>,
    instances: Vec<UiInstance>,
    dirty_instances: Vec<usize>,
    viewport_size: [u32; 2],
    current: HashMap<String, UiVisual>,
    active: HashMap<String, ActiveTransition>,
    pointer_position: Option<[f32; 2]>,
    pressed_until_seconds: f32,
    hit_pipeline: wgpu::RenderPipeline,
    hit_clear_pipeline: wgpu::RenderPipeline,
    hit_buffer: wgpu::Buffer,
    hit_capacity: usize,
    hit_readbacks: HitReadbackRing,
    hit_bindings: HashMap<u32, UiHitBinding>,
    image_pipeline: wgpu::RenderPipeline,
    image_buffer: wgpu::Buffer,
    image_capacity: usize,
    resident_images: HashMap<(String, u64, u64), ResidentImage>,
    resident_render_surfaces: HashMap<String, ResidentRenderSurface>,
    image_texture_layout: wgpu::BindGroupLayout,
    text_pipeline: wgpu::RenderPipeline,
    text_buffer: wgpu::Buffer,
    text_capacity: usize,
    popup_text_buffer: wgpu::Buffer,
    popup_text_capacity: usize,
    _text_texture_layout: wgpu::BindGroupLayout,
    resident_font: Option<ResidentFont>,
    last_panel_instance_count: usize,
    pointer_visual_dirty: bool,
    editing: UiTextEditingState,
    focused_control: Option<String>,
    drag: Option<RendererDrag>,
    drag_offsets: HashMap<String, [f32; 2]>,
    value_gesture: Option<UiValueGesture>,
    value_previews: HashMap<String, UiSemanticPayloadValue>,
    open_dropdown: Option<String>,
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
            bind_group_layouts: &[Some(&view_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-panel-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
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
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 64,
                            shader_location: 4,
                        },
                    ],
                })],
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
            multiview_mask: None,
            cache: None,
        });
        let hit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-hit-id-shader"),
            source: wgpu::ShaderSource::Wgsl(HIT_SHADER.into()),
        });
        let hit_clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-hit-clear-shader"),
            source: wgpu::ShaderSource::Wgsl(HIT_CLEAR_SHADER.into()),
        });
        let hit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-hit-id-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &hit_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiHitInstance>() as u64,
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
                            format: wgpu::VertexFormat::Uint32,
                            offset: 32,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 48,
                            shader_location: 3,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &hit_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Uint,
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
        let hit_clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-hit-clear-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &hit_clear_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &hit_clear_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Uint,
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
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-image-shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });
        let image_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("neon3-ui-image-texture-layout"),
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
        let image_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-ui-image-layout"),
            bind_group_layouts: &[Some(&view_layout), Some(&image_texture_layout)],
            immediate_size: 0,
        });
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-image-pipeline"),
            layout: Some(&image_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiImageInstance>() as u64,
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
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
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
            multiview_mask: None,
            cache: None,
        });
        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-text-shader"),
            source: wgpu::ShaderSource::Wgsl(TEXT_SHADER.into()),
        });
        let text_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("neon3-ui-text-atlas-layout"),
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
        let text_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-ui-text-layout"),
            bind_group_layouts: &[Some(&view_layout), Some(&text_texture_layout)],
            immediate_size: 0,
        });
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-text-pipeline"),
            layout: Some(&text_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiTextInstance>() as u64,
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
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
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
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            view_buffer,
            view_bind_group,
            instance_buffer: create_instance_buffer(device, 1),
            instance_capacity: 1,
            popup_instance_buffer: create_instance_buffer(device, 1),
            popup_instance_capacity: 1,
            plan_revisions: HashMap::new(),
            plan: Vec::new(),
            sampled: Vec::new(),
            instances: Vec::new(),
            dirty_instances: Vec::new(),
            viewport_size: [0, 0],
            current: HashMap::new(),
            active: HashMap::new(),
            pointer_position: None,
            pressed_until_seconds: 0.0,
            hit_pipeline,
            hit_clear_pipeline,
            hit_buffer: create_hit_buffer(device, 1),
            hit_capacity: 1,
            hit_readbacks: HitReadbackRing::new(device, 3),
            hit_bindings: HashMap::new(),
            image_pipeline,
            image_buffer: create_image_buffer(device, 1),
            image_capacity: 1,
            resident_images: HashMap::new(),
            resident_render_surfaces: HashMap::new(),
            image_texture_layout,
            text_pipeline,
            text_buffer: create_text_buffer(device, 1),
            text_capacity: 1,
            popup_text_buffer: create_text_buffer(device, 1),
            popup_text_capacity: 1,
            _text_texture_layout: text_texture_layout,
            resident_font: None,
            last_panel_instance_count: 0,
            pointer_visual_dirty: false,
            editing: UiTextEditingState::default(),
            focused_control: None,
            drag: None,
            drag_offsets: HashMap::new(),
            value_gesture: None,
            value_previews: HashMap::new(),
            open_dropdown: None,
        }
    }

    pub(crate) fn draw_hit_id<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport_size: [u32; 2],
        _time_seconds: f32,
    ) {
        pass.set_pipeline(&self.hit_clear_pipeline);
        pass.draw(0..3, 0..1);
        let declarations = collect_hit_declarations(fragments);
        self.hit_bindings.clear();
        let mut instances = Vec::new();
        for (node_path, _, visual, _) in flatten_fragments(fragments, self.resident_font.as_ref()) {
            let has_drag_binding = fragments.iter().any(|(fragment_id, fragment)| {
                let source_path = format!("{}/", fragment_id.0);
                node_path.strip_prefix(&source_path).is_some_and(|node_id| {
                    fragment.effects.iter().any(|effect| {
                        matches!(effect, neon_ui_schema::UiEffect::DragBinding { binding } if binding.source_node_id.0 == node_id)
                    })
                })
            });
            if (!is_interactive_control(&visual.kind)
                && !has_drag_binding)
                || !visual.enabled
                || visual.style.opacity <= 0.0
            {
                continue;
            }
            let hit_id = instances.len() as u32 + 1;
            let mut binding = declarations
                .get(&node_path)
                .cloned()
                .unwrap_or(UiHitBinding {
                    node_path: node_path.clone(),
                    fragment: UiFragmentRevision {
                        id: neon_ui_schema::UiFragmentId(
                            node_path.split('/').next().unwrap_or_default().into(),
                        ),
                        revision: neon_protocol::Revision(0),
                    },
                    intent: None,
                    text_input: None,
                });
            if visual.kind == UiNodeKind::TextInput {
                binding.text_input = Some(UiTextInputBinding {
                    node_path: node_path.clone(),
                    max_length: 256,
                    bounds: visual.bounds,
                });
            }
            self.hit_bindings.insert(hit_id, binding);
            instances.push(UiHitInstance {
                rect: [
                    visual.bounds.x,
                    visual.bounds.y,
                    visual.bounds.width,
                    visual.bounds.height,
                ],
                params: [
                    visual.style.border_width,
                    visual.style.corner_radius,
                    visual.style.opacity,
                    visual.clip_radius,
                ],
                hit_id,
                _pad: [0; 3],
                clip: [
                    visual.clip.x,
                    visual.clip.y,
                    visual.clip.x + visual.clip.width,
                    visual.clip.y + visual.clip.height,
                ],
            });
        }
        if instances.is_empty() {
            return;
        }
        if instances.len() > self.hit_capacity {
            self.hit_capacity = instances.len().next_power_of_two();
            self.hit_buffer = create_hit_buffer(device, self.hit_capacity);
        }
        queue.write_buffer(&self.hit_buffer, 0, bytemuck::cast_slice(&instances));
        queue.write_buffer(
            &self.view_buffer,
            0,
            bytemuck::bytes_of(&UiView {
                viewport: [
                    viewport_size[0].max(1) as f32,
                    viewport_size[1].max(1) as f32,
                ],
                _pad: [0.0; 2],
            }),
        );
        pass.set_pipeline(&self.hit_pipeline);
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.hit_buffer.slice(..));
        pass.draw(0..6, 0..instances.len() as u32);
    }

    pub(crate) fn enqueue_hit_readback(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        pixel: [u32; 2],
    ) -> Option<usize> {
        self.hit_readbacks.enqueue(encoder, target, pixel)
    }

    pub(crate) fn try_complete_hit_readback(
        &mut self,
        slot: usize,
    ) -> Option<Result<u32, wgpu::BufferAsyncError>> {
        self.hit_readbacks.try_complete(slot)
    }

    pub(crate) fn begin_hit_readback_mapping(&mut self, slot: usize) -> bool {
        self.hit_readbacks.begin_mapping(slot)
    }

    pub(crate) fn hit_binding(&self, hit_id: u32) -> Option<UiHitBinding> {
        self.hit_bindings.get(&hit_id).cloned()
    }

    /// Semantic declaration paths only. Numeric renderer hit IDs remain private
    /// to this process and are never included in a diagnostic response.
    pub(crate) fn semantic_hit_nodes(&self) -> Vec<String> {
        let mut paths = self
            .hit_bindings
            .values()
            .filter(|binding| binding.intent.is_some())
            .map(|binding| binding.node_path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Resolves the topmost declared control at the current pointer position.
    /// This is a local fallback for capture only; the renderer still submits the
    /// GPU hit pass for hover/readback diagnostics.
    pub(crate) fn hit_id_at_pointer(&self) -> Option<u32> {
        let pointer = self.pointer_position?;
        let modal = self.active_modal_index();
        self.plan.iter().rev().find_map(|node| {
            if !node.target.enabled
                || !contains(node.target.bounds, pointer)
                || !contains(node.target.clip, pointer)
            {
                return None;
            }
            if let Some(modal) = modal
                && !self.node_is_in_subtree(node.id.as_str(), modal)
            {
                return None;
            }
            self.hit_bindings
                .iter()
                .find_map(|(hit_id, binding)| (binding.node_path == node.id).then_some(*hit_id))
        })
    }

    /// Starts a renderer-local high-frequency gesture. The value is previewed
    /// per frame and sent once as a typed commit when the pointer is released.
    pub(crate) fn begin_value_gesture(&mut self, binding: &UiHitBinding) -> bool {
        let Some(node) = self.plan.iter().find(|node| node.id == binding.node_path) else {
            return false;
        };
        let (min, max) = match (&node.target.kind, &node.target.presentation) {
            (UiNodeKind::Slider | UiNodeKind::DragValue, Some(UiControlPresentation::Numeric { min, max, .. })) => (*min, *max),
            (UiNodeKind::Scrollbar, Some(UiControlPresentation::Scroll { .. })) => (0.0, 1.0),
            _ => return false,
        };
        let bounds = match node.target.kind {
            UiNodeKind::Slider => UiBounds {
                x: node.target.bounds.x + node.target.bounds.width * 0.57,
                y: node.target.bounds.y,
                width: node.target.bounds.width * 0.34,
                height: node.target.bounds.height,
            },
            UiNodeKind::Scrollbar => UiBounds {
                x: node.target.bounds.x + 10.0,
                y: node.target.bounds.y,
                width: (node.target.bounds.width - 20.0).max(1.0),
                height: node.target.bounds.height,
            },
            UiNodeKind::DragValue => drag_value_bounds(node.target.bounds),
            _ => node.target.bounds,
        };
        if !self.pointer_position.is_some_and(|pointer| contains(bounds, pointer)) {
            return false;
        }
        self.value_gesture = Some(UiValueGesture {
            node_path: binding.node_path.clone(),
            kind: node.target.kind.clone(),
            bounds,
            min,
            max,
        });
        self.update_value_gesture()
    }

    pub(crate) fn requires_value_gesture(&self, binding: &UiHitBinding) -> bool {
        self.plan.iter().find(|node| node.id == binding.node_path).is_some_and(|node| {
            matches!(node.target.kind, UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar)
        })
    }

    pub(crate) fn update_value_gesture(&mut self) -> bool {
        let Some(gesture) = self.value_gesture.as_ref() else {
            return false;
        };
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let fraction = ((pointer[0] - gesture.bounds.x) / gesture.bounds.width.max(1.0)).clamp(0.0, 1.0);
        let value = gesture.min + (gesture.max - gesture.min) * fraction;
        let payload = match gesture.kind {
            UiNodeKind::DragValue => UiSemanticPayloadValue::I32 { value: value.round() as i32 },
            UiNodeKind::Slider | UiNodeKind::Scrollbar => UiSemanticPayloadValue::F32 { value },
            _ => return false,
        };
        self.value_previews.insert(gesture.node_path.clone(), payload);
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn finish_value_gesture(&mut self) -> Option<UiSemanticPayloadValue> {
        let gesture = self.value_gesture.take()?;
        self.value_previews.remove(&gesture.node_path)
    }

    pub(crate) fn cancel_value_gesture(&mut self) {
        if let Some(gesture) = self.value_gesture.take() {
            self.value_previews.remove(&gesture.node_path);
        }
    }

    pub(crate) fn toggle_dropdown_at_pointer(&mut self) -> bool {
        if self.modal_active() {
            return false;
        }
        let Some(pointer) = self.pointer_position else { return false; };
        let Some(node) = self.plan.iter().find(|node| {
            node.target.kind == UiNodeKind::Dropdown && contains(node.target.bounds, pointer)
        }) else { return false; };
        if self.open_dropdown.as_deref() == Some(node.id.as_str()) {
            self.open_dropdown = None;
        } else {
            self.open_dropdown = Some(node.id.clone());
        }
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn dropdown_option_at_pointer(&self) -> Option<(UiHitBinding, UiSemanticPayloadValue)> {
        if self.modal_active() {
            return None;
        }
        let node_path = self.open_dropdown.as_ref()?;
        let pointer = self.pointer_position?;
        let (plan_index, rows) = self.dropdown_popup_layout()?;
        let node = &self.plan[plan_index];
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()? else { return None; };
        let index = rows.iter().position(|row| contains(*row, pointer))?;
        let value = options.get(index)?.clone();
        let binding = self.hit_bindings.values().find(|binding| binding.node_path == *node_path)?.clone();
        Some((binding, UiSemanticPayloadValue::Enum { value }))
    }

    pub(crate) fn list_option_at_pointer(&self) -> Option<(UiHitBinding, UiSemanticPayloadValue)> {
        if self.modal_active() {
            return None;
        }
        let pointer = self.pointer_position?;
        let node = self.plan.iter().find(|node| {
            node.target.kind == UiNodeKind::ListBox && contains(node.target.bounds, pointer)
        })?;
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()? else { return None; };
        let index = list_box_rows(node.target.bounds, options.len())
            .iter()
            .position(|row| contains(*row, pointer))?;
        let value = options.get(index)?.clone();
        let binding = self.hit_bindings.values().find(|binding| binding.node_path == node.id)?.clone();
        Some((binding, UiSemanticPayloadValue::Enum { value }))
    }

    pub(crate) fn close_dropdown(&mut self) {
        self.open_dropdown = None;
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn dropdown_debug_snapshot(&self) -> Value {
        let popup = self.dropdown_popup_layout().map(|(plan_index, rows)| {
            let anchor = self.plan[plan_index].target.bounds;
            json!({
                "node_path": self.plan[plan_index].id,
                "anchor": {"x": anchor.x, "y": anchor.y, "width": anchor.width, "height": anchor.height},
                "rows": rows.iter().map(|row| json!({"x": row.x, "y": row.y, "width": row.width, "height": row.height})).collect::<Vec<_>>(),
            })
        });
        json!({"viewport": self.viewport_size, "open_dropdown": self.open_dropdown, "popup": popup})
    }

    /// A top-level popup consumes an outside press before underlying controls
    /// see it, matching conventional menu dismissal behavior.
    pub(crate) fn dismiss_dropdown_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else { return false; };
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else { return false; };
        let source = self.plan[plan_index].target.bounds;
        if contains(source, pointer) || rows.iter().any(|row| contains(*row, pointer)) {
            return false;
        }
        self.close_dropdown();
        true
    }

    /// Modal dismissal is intentionally renderer-local: it only consumes an
    /// outside press and clears local text focus. Closing remains declarative.
    pub(crate) fn dismiss_modal_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else { return false; };
        let Some(index) = self.active_modal_index() else { return false; };
        if contains(self.plan[index].target.bounds, pointer) {
            return false;
        }
        self.clear_text_focus();
        self.pointer_visual_dirty = true;
        true
    }

    fn active_modal_index(&self) -> Option<usize> {
        self.plan.iter().rposition(|node| matches!(node.target.kind, UiNodeKind::Modal | UiNodeKind::Dialog))
    }

    fn modal_active(&self) -> bool {
        self.active_modal_index().is_some()
    }

    fn modal_blocks_pointer(&self) -> bool {
        let Some(pointer) = self.pointer_position else { return false; };
        self.active_modal_index().is_some_and(|index| !contains(self.plan[index].target.bounds, pointer))
    }

    fn active_modal_allows_node(&self, node_id: &str) -> bool {
        self.active_modal_index()
            .is_none_or(|modal| self.node_is_in_subtree(node_id, modal))
    }

    fn node_is_in_subtree(&self, node_id: &str, root: usize) -> bool {
        let mut current = self.plan.iter().position(|node| node.id == node_id);
        while let Some(index) = current {
            if index == root {
                return true;
            }
            current = self.plan[index]
                .parent_id
                .as_deref()
                .and_then(|parent| self.plan.iter().position(|node| node.id == parent));
        }
        false
    }

    /// Text inputs must focus on the first pointer press, before asynchronous GPU hit readback.
    pub(crate) fn text_input_at_pointer(&self) -> Option<UiTextInputBinding> {
        if self.modal_blocks_pointer() {
            return None;
        }
        let pointer = self.pointer_position?;
        self.hit_bindings
            .values()
            .filter_map(|binding| binding.text_input.as_ref())
            .find(|input| self.active_modal_allows_node(&input.node_path) && contains(input.bounds, pointer))
            .cloned()
    }

    /// Focus is renderer-local presentation state. Semantic events still carry
    /// only their declared intent and never this path or a hit identifier.
    pub(crate) fn focus_control_at_pointer(&mut self) -> bool {
        if self.modal_blocks_pointer() {
            return false;
        }
        let Some(pointer) = self.pointer_position else { return false; };
        let Some(path) = self.hit_bindings.values().find_map(|binding| {
            self.plan.iter().find(|node| node.id == binding.node_path)
                .filter(|node| self.active_modal_allows_node(&node.id) && node.target.enabled && contains(node.target.bounds, pointer))
                .map(|_| binding.node_path.clone())
        }) else { return false; };
        self.focused_control = Some(path);
        self.pointer_visual_dirty = true;
        true
    }

    pub fn set_pointer_position(&mut self, position: [f32; 2]) {
        self.pointer_position = Some(position);
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn pointer_position(&self) -> Option<[f32; 2]> {
        self.pointer_position
    }

    pub(crate) fn begin_drag_at_pointer(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        for fragment in fragments.values() {
            for effect in &fragment.effects {
                let neon_ui_schema::UiEffect::DragBinding { binding } = effect else {
                    continue;
                };
                let source_path =
                    format!("{}/{}", fragment.fragment_id.0, binding.source_node_id.0);
                let Some(index) = self.plan.iter().position(|node| node.id == source_path) else {
                    continue;
                };
                let source = self.sampled[index].bounds;
                if !contains(source, pointer) {
                    continue;
                }
                let parent = self.plan[index]
                    .parent_id
                    .as_deref()
                    .and_then(|id| self.plan.iter().position(|node| node.id == id))
                    .map(|parent| self.sampled[parent].bounds)
                    .unwrap_or(source);
                let boundary_bounds = match binding.boundary {
                    UiDragBoundary::Parent => Some(parent),
                    UiDragBoundary::Surface => Some(UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: self.viewport_size[0].max(1) as f32,
                        height: self.viewport_size[1].max(1) as f32,
                    }),
                    UiDragBoundary::Free => None,
                };
                self.drag = Some(RendererDrag {
                    binding: binding.clone(),
                    fragment: UiFragmentRevision {
                        id: fragment.fragment_id.clone(),
                        revision: fragment.revision,
                    },
                    source_path: source_path.clone(),
                    source_bounds: source,
                    boundary_bounds,
                    start: pointer,
                    origin: self
                        .drag_offsets
                        .get(&source_path)
                        .copied()
                        .unwrap_or([0.0; 2]),
                    moved: false,
                });
                return true;
            }
        }
        false
    }

    pub(crate) fn update_drag_preview(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(active) = self.drag.as_mut() else {
            return false;
        };
        let mut delta = [pointer[0] - active.start[0], pointer[1] - active.start[1]];
        if delta[0].hypot(delta[1]) < active.binding.threshold {
            return false;
        }
        match active.binding.axis {
            UiDragAxis::Horizontal => delta[1] = 0.0,
            UiDragAxis::Vertical => delta[0] = 0.0,
            UiDragAxis::Both => {}
        }
        let mut offset = [active.origin[0] + delta[0], active.origin[1] + delta[1]];
        if active.binding.snap > 0.0 {
            offset = [
                (offset[0] / active.binding.snap).round() * active.binding.snap,
                (offset[1] / active.binding.snap).round() * active.binding.snap,
            ];
        }
        offset = clamp_drag_offset(offset, active.source_bounds, active.boundary_bounds);
        self.drag_offsets.insert(active.source_path.clone(), offset);
        active.moved = true;
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn finish_drag_at_pointer(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> Option<UiResolvedDragDrop> {
        let active = self.drag.take()?;
        let pointer = self.pointer_position?;
        let result = active
            .moved
            .then(|| self.resolve_drop_target(fragments, &active, pointer));
        self.drag_offsets.remove(&active.source_path);
        self.pointer_visual_dirty = true;
        result.flatten()
    }

    pub(crate) fn cancel_drag(&mut self) {
        if let Some(active) = self.drag.take() {
            self.drag_offsets.remove(&active.source_path);
            self.pointer_visual_dirty = true;
        }
    }

    pub(crate) fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    fn resolve_drop_target(
        &self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        active: &RendererDrag,
        pointer: [f32; 2],
    ) -> Option<UiResolvedDragDrop> {
        self.plan
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, node)| {
                if node.id == active.source_path
                    || node.id.starts_with(&(active.source_path.clone() + "/"))
                    || !contains(self.sampled[index].bounds, pointer)
                    || !contains(self.sampled[index].clip, pointer)
                {
                    return None;
                }
                let (fragment_id, node_id) = fragments.iter().find_map(|(id, _)| {
                    node.id
                        .strip_prefix(&(id.0.clone() + "/"))
                        .map(|node_id| (id, node_id))
                })?;
                let fragment = fragments.get(fragment_id)?;
                fragment.effects.iter().rev().find_map(|effect| {
                    let neon_ui_schema::UiEffect::DropBinding { binding } = effect else {
                        return None;
                    };
                    (binding.target_node_id.0 == node_id
                        && binding.accepts_drag_key == active.binding.key)
                        .then(|| UiResolvedDragDrop {
                            fragment: active.fragment.clone(),
                            intent: binding.intent.clone(),
                            source_key: active.binding.source_node_id.0.clone(),
                            target_key: binding.target_node_id.0.clone(),
                            placement: binding.placement,
                            presentation_template_key: binding.presentation_template_key.clone(),
                        })
                })
            })
    }

    fn drag_offset_for_node(
        &self,
        index: usize,
        plan_index: &HashMap<&str, usize>,
    ) -> Option<[f32; 2]> {
        let mut current = Some(index);
        while let Some(node_index) = current {
            let node = &self.plan[node_index];
            if let Some(offset) = self.drag_offsets.get(&node.id) {
                return Some(*offset);
            }
            current = node
                .parent_id
                .as_deref()
                .and_then(|parent| plan_index.get(parent).copied());
        }
        None
    }

    pub fn press_hovered(&mut self, time_seconds: f32) {
        self.pressed_until_seconds = time_seconds + 0.14;
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn focus_text_input(&mut self, binding: UiTextInputBinding) {
        let initial_value = self
            .plan
            .iter()
            .find(|node| node.id == binding.node_path)
            .and_then(|node| node.target.text.as_ref())
            .and_then(text_ref_value)
            .unwrap_or_default()
            .to_owned();
        self.editing.focus(binding, initial_value);
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn set_text_input_caret_from_pointer(
        &mut self,
        position: [f32; 2],
        extend_selection: bool,
    ) {
        let Some(node_path) = self.editing.node_path.clone() else {
            return;
        };
        let Some(bounds) = self
            .plan
            .iter()
            .find(|node| node.id == node_path)
            .map(|node| node.target.bounds)
        else {
            return;
        };
        let Some(font) = self.resident_font.as_ref() else {
            return;
        };
        let text_x = position[0] - bounds.x - TEXT_INPUT_INSET + self.editing.horizontal_scroll;
        let cursor = caret_index_for_x(&font.font, &self.editing.committed, text_x);
        self.editing.cursor = cursor;
        if !extend_selection {
            self.editing.selection_anchor = cursor;
        }
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn set_ime_preedit(&mut self, value: String) {
        if self.editing.node_path.is_some() {
            self.editing.set_preedit(value);
            self.ensure_text_input_caret_visible();
            self.pointer_visual_dirty = true;
        }
    }

    pub(crate) fn commit_ime_text(&mut self, value: &str) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let committed = self.editing.commit(value)?;
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        let binding = self.text_input_binding(&node_path)?;
        Some((binding, committed))
    }

    pub(crate) fn backspace_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let committed = self.editing.backspace()?;
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        Some((self.text_input_binding(&node_path)?, committed))
    }

    pub(crate) fn delete_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let committed = self.editing.delete()?;
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        Some((self.text_input_binding(&node_path)?, committed))
    }

    pub(crate) fn move_text_input_cursor(&mut self, delta: isize, extend_selection: bool) -> bool {
        if self.editing.node_path.is_none() {
            return false;
        }
        self.editing.move_cursor(delta, extend_selection);
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn move_text_input_to_edge(&mut self, end: bool, extend_selection: bool) -> bool {
        if self.editing.node_path.is_none() {
            return false;
        }
        self.editing.move_to_edge(end, extend_selection);
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        true
    }

    fn text_input_binding(&self, node_path: &str) -> Option<UiHitBinding> {
        self.hit_bindings
            .values()
            .find(|binding| {
                binding
                    .text_input
                    .as_ref()
                    .is_some_and(|input| input.node_path == node_path)
            })
            .cloned()
    }

    pub(crate) fn text_input_ime_rect(&self) -> Option<UiBounds> {
        let node_path = self.editing.node_path.as_ref()?;
        let bounds = self
            .plan
            .iter()
            .find(|node| &node.id == node_path)?
            .target
            .bounds;
        let font = self.resident_font.as_ref()?;
        let x = bounds.x
            + TEXT_INPUT_INSET
            + text_advance(&font.font, &self.editing.committed, self.editing.cursor)
            + text_advance(
                &font.font,
                &self.editing.preedit,
                self.editing.preedit.chars().count(),
            )
            - self.editing.horizontal_scroll;
        Some(UiBounds {
            x,
            y: bounds.y + ((bounds.height - font.line_height).max(0.0) * 0.5),
            width: CARET_WIDTH,
            height: font.line_height.min(bounds.height),
        })
    }

    fn ensure_text_input_caret_visible(&mut self) {
        let Some(rect) = self.text_input_ime_rect() else {
            return;
        };
        let Some(node_path) = self.editing.node_path.as_ref() else {
            return;
        };
        let Some(bounds) = self
            .plan
            .iter()
            .find(|node| &node.id == node_path)
            .map(|node| node.target.bounds)
        else {
            return;
        };
        let left = bounds.x + TEXT_INPUT_INSET;
        let right = bounds.x + bounds.width - TEXT_INPUT_INSET - CARET_WIDTH;
        if rect.x < left {
            self.editing.horizontal_scroll =
                (self.editing.horizontal_scroll - (left - rect.x)).max(0.0);
        }
        if rect.x > right {
            self.editing.horizontal_scroll += rect.x - right;
        }
    }

    pub(crate) fn clear_text_focus(&mut self) {
        self.editing.clear();
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn has_active_animation(&mut self, time_seconds: f32) -> bool {
        self.active.retain(|_, active| {
            let end = active.started_at_seconds
                + (active.transition.delay_ms + active.transition.duration_ms) as f32 / 1000.0;
            time_seconds < end
        });
        !self.active.is_empty() || time_seconds < self.pressed_until_seconds
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn preload_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        content: &AssetBytes,
    ) -> Result<(), &'static str> {
        if content.asset.kind != "image" || content.media_type != "application/x-neon-rgba8" {
            return Err("unsupported_image_format");
        }
        let (Some(width), Some(height)) = (content.width, content.height) else {
            return Err("invalid_image_dimensions");
        };
        if width == 0 || height == 0 || content.bytes.len() != width as usize * height as usize * 4
        {
            return Err("invalid_image_bytes");
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-resident-image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &content.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-resident-image-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-resident-image-bind-group"),
            layout: &self.image_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.resident_images.insert(
            (
                content.asset.project_id.clone(),
                content.asset.asset_id,
                content.asset.revision.0,
            ),
            ResidentImage { bind_group },
        );
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_render_surface(
        &mut self,
        device: &wgpu::Device,
        target_id: impl Into<String>,
        texture: wgpu::Texture,
    ) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-render-surface-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-render-surface-bind-group"),
            layout: &self.image_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.resident_render_surfaces.insert(
            target_id.into(),
            ResidentRenderSurface {
                _texture: texture,
                _view: view,
                bind_group,
                size: None,
            },
        );
    }

    pub(crate) fn ensure_render_surface(
        &mut self,
        device: &wgpu::Device,
        target_id: &str,
        size: [u32; 2],
    ) -> wgpu::TextureView {
        if let Some(surface) = self.resident_render_surfaces.get(target_id)
            && surface.size == Some(size)
        {
            return surface._view.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-resident-render-surface"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-render-surface-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-render-surface-bind-group"),
            layout: &self.image_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.resident_render_surfaces.insert(
            target_id.into(),
            ResidentRenderSurface {
                _texture: texture,
                _view: view.clone(),
                bind_group,
                size: Some(size),
            },
        );
        view
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn preload_font(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        content: &AssetBytes,
    ) -> Result<(), &'static str> {
        if content.asset.kind != "font" || content.bytes.is_empty() {
            return Err("invalid_font_content");
        }
        self.install_font(device, queue, content.bytes.as_slice())
    }

    fn ensure_builtin_font(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.resident_font.is_none() {
            self.install_font(device, queue, BUILTIN_UI_FONT)
                .expect("the bundled Sarasa UI font must be valid");
        }
    }

    fn install_font(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        bytes: &[u8],
    ) -> Result<(), &'static str> {
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|_| "invalid_font_content")?;
        let line_metrics = font
            .horizontal_line_metrics(FONT_RASTER_SIZE)
            .ok_or("invalid_font_metrics")?;
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-font-atlas"),
            size: wgpu::Extent3d {
                width: FONT_ATLAS_SIZE,
                height: FONT_ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-font-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-font-atlas-bind-group"),
            layout: &self._text_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.resident_font = Some(ResidentFont {
            font,
            _atlas: atlas,
            bind_group,
            glyphs: HashMap::new(),
            ascent: line_metrics.ascent,
            line_height: line_metrics.new_line_size,
            next_x: 1,
            next_y: 1,
            row_height: 0,
        });
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
        self.ensure_builtin_font(device, queue);
        let viewport_size = [viewport_size[0].max(1), viewport_size[1].max(1)];
        if self.viewport_size != viewport_size {
            self.viewport_size = viewport_size;
            queue.write_buffer(
                &self.view_buffer,
                0,
                bytemuck::bytes_of(&UiView {
                    viewport: [viewport_size[0] as f32, viewport_size[1] as f32],
                    _pad: [0.0; 2],
                }),
            );
        }
        let plan_changed = self.refresh_plan(fragments);
        self.instances.clear();
        let pointer_active = self.pointer_visual_dirty || time_seconds < self.pressed_until_seconds;
        self.dirty_instances.clear();
        for index in 0..self.plan.len() {
            let node = &self.plan[index];
            // A parent transition is applied after this pass. Sampling every node avoids
            // carrying the previous frame's inherited translation into the next frame.
            let _should_sample =
                plan_changed || pointer_active || self.active.contains_key(&node.id);
            let sampled = Self::sample(
                &mut self.current,
                &mut self.active,
                &node.id,
                &node.target,
                node.transition.as_ref(),
                time_seconds,
            );
            self.sampled[index] = sampled;
        }
        // Child bounds are flattened to final window coordinates. Reapply each parent's
        // sampled translation and opacity so an entering panel carries its whole subtree.
        let plan_index = self
            .plan
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect::<HashMap<_, _>>();
        let top_layer = top_layer_roots(&self.plan, &plan_index);
        let mut subtree_translation = vec![[0.0_f32; 2]; self.plan.len()];
        let mut subtree_opacity = vec![1.0_f32; self.plan.len()];
        for index in 0..self.plan.len() {
            let target = &self.plan[index].target;
            let parent = self.plan[index]
                .parent_id
                .as_deref()
                .and_then(|parent| plan_index.get(parent).copied());
            let inherited_translation =
                parent.map_or([0.0; 2], |parent| subtree_translation[parent]);
            let inherited_opacity = parent.map_or(1.0, |parent| subtree_opacity[parent]);
            let own_translation = [
                self.sampled[index].bounds.x - target.bounds.x,
                self.sampled[index].bounds.y - target.bounds.y,
            ];
            let own_opacity = if target.style.opacity > 0.0 {
                self.sampled[index].style.opacity / target.style.opacity
            } else {
                1.0
            };
            self.sampled[index].bounds.x += inherited_translation[0];
            self.sampled[index].bounds.y += inherited_translation[1];
            self.sampled[index].clip.x += inherited_translation[0];
            self.sampled[index].clip.y += inherited_translation[1];
            if self.sampled[index].clip == target.bounds {
                self.sampled[index].clip.x += own_translation[0];
                self.sampled[index].clip.y += own_translation[1];
            }
            self.sampled[index].style.opacity *= inherited_opacity;
            if top_layer[index].is_some() {
                // Like dropdown popups and captured drags, declared overlays escape
                // document clipping while remaining in logical window coordinates.
                self.sampled[index].clip = UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: viewport_size[0] as f32,
                    height: viewport_size[1] as f32,
                };
                self.sampled[index].clip_radius = 0.0;
            }
            if let Some(offset) = self.drag_offset_for_node(index, &plan_index) {
                self.sampled[index].bounds.x += offset[0];
                self.sampled[index].bounds.y += offset[1];
                // A captured drag is composed as a temporary top-level layer,
                // so its subtree is no longer clipped by its original parent.
                self.sampled[index].clip = UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: viewport_size[0] as f32,
                    height: viewport_size[1] as f32,
                };
                self.sampled[index].clip_radius = 0.0;
            }
            subtree_translation[index] = [
                inherited_translation[0] + own_translation[0],
                inherited_translation[1] + own_translation[1],
            ];
            subtree_opacity[index] = inherited_opacity * own_opacity;
        }
        // Preserve document order for ordinary UI, then append the captured subtree.
        // This gives the preview a temporary top-level z-order without changing the
        // canonical UI tree or its declared parentage.
        for dragged_layer in [false, true] {
            for index in 0..self.plan.len() {
                if self.plan[index].instance_index.is_none()
                    || top_layer[index].is_some()
                    || self.drag_offset_for_node(index, &plan_index).is_some() != dragged_layer
                {
                    continue;
                }
                let first_instance = self.instances.len();
                let visual = &self.sampled[index];
                self.instances
                    .push(self.instance(visual, &self.plan[index].id, time_seconds));
                self.instances
                    .extend(self.component_chrome_instances(visual, &self.plan[index].id));
                self.dirty_instances.extend(first_instance..self.instances.len());
            }
        }
        // Reuse the dropdown popup resource path for declarative top-level layers.
        // Modal backdrops are inserted immediately before their own subtree.
        let mut popup_instances = self.dropdown_popup_instances();
        for index in 0..self.plan.len() {
            let Some(root) = top_layer[index] else { continue; };
            if root == index && matches!(self.plan[index].target.kind, UiNodeKind::Modal | UiNodeKind::Dialog) {
                popup_instances.push(overlay_instance(
                    UiBounds { x: 0.0, y: 0.0, width: viewport_size[0] as f32, height: viewport_size[1] as f32 },
                    UiBounds { x: 0.0, y: 0.0, width: viewport_size[0] as f32, height: viewport_size[1] as f32 },
                    [0.0, 0.0, 0.0, 0.45],
                ));
            }
            if self.plan[index].instance_index.is_some() {
                popup_instances.push(self.instance(&self.sampled[index], &self.plan[index].id, time_seconds));
                popup_instances.extend(self.component_chrome_instances(&self.sampled[index], &self.plan[index].id));
            }
        }
        self.pointer_visual_dirty = false;
        self.append_text_input_overlays();
        self.last_panel_instance_count = self.instances.len();
        let mut instance_buffer_recreated = false;
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
            instance_buffer_recreated = true;
        }
        if popup_instances.len() > self.popup_instance_capacity {
            self.popup_instance_capacity = popup_instances.len().next_power_of_two();
            self.popup_instance_buffer = create_instance_buffer(device, self.popup_instance_capacity);
        }
        if plan_changed || instance_buffer_recreated || self.editing.node_path.is_some() {
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
        } else {
            for &index in &self.dirty_instances {
                let offset = (index * std::mem::size_of::<UiInstance>()) as u64;
                queue.write_buffer(
                    &self.instance_buffer,
                    offset,
                    bytemuck::bytes_of(&self.instances[index]),
                );
            }
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.instances.len() as u32);
        let images = self
            .sampled
            .iter()
            .filter_map(|visual| {
                let asset = visual.image.as_ref()?;
                let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
                self.resident_images.contains_key(&key).then_some((
                    key,
                    UiImageInstance {
                        rect: [
                            visual.bounds.x,
                            visual.bounds.y,
                            visual.bounds.width,
                            visual.bounds.height,
                        ],
                        tint: [
                            visual.style.background_color[0],
                            visual.style.background_color[1],
                            visual.style.background_color[2],
                            visual.style.background_color[3] * visual.style.opacity,
                        ],
                        clip: [
                            visual.clip.x,
                            visual.clip.y,
                            visual.clip.x + visual.clip.width,
                            visual.clip.y + visual.clip.height,
                        ],
                        asset_id: asset.asset_id as u32,
                        _pad: [0; 3],
                    },
                ))
            })
            .collect::<Vec<_>>();
        if !images.is_empty() {
            if images.len() > self.image_capacity {
                self.image_capacity = images.len().next_power_of_two();
                self.image_buffer = create_image_buffer(device, self.image_capacity);
            }
            pass.set_pipeline(&self.image_pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            for (index, (key, image)) in images.iter().enumerate() {
                queue.write_buffer(&self.image_buffer, 0, bytemuck::bytes_of(image));
                pass.set_bind_group(1, &self.resident_images[key].bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                pass.draw(0..6, 0..1);
                let _ = index;
            }
        }
        let surfaces = self
            .sampled
            .iter()
            .filter_map(|visual| {
                let surface = visual.surface.as_ref()?;
                self.resident_render_surfaces
                    .contains_key(&surface.target_id)
                    .then_some((
                        surface.target_id.clone(),
                        UiImageInstance {
                            rect: [
                                visual.bounds.x,
                                visual.bounds.y,
                                visual.bounds.width,
                                visual.bounds.height,
                            ],
                            tint: [1.0, 1.0, 1.0, visual.style.opacity],
                            clip: [
                                visual.clip.x,
                                visual.clip.y,
                                visual.clip.x + visual.clip.width,
                                visual.clip.y + visual.clip.height,
                            ],
                            asset_id: 0,
                            _pad: [0; 3],
                        },
                    ))
            })
            .collect::<Vec<_>>();
        if !surfaces.is_empty() {
            pass.set_pipeline(&self.image_pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            for (key, surface) in &surfaces {
                queue.write_buffer(&self.image_buffer, 0, bytemuck::bytes_of(surface));
                pass.set_bind_group(1, &self.resident_render_surfaces[key].bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }
        let dropdown_texts = self.dropdown_option_texts();
        let list_box_texts = self.list_box_option_texts();
        let drag_value_texts = self.drag_value_texts();
        let (texts, popup_texts) = self
            .resident_font
            .as_mut()
            .map(|font| {
                let texts = self.sampled
                    .iter()
                    .enumerate()
                    .filter_map(|(index, visual)| {
                        if top_layer[index].is_some() {
                            return None;
                        }
                        let local_text = Some(&self.editing)
                            .filter(|editing| {
                                editing.node_path.as_deref() == Some(self.plan[index].id.as_str())
                            })
                            .map(UiTextEditingState::rendered_text);
                        let text = local_text
                            .as_deref()
                            .or_else(|| visual.text.as_ref().and_then(text_ref_value));
                        if !matches!(
                            visual.kind,
                            UiNodeKind::Label | UiNodeKind::Button | UiNodeKind::TextInput
                                | UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Slider
                                | UiNodeKind::DragValue | UiNodeKind::Combo | UiNodeKind::Dropdown
                                | UiNodeKind::Selectable | UiNodeKind::Scrollbar
                                | UiNodeKind::ProgressBar
                        ) || text.is_none()
                        {
                            return None;
                        }
                        let horizontal_scroll = (visual.kind == UiNodeKind::TextInput
                            && local_text.is_some())
                        .then_some(self.editing.horizontal_scroll);
                        layout_text(
                            device,
                            queue,
                            font,
                            visual,
                            text.unwrap(),
                            horizontal_scroll,
                        )
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                let mut texts = texts;
                for (visual, text) in &list_box_texts {
                    if let Some(instances) = layout_text(device, queue, font, visual, text, None) {
                        texts.extend(instances);
                    }
                }
                for (visual, text) in &drag_value_texts {
                    if let Some(instances) = layout_text(device, queue, font, visual, text, None) {
                        texts.extend(instances);
                    }
                }
                let mut popup_texts = dropdown_texts.iter().flat_map(|(visual, text)| {
                        layout_text(device, queue, font, visual, text, None).unwrap_or_default()
                }).collect::<Vec<_>>();
                for (index, visual) in self.sampled.iter().enumerate() {
                    if top_layer[index].is_none() {
                        continue;
                    }
                    if let Some(text) = visual.text.as_ref().and_then(text_ref_value)
                        && matches!(visual.kind,
                            UiNodeKind::Label | UiNodeKind::Button | UiNodeKind::TextInput
                            | UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Slider
                            | UiNodeKind::DragValue | UiNodeKind::Combo | UiNodeKind::Dropdown
                            | UiNodeKind::Selectable | UiNodeKind::Scrollbar | UiNodeKind::ProgressBar
                        )
                        && let Some(instances) = layout_text(device, queue, font, visual, text, None)
                    {
                        popup_texts.extend(instances);
                    }
                }
                (texts, popup_texts)
            })
            .unwrap_or_default();
        if texts.len() > self.text_capacity {
            self.text_capacity = texts.len().next_power_of_two();
            self.text_buffer = create_text_buffer(device, self.text_capacity);
        }
        if popup_texts.len() > self.popup_text_capacity {
            self.popup_text_capacity = popup_texts.len().next_power_of_two();
            self.popup_text_buffer = create_text_buffer(device, self.popup_text_capacity);
        }
        if !texts.is_empty() {
            queue.write_buffer(&self.text_buffer, 0, bytemuck::cast_slice(&texts));
            pass.set_pipeline(&self.text_pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            pass.set_bind_group(1, &self.resident_font.as_ref().unwrap().bind_group, &[]);
            pass.set_vertex_buffer(0, self.text_buffer.slice(..));
            pass.draw(0..6, 0..texts.len() as u32);
        }
        if !popup_instances.is_empty() {
            queue.write_buffer(&self.popup_instance_buffer, 0, bytemuck::cast_slice(&popup_instances));
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            pass.set_vertex_buffer(0, self.popup_instance_buffer.slice(..));
            pass.draw(0..6, 0..popup_instances.len() as u32);
        }
        if !popup_texts.is_empty() {
            queue.write_buffer(&self.popup_text_buffer, 0, bytemuck::cast_slice(&popup_texts));
            pass.set_pipeline(&self.text_pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            pass.set_bind_group(1, &self.resident_font.as_ref().unwrap().bind_group, &[]);
            pass.set_vertex_buffer(0, self.popup_text_buffer.slice(..));
            pass.draw(0..6, 0..popup_texts.len() as u32);
        }
    }

    pub(crate) fn last_panel_instance_count(&self) -> usize {
        self.last_panel_instance_count
    }

    fn refresh_plan(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> bool {
        let matches = self.plan_revisions.len() == fragments.len()
            && fragments
                .iter()
                .all(|(id, fragment)| self.plan_revisions.get(id) == Some(&fragment.revision));
        if matches {
            return false;
        }
        let nodes = flatten_fragments(fragments, self.resident_font.as_ref());
        let live: HashSet<_> = nodes.iter().map(|(id, _, _, _)| id.clone()).collect();
        self.current.retain(|id, _| live.contains(id));
        self.active.retain(|id, _| live.contains(id));
        self.plan.clear();
        self.sampled.clear();
        self.instances.clear();
        for (id, parent_id, target, transition) in nodes {
            let instance_index =
                (!matches!(target.kind, UiNodeKind::Image | UiNodeKind::RenderSurface))
                    .then(|| self.instances.len());
            if let Some(instance_index) = instance_index {
                self.instances.push(UiInstance::zeroed());
                debug_assert_eq!(instance_index, self.instances.len() - 1);
            }
            self.sampled.push(target.clone());
            self.plan.push(PlannedNode {
                id,
                parent_id,
                target,
                transition,
                instance_index,
            });
        }
        self.plan_revisions.clear();
        self.plan_revisions.extend(
            fragments
                .iter()
                .map(|(id, fragment)| (id.clone(), fragment.revision)),
        );
        true
    }

    fn append_text_input_overlays(&mut self) {
        let Some(node_path) = self.editing.node_path.as_ref() else {
            return;
        };
        let Some(index) = self.plan.iter().position(|node| &node.id == node_path) else {
            return;
        };
        let visual = &self.sampled[index];
        let Some(font) = self.resident_font.as_ref() else {
            return;
        };
        let range = self.editing.selection_range();
        if !range.is_empty() {
            let start = text_advance(&font.font, &self.editing.committed, range.start)
                - self.editing.horizontal_scroll;
            let end = text_advance(&font.font, &self.editing.committed, range.end)
                - self.editing.horizontal_scroll;
            self.instances.push(overlay_instance(
                UiBounds {
                    x: visual.bounds.x + TEXT_INPUT_INSET + start,
                    y: visual.bounds.y + 3.0,
                    width: (end - start).max(1.0),
                    height: (visual.bounds.height - 6.0).max(0.0),
                },
                input_clip(visual),
                [0.18, 0.62, 0.7, 0.62],
            ));
        }
        if let Some(caret) = self.text_input_ime_rect() {
            self.instances.push(overlay_instance(
                caret,
                input_clip(visual),
                [0.84, 0.98, 0.96, 1.0],
            ));
        }
    }

    fn sample(
        current: &mut HashMap<String, UiVisual>,
        active: &mut HashMap<String, ActiveTransition>,
        id: &str,
        target: &UiVisual,
        transition: Option<&UiTransition>,
        time_seconds: f32,
    ) -> UiVisual {
        if let Some(active_transition) = active.get(id)
            && active_transition.target == *target
        {
            let sampled = sample_transition(active_transition, time_seconds);
            current.insert(id.to_owned(), sampled.clone());
            return sampled;
        }
        let source = current.get(id).cloned();
        let sampled = match transition {
            Some(transition) if transition.duration_ms > 0 => {
                let from = source.unwrap_or_else(|| transition_source(target, transition));
                let next_active = ActiveTransition {
                    from,
                    target: target.clone(),
                    started_at_seconds: time_seconds,
                    transition: transition.clone(),
                };
                let sampled = sample_transition(&next_active, time_seconds);
                active.insert(id.to_owned(), next_active);
                sampled
            }
            _ => target.clone(),
        };
        current.insert(id.to_owned(), sampled.clone());
        sampled
    }

    fn instance(&self, visual: &UiVisual, node_path: &str, time_seconds: f32) -> UiInstance {
        let mut style = if visual.style == UiStyle::default() {
            default_component_style(&visual.kind)
        } else {
            visual.style
        };
        if matches!(visual.kind, UiNodeKind::Checkbox | UiNodeKind::RadioButton)
            && let Some(UiControlPresentation::Toggle { selected }) = &visual.presentation
        {
            style = if *selected {
                UiStyle {
                    background_color: [0.10, 0.25, 0.20, 1.0],
                    border_color: [0.34, 0.80, 0.64, 0.95],
                    border_width: 1.0,
                    corner_radius: 5.0,
                    opacity: style.opacity,
                }
            } else {
                UiStyle {
                    background_color: [0.17, 0.18, 0.20, 1.0],
                    border_color: [0.39, 0.41, 0.45, 0.96],
                    border_width: 1.0,
                    corner_radius: 5.0,
                    opacity: style.opacity,
                }
            };
        }
        let mut fill = style.background_color;
        let bounds = visual.bounds;
        if is_interactive_control(&visual.kind)
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
        if self.focused_control.as_deref() == Some(node_path) {
            fill[0] = (fill[0] * 1.08).min(1.0);
            fill[1] = (fill[1] * 1.08).min(1.0);
            fill[2] = (fill[2] * 1.08).min(1.0);
        }
        UiInstance {
            rect: [bounds.x, bounds.y, bounds.width, bounds.height],
            fill,
            border: style.border_color,
            params: [
                style.border_width,
                style.corner_radius,
                style.opacity,
                visual.clip_radius,
            ],
            clip: [
                visual.clip.x,
                visual.clip.y,
                visual.clip.x + visual.clip.width,
                visual.clip.y + visual.clip.height,
            ],
        }
    }

    fn component_chrome_instances(&self, visual: &UiVisual, node_path: &str) -> Vec<UiInstance> {
        let mut preview = visual.clone();
        if let Some(value) = self.value_previews.get(node_path) {
            preview.presentation = match (&preview.presentation, value) {
                (Some(UiControlPresentation::Numeric { min, max, .. }), UiSemanticPayloadValue::F32 { value }) => {
                    Some(UiControlPresentation::Numeric { value: *value, min: *min, max: *max })
                }
                (Some(UiControlPresentation::Numeric { min, max, .. }), UiSemanticPayloadValue::I32 { value }) => {
                    Some(UiControlPresentation::Numeric { value: *value as f32, min: *min, max: *max })
                }
                (Some(UiControlPresentation::Scroll { .. }), UiSemanticPayloadValue::F32 { value }) => {
                    Some(UiControlPresentation::Scroll { position: *value })
                }
                _ => preview.presentation,
            };
        }
        let instances = component_chrome_instances(&preview);
        instances
    }

    fn dropdown_popup_layout(&self) -> Option<(usize, Vec<UiBounds>)> {
        let node_path = self.open_dropdown.as_ref()?;
        let plan_index = self.plan.iter().position(|node| &node.id == node_path)?;
        let node = &self.plan[plan_index];
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()? else { return None; };
        let row_height = 24.0;
        let margin = 4.0;
        let popup_height = options.len() as f32 * row_height;
        let viewport_height = self.viewport_size[1].max(1) as f32;
        let y = if node.target.bounds.y + node.target.bounds.height + margin + popup_height <= viewport_height - margin {
            node.target.bounds.y + node.target.bounds.height + margin
        } else {
            (node.target.bounds.y - margin - popup_height).max(margin)
        };
        Some((plan_index, (0..options.len()).map(|index| UiBounds {
                    x: node.target.bounds.x,
                    y: y + index as f32 * row_height,
                    width: node.target.bounds.width,
                    height: row_height,
        }).collect()))
    }

    fn dropdown_popup_instances(&self) -> Vec<UiInstance> {
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else { return Vec::new(); };
        let visual = &self.sampled[plan_index];
        let Some(UiControlPresentation::Choice { token, options, .. }) = &visual.presentation else { return Vec::new(); };
        let clip = [0.0, 0.0, self.viewport_size[0].max(1) as f32, self.viewport_size[1].max(1) as f32];
        let panel_y = rows.first().map(|row| row.y - 2.0).unwrap_or(visual.bounds.y);
        let panel_height = rows.last().map(|row| row.y + row.height - panel_y + 2.0).unwrap_or(0.0);
        let mut instances = vec![UiInstance {
            rect: [visual.bounds.x - 2.0, panel_y, visual.bounds.width + 4.0, panel_height],
            fill: [0.045, 0.075, 0.07, 0.99],
            border: [0.42, 0.68, 0.57, 0.96],
            params: [1.0, 4.0, 1.0, 0.0],
            clip,
        }];
        for (row, option) in rows.into_iter().zip(options) {
            instances.push(UiInstance {
                rect: [row.x, row.y, row.width, row.height],
                fill: if option == token { [0.16, 0.35, 0.28, 1.0] } else { [0.075, 0.12, 0.11, 1.0] },
                border: [0.22, 0.40, 0.34, 0.84],
                params: [1.0, 2.0, 1.0, 0.0],
                clip,
            });
        }
        instances
    }

    fn dropdown_option_texts(&self) -> Vec<(UiVisual, String)> {
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else { return Vec::new(); };
        let Some(UiControlPresentation::Choice { options, .. }) = self.sampled[plan_index].presentation.as_ref() else { return Vec::new(); };
        options.iter().zip(rows).map(|(option, row)| {
                let mut visual = self.sampled[plan_index].clone();
                visual.kind = UiNodeKind::Label;
                visual.text = None;
                visual.bounds = row;
            visual.clip = UiBounds { x: 0.0, y: 0.0, width: self.viewport_size[0].max(1) as f32, height: self.viewport_size[1].max(1) as f32 };
                visual.clip_radius = 0.0;
                (visual, option.clone())
        }).collect()
    }

    fn list_box_option_texts(&self) -> Vec<(UiVisual, String)> {
        self.plan.iter().enumerate().flat_map(|(plan_index, node)| {
            if node.target.kind != UiNodeKind::ListBox {
                return Vec::new();
            }
            let Some(UiControlPresentation::Choice { options, .. }) = self.sampled[plan_index].presentation.as_ref() else {
                return Vec::new();
            };
            list_box_rows(self.sampled[plan_index].bounds, options.len()).into_iter().zip(options).map(|(row, option)| {
                let mut visual = self.sampled[plan_index].clone();
                visual.kind = UiNodeKind::Label;
                visual.text = None;
                visual.bounds = row;
                (visual, option.clone())
            }).collect::<Vec<_>>()
        }).collect()
    }

    fn drag_value_texts(&self) -> Vec<(UiVisual, String)> {
        self.plan.iter().enumerate().filter_map(|(plan_index, node)| {
            if node.target.kind != UiNodeKind::DragValue {
                return None;
            }
            let Some(UiControlPresentation::Numeric { value, min: _, max }) = self.sampled[plan_index].presentation.as_ref() else {
                return None;
            };
            let value = match self.value_previews.get(&node.id) {
                Some(UiSemanticPayloadValue::I32 { value }) => *value as f32,
                _ => *value,
            };
            let mut visual = self.sampled[plan_index].clone();
            visual.kind = UiNodeKind::Label;
            visual.text = None;
            visual.bounds = drag_value_bounds(visual.bounds);
            Some((visual, format!("{} / {}", value.round() as i32, max.round() as i32)))
        }).collect()
    }
}

fn list_box_rows(bounds: UiBounds, option_count: usize) -> Vec<UiBounds> {
    if option_count == 0 {
        return Vec::new();
    }
    let inset = 6.0;
    let row_height = ((bounds.height - inset * 2.0) / option_count as f32).max(18.0);
    (0..option_count).map(|index| UiBounds {
        x: bounds.x + inset,
        y: bounds.y + inset + index as f32 * row_height,
        width: (bounds.width - inset * 2.0).max(0.0),
        height: row_height,
    }).collect()
}

fn drag_value_bounds(bounds: UiBounds) -> UiBounds {
    let width = (bounds.width * 0.42).clamp(112.0, 180.0);
    UiBounds {
        x: bounds.x + bounds.width - width - 8.0,
        y: bounds.y + 5.0,
        width,
        height: (bounds.height - 10.0).max(0.0),
    }
}

fn default_component_style(kind: &UiNodeKind) -> UiStyle {
    match kind {
        UiNodeKind::Label => UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
        UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable => UiStyle {
            background_color: [0.10, 0.15, 0.14, 1.0],
            border_color: [0.32, 0.54, 0.47, 0.82],
            border_width: 1.0,
            corner_radius: 5.0,
            opacity: 1.0,
        },
        UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar => UiStyle {
            background_color: [0.09, 0.12, 0.12, 1.0],
            border_color: [0.27, 0.46, 0.40, 0.82],
            border_width: 1.0,
            corner_radius: 5.0,
            opacity: 1.0,
        },
        UiNodeKind::Combo | UiNodeKind::Dropdown | UiNodeKind::ListBox => UiStyle {
            background_color: [0.11, 0.16, 0.15, 1.0],
            border_color: [0.35, 0.56, 0.49, 0.88],
            border_width: 1.0,
            corner_radius: 5.0,
            opacity: 1.0,
        },
        UiNodeKind::ProgressBar => UiStyle {
            background_color: [0.08, 0.10, 0.10, 1.0],
            border_color: [0.32, 0.50, 0.43, 0.70],
            border_width: 1.0,
            corner_radius: 4.0,
            opacity: 1.0,
        },
        _ => UiStyle::default(),
    }
}

fn component_chrome_instances(visual: &UiVisual) -> Vec<UiInstance> {
    let bounds = visual.bounds;
    let center_y = bounds.y + bounds.height * 0.5;
    let mint = [0.34, 0.80, 0.64, 0.95];
    let muted = [0.18, 0.29, 0.26, 1.0];
    let inactive = [0.34, 0.36, 0.40, 1.0];
    let clip = [
        visual.clip.x,
        visual.clip.y,
        visual.clip.x + visual.clip.width,
        visual.clip.y + visual.clip.height,
    ];
    let chrome = |rect: UiBounds, fill: [f32; 4], border: [f32; 4], radius: f32| UiInstance {
        rect: [rect.x, rect.y, rect.width, rect.height],
        fill,
        border,
        params: [1.0, radius, visual.style.opacity, visual.clip_radius],
        clip,
    };
    let selected = matches!(
        &visual.presentation,
        Some(UiControlPresentation::Toggle { selected: true })
            | Some(UiControlPresentation::Choice { selected: true, .. })
    );
    let normalized = match &visual.presentation {
        Some(UiControlPresentation::Numeric { value, min, max }) => {
            ((value - min) / (max - min)).clamp(0.0, 1.0)
        }
        Some(UiControlPresentation::Scroll { position }) => position.clamp(0.0, 1.0),
        Some(UiControlPresentation::Choice { token, options, .. }) => options
            .iter()
            .position(|option| option == token)
            .map(|index| index as f32 / options.len().saturating_sub(1).max(1) as f32)
            .unwrap_or(0.0),
        _ => 0.5,
    };
    let choice_tint = match &visual.presentation {
        Some(UiControlPresentation::Choice { token, .. }) if token == "alpha" => [0.86, 0.59, 0.33, 0.95],
        Some(UiControlPresentation::Choice { token, .. }) if token == "gamma" => [0.48, 0.66, 0.95, 0.95],
        _ => mint,
    };
    match visual.kind {
        UiNodeKind::Checkbox => vec![chrome(
            UiBounds { x: bounds.x + 8.0, y: center_y - 7.0, width: 14.0, height: 14.0 },
            if selected { mint } else { inactive },
            if selected { mint } else { [0.53, 0.55, 0.60, 1.0] },
            3.0,
        )],
        UiNodeKind::RadioButton => vec![chrome(
            UiBounds { x: bounds.x + 8.0, y: center_y - 7.0, width: 14.0, height: 14.0 },
            if selected { mint } else { inactive },
            if selected { mint } else { [0.53, 0.55, 0.60, 1.0] },
            7.0,
        )],
        UiNodeKind::Selectable => vec![chrome(
            UiBounds { x: bounds.x + 5.0, y: bounds.y + 5.0, width: 3.0, height: (bounds.height - 10.0).max(0.0) },
            if selected { mint } else { muted },
            if selected { mint } else { [0.34, 0.54, 0.47, 0.82] },
            1.5,
        )],
        UiNodeKind::Slider => {
            let track = UiBounds { x: bounds.x + bounds.width * 0.57, y: center_y - 2.0, width: bounds.width * 0.34, height: 4.0 };
            vec![
                chrome(track, muted, muted, 2.0),
                chrome(UiBounds { x: track.x + track.width * normalized - 5.0, y: center_y - 6.0, width: 12.0, height: 12.0 }, mint, mint, 6.0),
            ]
        }
        UiNodeKind::DragValue => {
            let well = drag_value_bounds(bounds);
            let progress = UiBounds { x: well.x + 1.0, y: well.y + 1.0, width: ((well.width - 2.0) * normalized).max(0.0), height: (well.height - 2.0).max(0.0) };
            vec![
                chrome(well, muted, choice_tint, 4.0),
                UiInstance {
                    rect: [progress.x, progress.y, progress.width, progress.height],
                    fill: [0.18, 0.52, 0.90, 0.92],
                    border: [0.18, 0.52, 0.90, 0.92],
                    params: [0.0, 3.0, visual.style.opacity, visual.clip_radius],
                    clip,
                },
            ]
        }
        UiNodeKind::Combo | UiNodeKind::Dropdown => vec![chrome(
            UiBounds { x: bounds.x + bounds.width - 27.0, y: center_y - 5.0, width: 16.0, height: 10.0 },
            muted,
            choice_tint,
            3.0,
        )],
        UiNodeKind::ListBox => match &visual.presentation {
            Some(UiControlPresentation::Choice { token, options, .. }) => list_box_rows(bounds, options.len()).into_iter().zip(options).map(|(row, option)| {
                let active = option == token;
                chrome(row, if active { [0.16, 0.35, 0.28, 1.0] } else { [0.075, 0.12, 0.11, 1.0] }, if active { choice_tint } else { muted }, 3.0)
            }).collect(),
            _ => Vec::new(),
        },
        UiNodeKind::Scrollbar => {
            let track = UiBounds { x: bounds.x + 10.0, y: center_y - 3.0, width: (bounds.width - 20.0).max(0.0), height: 6.0 };
            vec![
                chrome(track, muted, muted, 3.0),
                chrome(UiBounds { x: track.x + (track.width - track.width * 0.28) * normalized, y: center_y - 5.0, width: track.width * 0.28, height: 10.0 }, mint, mint, 5.0),
            ]
        }
        _ => Vec::new(),
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
    resident_surfaces: Vec<(String, wgpu::Texture)>,
) -> Vec<u8> {
    let mut renderer = UiWgpuRenderer::new(device, format);
    for asset in resident_images {
        if asset.asset.kind == "image" {
            renderer.preload_image(device, queue, asset).unwrap();
        }
        if asset.asset.kind == "font" {
            renderer.preload_font(device, queue, asset).unwrap();
        }
    }
    for (target_id, texture) in resident_surfaces {
        renderer.register_render_surface(device, target_id, texture);
    }
    render_renderer_offscreen_for_test(
        &mut renderer,
        device,
        queue,
        format,
        fragments,
        size,
        time_seconds,
    )
}

#[cfg(test)]
pub(crate) fn render_renderer_offscreen_for_test(
    renderer: &mut UiWgpuRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    size: [u32; 2],
    time_seconds: f32,
) -> Vec<u8> {
    let row_bytes = size[0] * 4;
    let padded_bytes_per_row =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
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
        size: (padded_bytes_per_row * size[1]) as u64,
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
                bytes_per_row: Some(padded_bytes_per_row),
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
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    receiver.recv().unwrap().unwrap();
    let pixels = readback.slice(..).get_mapped_range().unwrap();
    let mut tight = Vec::with_capacity((row_bytes * size[1]) as usize);
    for row in pixels.chunks_exact(padded_bytes_per_row as usize) {
        tight.extend_from_slice(&row[..row_bytes as usize]);
    }
    tight
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
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes_per_row = size[0] * 4;
    assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &vec![0xff; (bytes_per_row * size[1]) as usize],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-hit-id-readback"),
        size: (bytes_per_row * size[1]) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("neon3-ui-hit-id-test-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("neon3-ui-hit-id-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.draw_hit_id(device, queue, &mut pass, fragments, size, 1.0);
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
                bytes_per_row: Some(bytes_per_row),
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
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    receiver.recv().unwrap().unwrap();
    let bytes = readback.slice(..).get_mapped_range().unwrap();
    bytes
        .chunks_exact(4)
        .map(|pixel| u32::from_ne_bytes(pixel.try_into().unwrap()))
        .collect()
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
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-hit-instances"),
        size: (capacity * std::mem::size_of::<UiHitInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_image_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-image-instances"),
        size: (capacity * std::mem::size_of::<UiImageInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_text_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-text-instances"),
        size: (capacity * std::mem::size_of::<UiTextInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn text_ref_value(text: &TextRef) -> Option<&str> {
    match text {
        TextRef::Key { key, .. } => (!key.trim().is_empty()).then_some(key.as_str()),
        TextRef::Literal { value } => (!value.is_empty()).then_some(value.as_str()),
    }
}

fn ensure_glyph(
    _device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut ResidentFont,
    ch: char,
) -> Result<AtlasGlyph, &'static str> {
    if let Some(glyph) = font.glyphs.get(&ch).copied() {
        return Ok(glyph);
    }
    let (metrics, bitmap) = font.font.rasterize(ch, FONT_RASTER_SIZE);
    let width = metrics.width as u32;
    let height = metrics.height as u32;
    if width == 0 || height == 0 {
        let glyph = AtlasGlyph {
            uv: [0.0, 0.0, 0.0, 0.0],
            width: 0.0,
            height: 0.0,
            xmin: metrics.xmin as f32,
            plane_min_y: 0.0,
            advance: metrics.advance_width,
        };
        font.glyphs.insert(ch, glyph);
        return Ok(glyph);
    }
    let padding = 1;
    if font.next_x + width + padding >= FONT_ATLAS_SIZE {
        font.next_x = 1;
        font.next_y = font.next_y.saturating_add(font.row_height + padding);
        font.row_height = 0;
    }
    if font.next_y + height + padding >= FONT_ATLAS_SIZE {
        return Err("font_atlas_full");
    }
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
        wgpu::TexelCopyTextureInfo {
            texture: &font._atlas,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &upload,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded_bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    font.next_x = x + width + padding;
    font.row_height = font.row_height.max(height);
    let atlas = FONT_ATLAS_SIZE as f32;
    // Keep the geometric baseline offset instead of reconstructing it from rounded bitmap pixels.
    let plane_min_y = -metrics.bounds.height - metrics.bounds.ymin;
    let glyph = AtlasGlyph {
        uv: [
            x as f32 / atlas,
            y as f32 / atlas,
            width as f32 / atlas,
            height as f32 / atlas,
        ],
        width: width as f32,
        height: height as f32,
        xmin: metrics.xmin as f32,
        plane_min_y,
        advance: metrics.advance_width,
    };
    font.glyphs.insert(ch, glyph);
    Ok(glyph)
}

fn text_clip(visual: &UiVisual) -> Option<[f32; 4]> {
    let clip = if visual.kind == UiNodeKind::TextInput {
        input_clip(visual)
    } else {
        visual.clip
    };
    let left = visual.bounds.x.max(clip.x);
    let top = visual.bounds.y.max(clip.y);
    let right = (visual.bounds.x + visual.bounds.width).min(clip.x + clip.width);
    let bottom = (visual.bounds.y + visual.bounds.height).min(clip.y + clip.height);
    (left < right && top < bottom).then_some([left, top, right, bottom])
}

fn input_clip(visual: &UiVisual) -> UiBounds {
    let left = visual.bounds.x.max(visual.clip.x);
    let top = visual.bounds.y.max(visual.clip.y);
    let right = (visual.bounds.x + visual.bounds.width).min(visual.clip.x + visual.clip.width);
    let bottom = (visual.bounds.y + visual.bounds.height).min(visual.clip.y + visual.clip.height);
    UiBounds {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

fn layout_text(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut ResidentFont,
    visual: &UiVisual,
    text: &str,
    horizontal_scroll: Option<f32>,
) -> Option<Vec<UiTextInstance>> {
    let clip = text_clip(visual)?;
    let mut lines = Vec::<Vec<AtlasGlyph>>::new();
    let mut line = Vec::<AtlasGlyph>::new();
    let mut line_width = 0.0;
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut line));
            line_width = 0.0;
            continue;
        }
        let glyph = ensure_glyph(device, queue, font, ch).ok()?;
        if !line.is_empty() && line_width + glyph.advance > visual.bounds.width {
            lines.push(std::mem::take(&mut line));
            line_width = 0.0;
        }
        line_width += glyph.advance;
        line.push(glyph);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }

    let block_height = font.line_height * lines.len() as f32;
    let top = visual.bounds.y + ((visual.bounds.height - block_height).max(0.0) * 0.5);
    let mut result = Vec::new();
    for (line_index, glyphs) in lines.into_iter().enumerate() {
        let advance = glyphs.iter().map(|glyph| glyph.advance).sum::<f32>();
        let mut x = if visual.kind == UiNodeKind::Button {
            visual.bounds.x + ((visual.bounds.width - advance).max(0.0) * 0.5)
        } else {
            visual.bounds.x
                + if visual.kind == UiNodeKind::TextInput {
                    TEXT_INPUT_INSET - horizontal_scroll.unwrap_or(0.0)
                } else if matches!(
                    visual.kind,
                    UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
                ) {
                    30.0
                } else {
                    10.0
                }
        };
        let baseline = top + font.ascent + line_index as f32 * font.line_height;
        for glyph in glyphs {
            result.push(UiTextInstance {
                rect: [
                    x + glyph.xmin,
                    baseline + glyph.plane_min_y,
                    glyph.width,
                    glyph.height,
                ],
                color: [0.86, 0.95, 0.98, visual.style.opacity],
                clip,
                uv: glyph.uv,
            });
            x += glyph.advance;
        }
    }
    Some(result)
}

fn text_advance(font: &fontdue::Font, value: &str, char_count: usize) -> f32 {
    value
        .chars()
        .take(char_count)
        .map(|ch| font.metrics(ch, FONT_RASTER_SIZE).advance_width)
        .sum()
}

fn caret_index_for_x(font: &fontdue::Font, value: &str, x: f32) -> usize {
    let mut advance = 0.0;
    for (index, ch) in value.chars().enumerate() {
        let next = advance + font.metrics(ch, FONT_RASTER_SIZE).advance_width;
        if x < (advance + next) * 0.5 {
            return index;
        }
        advance = next;
    }
    value.chars().count()
}

fn overlay_instance(bounds: UiBounds, clip: UiBounds, color: [f32; 4]) -> UiInstance {
    UiInstance {
        rect: [bounds.x, bounds.y, bounds.width, bounds.height],
        fill: color,
        border: [0.0; 4],
        params: [0.0, 0.0, 1.0, 0.0],
        clip: [clip.x, clip.y, clip.x + clip.width, clip.y + clip.height],
    }
}

fn top_layer_roots(plan: &[PlannedNode], indices: &HashMap<&str, usize>) -> Vec<Option<usize>> {
    let mut roots = vec![None; plan.len()];
    for (index, node) in plan.iter().enumerate() {
        roots[index] = if matches!(node.target.kind, UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog) {
            Some(index)
        } else {
            node.parent_id.as_deref().and_then(|parent| indices.get(parent).copied()).and_then(|parent| roots[parent])
        };
    }
    roots
}

fn flatten_fragments(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    font: Option<&ResidentFont>,
) -> Vec<(String, Option<String>, UiVisual, Option<UiTransition>)> {
    let presentations = collect_control_presentations(fragments);
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
            None,
            None,
            font,
            None,
            false,
        );
    }
    for (node_path, _, visual, _) in &mut result {
        visual.presentation = presentations.get(node_path).cloned();
        if let Some(UiControlPresentation::Numeric { value, .. }) = &visual.presentation
            && matches!(visual.kind, UiNodeKind::Slider | UiNodeKind::DragValue)
            && let Some(TextRef::Literal { value: label }) = &visual.text
        {
            visual.text = Some(TextRef::Literal {
                value: if visual.kind == UiNodeKind::DragValue {
                    label.clone()
                } else {
                    format!("{label}: {value:.2}")
                },
            });
        }
        let Some(UiControlPresentation::Choice { token, options, .. }) = &visual.presentation else {
            continue;
        };
        let label = match &visual.text {
            Some(TextRef::Literal { value }) => value.clone(),
            _ => String::new(),
        };
        visual.text = match visual.kind {
            UiNodeKind::Combo | UiNodeKind::Dropdown => Some(TextRef::Literal {
                value: format!("{label}: {token}"),
            }),
            UiNodeKind::ListBox => Some(TextRef::Literal {
                value: options.join("\n"),
            }),
            _ => visual.text.clone(),
        };
    }
    result
}

fn collect_control_presentations(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
) -> HashMap<String, UiControlPresentation> {
    let mut presentations = HashMap::new();
    for fragment in fragments.values() {
        let node_paths = collect_node_paths(&fragment.fragment_id.0, &fragment.root);
        for effect in &fragment.effects {
            if let neon_ui_schema::UiEffect::ControlPresentation { node_id, state } = effect
                && let Some(node_path) = node_paths.get(&node_id.0)
            {
                presentations.insert(node_path.clone(), state.clone());
            }
        }
    }
    presentations
}

fn collect_hit_declarations(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
) -> HashMap<String, UiHitBinding> {
    let mut declarations = HashMap::new();
    for fragment in fragments.values() {
        let node_paths = collect_node_paths(&fragment.fragment_id.0, &fragment.root);
        for effect in &fragment.effects {
            if let neon_ui_schema::UiEffect::BoundSemanticIntent { node_id, intent } = effect {
                let Some(node_path) = node_paths.get(&node_id.0) else {
                    continue;
                };
                declarations.insert(
                    node_path.clone(),
                    UiHitBinding {
                        node_path: node_path.clone(),
                        fragment: UiFragmentRevision {
                            id: fragment.fragment_id.clone(),
                            revision: fragment.revision,
                        },
                        intent: Some(intent.clone()),
                        text_input: None,
                    },
                );
            }
        }
    }
    declarations
}

/// Hit bindings use the same fragment-scoped identity emitted by `flatten_node`.
/// Node IDs are unique within a fragment, so hierarchy must not be added here.
fn collect_node_paths(fragment_id: &str, root: &UiNode) -> HashMap<String, String> {
    fn visit(node: &UiNode, fragment_id: &str, paths: &mut HashMap<String, String>) {
        paths.insert(
            node.node_id.0.clone(),
            format!("{fragment_id}/{}", node.node_id.0),
        );
        for child in &node.children {
            visit(child, fragment_id, paths);
        }
    }

    let mut paths = HashMap::new();
    visit(root, fragment_id, &mut paths);
    paths
}

fn flatten_node(
    out: &mut Vec<(String, Option<String>, UiVisual, Option<UiTransition>)>,
    fragment_id: &str,
    node: &UiNode,
    parent_offset: [f32; 2],
    inherited_clip: Option<UiBounds>,
    inherited_clip_radius: Option<f32>,
    parent_id: Option<&str>,
    font: Option<&ResidentFont>,
    assigned_size: Option<[f32; 2]>,
    inherited_top_layer: bool,
) {
    let node_layout = node.layout.unwrap_or_default();
    let bounds = UiBounds {
        x: parent_offset[0] + node.bounds.x,
        y: parent_offset[1] + node.bounds.y,
        width: assigned_size.map_or_else(
            || resolved_dimension(node.bounds.width, node, &node_layout, font, false),
            |size| size[0],
        ),
        height: assigned_size.map_or_else(
            || resolved_dimension(node.bounds.height, node, &node_layout, font, true),
            |size| size[1],
        ),
    };
    let top_layer = inherited_top_layer || matches!(node.kind, UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog);
    let own_clip = if top_layer {
        None
    } else { match node_layout.clip {
            UiClipPolicy::None => inherited_clip,
            UiClipPolicy::Bounds | UiClipPolicy::Rounded | UiClipPolicy::Scroll => {
                Some(intersect_clip(inherited_clip, bounds))
            }
    }};
    let own_clip_radius = if top_layer { None } else { match node_layout.clip {
            UiClipPolicy::Rounded => Some(node.style.corner_radius),
            UiClipPolicy::None => inherited_clip_radius,
            UiClipPolicy::Bounds | UiClipPolicy::Scroll => None,
    }};
    let effective_clip = own_clip.unwrap_or(UiBounds {
        x: -1_000_000.0,
        y: -1_000_000.0,
        width: 2_000_000.0,
        height: 2_000_000.0,
    });
    if node.visible && node.style.opacity > 0.0 {
        let node_path = format!("{fragment_id}/{}", node.node_id.0);
        out.push((
            node_path.clone(),
            parent_id.map(str::to_owned),
            UiVisual {
                bounds,
                style: node.style,
                kind: node.kind.clone(),
                enabled: node.enabled,
                clip: effective_clip,
                clip_radius: own_clip_radius.unwrap_or(0.0),
                image: node.image.clone(),
                surface: node.surface.clone(),
                text: node.text.clone(),
                presentation: None,
            },
            node.enter_transition.clone(),
        ));
    }
    let inner = UiBounds {
        x: bounds.x + node_layout.padding[3],
        y: bounds.y + node_layout.padding[0],
        width: (bounds.width - node_layout.padding[1] - node_layout.padding[3]).max(0.0),
        height: (bounds.height - node_layout.padding[0] - node_layout.padding[2]).max(0.0),
    };
    let child_bounds = resolve_children(node, bounds, node_layout, inner, font);
    for (child, child_bounds) in node.children.iter().zip(child_bounds) {
        let offset = [
            child_bounds.x - child.bounds.x,
            child_bounds.y - child.bounds.y,
        ];
        let node_path = format!("{fragment_id}/{}", node.node_id.0);
        flatten_node(
            out,
            fragment_id,
            child,
            offset,
            own_clip,
            own_clip_radius,
            Some(&node_path),
            font,
            Some([child_bounds.width, child_bounds.height]),
            top_layer,
        );
    }
}

fn resolved_dimension(
    declared: f32,
    node: &UiNode,
    layout: &UiLayout,
    font: Option<&ResidentFont>,
    height: bool,
) -> f32 {
    let intrinsic = intrinsic_size(node, font);
    let mut value = if declared > 0.0 {
        declared
    } else if height {
        intrinsic[1]
    } else {
        intrinsic[0]
    };
    if let Some([width, height_value]) = layout.preferred_size {
        value = if height { height_value } else { width };
    }
    if let Some([width, height_value]) = layout.min_size {
        value = value.max(if height { height_value } else { width });
    }
    if let Some([width, height_value]) = layout.max_size {
        value = value.min(if height { height_value } else { width });
    }
    value
}

fn intrinsic_size(node: &UiNode, font: Option<&ResidentFont>) -> [f32; 2] {
    let Some(text) = node.text.as_ref().and_then(text_ref_value) else {
        return [0.0, 0.0];
    };
    let line_height = font.map_or(FONT_RASTER_SIZE, |font| font.line_height);
    let width = font.map_or_else(
        || text.chars().count() as f32 * FONT_RASTER_SIZE * 0.5,
        |font| {
            text.chars()
                .map(|ch| font.font.metrics(ch, FONT_RASTER_SIZE).advance_width)
                .sum()
        },
    );
    [
        width
            + if node.kind == UiNodeKind::TextInput {
                TEXT_INPUT_INSET * 2.0
            } else {
                0.0
            },
        line_height,
    ]
}

fn is_interactive_control(kind: &UiNodeKind) -> bool {
    matches!(kind,
        UiNodeKind::Button | UiNodeKind::TextInput | UiNodeKind::Checkbox | UiNodeKind::RadioButton
        | UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Combo | UiNodeKind::Dropdown
        | UiNodeKind::Selectable | UiNodeKind::ListBox | UiNodeKind::Scrollbar)
}

fn resolve_children(
    node: &UiNode,
    bounds: UiBounds,
    parent_layout: UiLayout,
    inner: UiBounds,
    font: Option<&ResidentFont>,
) -> Vec<UiBounds> {
    if !matches!(parent_layout.mode, UiLayoutMode::Row | UiLayoutMode::Column) {
        return node
            .children
            .iter()
            .map(|child| UiBounds {
                x: bounds.x + child.bounds.x - parent_layout.scroll_offset[0],
                y: bounds.y + child.bounds.y - parent_layout.scroll_offset[1],
                width: resolved_dimension(
                    child.bounds.width,
                    child,
                    &child.layout.unwrap_or_default(),
                    font,
                    false,
                ),
                height: resolved_dimension(
                    child.bounds.height,
                    child,
                    &child.layout.unwrap_or_default(),
                    font,
                    true,
                ),
            })
            .collect();
    }
    let row = parent_layout.mode == UiLayoutMode::Row;
    let available = if row { inner.width } else { inner.height };
    let mut main_sizes = node
        .children
        .iter()
        .map(|child| {
            let layout = child.layout.unwrap_or_default();
            layout.flex_basis.unwrap_or_else(|| {
                resolved_dimension(
                    if row {
                        child.bounds.width
                    } else {
                        child.bounds.height
                    },
                    child,
                    &layout,
                    font,
                    !row,
                )
            })
        })
        .collect::<Vec<_>>();
    let outer = node
        .children
        .iter()
        .map(|child| {
            let margin = child.layout.unwrap_or_default().margin;
            if row {
                margin[3] + margin[1]
            } else {
                margin[0] + margin[2]
            }
        })
        .collect::<Vec<_>>();
    let occupied = main_sizes.iter().sum::<f32>()
        + outer.iter().sum::<f32>()
        + parent_layout.gap * node.children.len().saturating_sub(1) as f32;
    let free = available - occupied;
    if free > 0.0 {
        let total = node
            .children
            .iter()
            .map(|child| child.layout.unwrap_or_default().flex_grow)
            .sum::<f32>();
        if total > 0.0 {
            for (size, child) in main_sizes.iter_mut().zip(&node.children) {
                *size += free * child.layout.unwrap_or_default().flex_grow / total;
            }
        }
    } else if free < 0.0 {
        let total = node
            .children
            .iter()
            .zip(&main_sizes)
            .map(|(child, size)| child.layout.unwrap_or_default().flex_shrink * *size)
            .sum::<f32>();
        if total > 0.0 {
            for (size, child) in main_sizes.iter_mut().zip(&node.children) {
                *size = (*size
                    + free * child.layout.unwrap_or_default().flex_shrink * *size / total)
                    .max(0.0);
            }
        }
    }
    let used = main_sizes.iter().sum::<f32>()
        + outer.iter().sum::<f32>()
        + parent_layout.gap * node.children.len().saturating_sub(1) as f32;
    let remaining = (available - used).max(0.0);
    let count = node.children.len() as f32;
    let (mut cursor, gap) = match parent_layout.justify_content {
        UiJustifyContent::Start => (0.0, parent_layout.gap),
        UiJustifyContent::Center => (remaining * 0.5, parent_layout.gap),
        UiJustifyContent::End => (remaining, parent_layout.gap),
        UiJustifyContent::SpaceBetween if count > 1.0 => {
            (0.0, parent_layout.gap + remaining / (count - 1.0))
        }
        UiJustifyContent::SpaceAround if count > 0.0 => (
            remaining / count * 0.5,
            parent_layout.gap + remaining / count,
        ),
        UiJustifyContent::SpaceEvenly if count > 0.0 => (
            remaining / (count + 1.0),
            parent_layout.gap + remaining / (count + 1.0),
        ),
        _ => (0.0, parent_layout.gap),
    };
    node.children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            let layout = child.layout.unwrap_or_default();
            let margin = layout.margin;
            let cross_available = if row { inner.height } else { inner.width };
            let declared_cross = if row {
                child.bounds.height
            } else {
                child.bounds.width
            };
            let mut cross_size = resolved_dimension(declared_cross, child, &layout, font, row);
            let align = layout.align_self.unwrap_or(parent_layout.align_items);
            if align == UiAlignItems::Stretch && declared_cross == 0.0 {
                cross_size = (cross_available
                    - if row {
                        margin[0] + margin[2]
                    } else {
                        margin[3] + margin[1]
                    })
                .max(0.0);
            }
            let cross_margin_start = if row { margin[0] } else { margin[3] };
            let cross_margin_end = if row { margin[2] } else { margin[1] };
            let cross_offset = match align {
                UiAlignItems::Start | UiAlignItems::Stretch => cross_margin_start,
                UiAlignItems::Center => {
                    (cross_available - cross_size - cross_margin_start - cross_margin_end).max(0.0)
                        * 0.5
                        + cross_margin_start
                }
                UiAlignItems::End => (cross_available - cross_size - cross_margin_end).max(0.0),
            };
            let main_margin_start = if row { margin[3] } else { margin[0] };
            let main_margin_end = if row { margin[1] } else { margin[2] };
            cursor += main_margin_start;
            let result = if row {
                UiBounds {
                    x: inner.x + cursor - parent_layout.scroll_offset[0],
                    y: inner.y + cross_offset - parent_layout.scroll_offset[1],
                    width: main_sizes[index],
                    height: cross_size,
                }
            } else {
                UiBounds {
                    x: inner.x + cross_offset - parent_layout.scroll_offset[0],
                    y: inner.y + cursor - parent_layout.scroll_offset[1],
                    width: cross_size,
                    height: main_sizes[index],
                }
            };
            cursor += main_sizes[index] + main_margin_end + gap;
            result
        })
        .collect()
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
        clip_radius: target.clip_radius,
        image: target.image.clone(),
        surface: target.surface.clone(),
        text: target.text.clone(),
        presentation: target.presentation.clone(),
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
        clip_radius: active.target.clip_radius,
        image: active.target.image.clone(),
        surface: active.target.surface.clone(),
        text: active.target.text.clone(),
        presentation: active.target.presentation.clone(),
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

fn clamp_drag_offset(offset: [f32; 2], source: UiBounds, boundary: Option<UiBounds>) -> [f32; 2] {
    let Some(boundary) = boundary else {
        return offset;
    };
    let relative = [source.x - boundary.x, source.y - boundary.y];
    let minimum = [-relative[0], -relative[1]];
    let maximum = [
        (boundary.width - source.width - relative[0]).max(minimum[0]),
        (boundary.height - source.height - relative[1]).max(minimum[1]),
    ];
    [
        offset[0].clamp(minimum[0], maximum[0]),
        offset[1].clamp(minimum[1], maximum[1]),
    ]
}

fn contains(bounds: UiBounds, position: [f32; 2]) -> bool {
    position[0] >= bounds.x
        && position[0] <= bounds.x + bounds.width
        && position[1] >= bounds.y
        && position[1] <= bounds.y + bounds.height
}

fn intersect_clip(inherited: Option<UiBounds>, bounds: UiBounds) -> UiBounds {
    let Some(parent) = inherited else {
        return bounds;
    };
    let left = parent.x.max(bounds.x);
    let top = parent.y.max(bounds.y);
    let right = (parent.x + parent.width).min(bounds.x + bounds.width);
    let bottom = (parent.y + parent.height).min(bounds.y + bounds.height);
    UiBounds {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus, ServiceName};
    use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow, UiRuntime};
    use neon_ui_schema::{
        TextRef, UiAlignItems, UiCommand, UiEffect, UiFragmentId, UiFragmentSubmission,
        UiIntent, UiJustifyContent, UiLayout, UiNodeId, UiSemanticEvent, UiSemanticEventType,
        UiTransitionState,
    };
    use serde_json::json;
    use std::sync::Mutex;

    static GPU_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fixture_font() -> AssetBytes {
        AssetBytes {
            asset: AssetRef {
                project_id: "fixture-project".into(),
                asset_id: 82,
                revision: Revision(5),
                kind: "font".into(),
            },
            media_type: "font/ttf".into(),
            width: None,
            height: None,
            bytes: include_bytes!("../../../assets/fonts/SarasaUiSC-Light.ttf").to_vec(),
        }
    }

    #[test]
    fn text_edit_buffer_uses_character_boundaries_and_keeps_preedit_local() {
        let mut editing = UiTextEditingState::default();
        editing.focus(
            UiTextInputBinding {
                node_path: "surface/input".into(),
                max_length: 16,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            },
            "地形A".into(),
        );
        editing.move_cursor(-1, false);
        editing.set_preedit("测试".into());
        assert_eq!(editing.commit("测试"), Some("地形测试A".into()));
        assert_eq!(editing.backspace(), Some("地形测A".into()));
        assert_eq!(editing.backspace(), Some("地形A".into()));
        assert_eq!(editing.delete(), Some("地形".into()));
        assert_eq!(editing.cursor, 2);
    }

    #[test]
    fn drag_boundary_offsets_clamp_to_parent_or_surface_and_allow_free_motion() {
        let source = UiBounds {
            x: 20.0,
            y: 30.0,
            width: 40.0,
            height: 20.0,
        };
        let parent = UiBounds {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
        };
        let surface = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 96.0,
        };
        assert_eq!(
            clamp_drag_offset([-100.0, 100.0], source, Some(parent)),
            [-10.0, 50.0]
        );
        assert_eq!(
            clamp_drag_offset([100.0, 100.0], source, Some(surface)),
            [68.0, 46.0]
        );
        assert_eq!(
            clamp_drag_offset([100.0, -100.0], source, None),
            [100.0, -100.0]
        );
    }

    #[test]
    fn release_resolves_each_sibling_drop_target_for_the_active_drag() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-drag-target-switching");
        let source = UiVisual {
            bounds: UiBounds {
                x: 8.0,
                y: 8.0,
                width: 20.0,
                height: 20.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 160.0,
                height: 80.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
        };
        let target_a = UiVisual {
            bounds: UiBounds {
                x: 48.0,
                y: 8.0,
                width: 40.0,
                height: 40.0,
            },
            ..source.clone()
        };
        let target_b = UiVisual {
            bounds: UiBounds {
                x: 104.0,
                y: 8.0,
                width: 40.0,
                height: 40.0,
            },
            ..source.clone()
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan = vec![
            PlannedNode {
                id: "fixture/source".into(),
                parent_id: None,
                target: source.clone(),
                transition: None,
                instance_index: None,
            },
            PlannedNode {
                id: "fixture/target-a".into(),
                parent_id: None,
                target: target_a.clone(),
                transition: None,
                instance_index: None,
            },
            PlannedNode {
                id: "fixture/target-b".into(),
                parent_id: None,
                target: target_b.clone(),
                transition: None,
                instance_index: None,
            },
        ];
        renderer.sampled = vec![source, target_a, target_b];
        let fragment_id = neon_ui_schema::UiFragmentId("fixture".into());
        let intent = UiIntent::Invoke {
            action: "test.drop".into(),
            params: serde_json::json!({}),
        };
        let fragments = HashMap::from([(
            fragment_id.clone(),
            UiFragment {
                fragment_id: fragment_id.clone(),
                revision: Revision(1),
                root: node(),
                effects: vec![
                    neon_ui_schema::UiEffect::DragBinding {
                        binding: UiDragBinding {
                            key: "card-drag".into(),
                            source_node_id: UiNodeId("source".into()),
                            axis: UiDragAxis::Both,
                            snap: 0.0,
                            threshold: 1.0,
                            boundary: UiDragBoundary::Free,
                        },
                    },
                    neon_ui_schema::UiEffect::DropBinding {
                        binding: neon_ui_schema::UiDropBinding {
                            key: "drop-a".into(),
                            target_node_id: UiNodeId("target-a".into()),
                            accepts_drag_key: "card-drag".into(),
                            placement: UiDropPlacement::Into,
                            presentation_template_key: None,
                            intent: intent.clone(),
                        },
                    },
                    neon_ui_schema::UiEffect::DropBinding {
                        binding: neon_ui_schema::UiDropBinding {
                            key: "drop-b".into(),
                            target_node_id: UiNodeId("target-b".into()),
                            accepts_drag_key: "card-drag".into(),
                            placement: UiDropPlacement::Into,
                            presentation_template_key: None,
                            intent: intent.clone(),
                        },
                    },
                ],
            },
        )]);
        let binding = match &fragments[&fragment_id].effects[0] {
            neon_ui_schema::UiEffect::DragBinding { binding } => binding.clone(),
            _ => unreachable!(),
        };
        for (pointer, target) in [([60.0, 20.0], "target-a"), ([116.0, 20.0], "target-b")] {
            renderer.drag = Some(RendererDrag {
                binding: binding.clone(),
                fragment: UiFragmentRevision {
                    id: fragment_id.clone(),
                    revision: Revision(1),
                },
                source_path: "fixture/source".into(),
                source_bounds: UiBounds {
                    x: 8.0,
                    y: 8.0,
                    width: 20.0,
                    height: 20.0,
                },
                boundary_bounds: None,
                start: [8.0, 8.0],
                origin: [0.0, 0.0],
                moved: true,
            });
            renderer.set_pointer_position(pointer);
            assert_eq!(
                renderer
                    .finish_drag_at_pointer(&fragments)
                    .unwrap()
                    .target_key,
                target
            );
        }
    }

    #[test]
    fn text_edit_selection_replaces_character_safe_ranges() {
        let mut editing = UiTextEditingState::default();
        editing.focus(
            UiTextInputBinding {
                node_path: "surface/input".into(),
                max_length: 16,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            },
            "A地形B".into(),
        );
        editing.move_cursor(-2, false);
        editing.move_cursor(-2, true);
        assert_eq!(editing.selection_range(), 0..2);
        assert_eq!(editing.commit("测试"), Some("测试形B".into()));
        assert_eq!(editing.committed, "测试形B");
        assert_eq!(editing.selection_anchor, editing.cursor);
    }

    #[test]
    fn text_input_pointer_hit_is_available_before_gpu_readback() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-text-input-pointer-hit");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: "input/field".into(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("input".into()),
                    revision: Revision(1),
                },
                intent: None,
                text_input: Some(UiTextInputBinding {
                    node_path: "input/field".into(),
                    max_length: 256,
                    bounds: UiBounds {
                        x: 20.0,
                        y: 30.0,
                        width: 100.0,
                        height: 32.0,
                    },
                }),
            },
        );
        renderer.set_pointer_position([24.0, 40.0]);
        assert_eq!(
            renderer.text_input_at_pointer().unwrap().node_path,
            "input/field"
        );
        renderer.set_pointer_position([124.0, 40.0]);
        assert!(renderer.text_input_at_pointer().is_none());
    }

    #[test]
    fn generic_control_focus_is_local_and_skips_disabled_controls() {
        let _gpu_test = GPU_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-component-focus");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let visual = UiVisual { bounds: UiBounds { x: 10.0, y: 10.0, width: 40.0, height: 20.0 }, style: UiStyle::default(), kind: UiNodeKind::Slider, enabled: true, clip: UiBounds { x: 0.0, y: 0.0, width: 100.0, height: 100.0 }, clip_radius: 0.0, image: None, surface: None, text: None, presentation: None };
        renderer.plan.push(PlannedNode { id: "gallery/slider".into(), parent_id: None, target: visual, transition: None, instance_index: None });
        renderer.hit_bindings.insert(1, UiHitBinding { node_path: "gallery/slider".into(), fragment: UiFragmentRevision { id: UiFragmentId("gallery".into()), revision: Revision(1) }, intent: None, text_input: None });
        renderer.set_pointer_position([20.0, 16.0]);
        assert_eq!(renderer.hit_id_at_pointer(), Some(1));
        assert!(renderer.focus_control_at_pointer());
        assert_eq!(renderer.focused_control.as_deref(), Some("gallery/slider"));
        renderer.plan[0].target.enabled = false;
        renderer.focused_control = None;
        assert_eq!(renderer.hit_id_at_pointer(), None);
        assert!(!renderer.focus_control_at_pointer());
        assert!(renderer.focused_control.is_none());
    }

    #[test]
    fn parent_transition_moves_child_panel_from_the_same_sampled_origin() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-subtree-transition");
        let mut root = node();
        root.bounds = UiBounds {
            x: 20.0,
            y: 8.0,
            width: 28.0,
            height: 28.0,
        };
        root.style = UiStyle {
            background_color: [0.0; 4],
            border_color: [0.0; 4],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        root.enter_transition = Some(UiTransition {
            delay_ms: 0,
            duration_ms: 200,
            easing: UiEasing::Linear,
            from: UiTransitionState {
                bounds: Some(UiBounds {
                    x: 0.0,
                    y: 8.0,
                    width: 28.0,
                    height: 28.0,
                }),
                ..UiTransitionState::default()
            },
        });
        root.children.push(UiNode {
            node_id: UiNodeId("child".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 4.0,
                y: 4.0,
                width: 8.0,
                height: 8.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [1.0, 0.0, 0.0, 1.0],
                border_color: [0.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: Vec::new(),
        });
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(
                UiFragmentId("subtree".into()),
                UiFragment {
                    fragment_id: UiFragmentId("subtree".into()),
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [48, 48],
            1.0,
            &[],
            Vec::new(),
        );
        assert!(
            pixels[4 * (14 * 48 + 6) + 3] > 0,
            "child must render at the parent's transition origin"
        );
        assert_eq!(
            pixels[4 * (14 * 48 + 26) + 3],
            0,
            "child must not jump to the parent's final position"
        );
    }

    #[test]
    fn component_controls_render_and_hit_test_without_exposing_renderer_ids() {
        let _gpu_test = GPU_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-component-control-hits");
        let kinds = [UiNodeKind::Checkbox, UiNodeKind::RadioButton, UiNodeKind::Slider, UiNodeKind::DragValue, UiNodeKind::Combo, UiNodeKind::Dropdown, UiNodeKind::Selectable, UiNodeKind::ListBox, UiNodeKind::Scrollbar];
        let mut root = node();
        root.node_id = UiNodeId("gallery".into());
        root.bounds = UiBounds { x: 0.0, y: 0.0, width: 128.0, height: 128.0 };
        root.children = kinds.iter().enumerate().map(|(index, kind)| UiNode { node_id: UiNodeId(format!("control-{index}")), kind: kind.clone(), bounds: UiBounds { x: 4.0, y: 4.0 + index as f32 * 12.0, width: 56.0, height: 8.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() }).collect();
        root.children.push(UiNode { node_id: UiNodeId("disabled-slider".into()), kind: UiNodeKind::Slider, bounds: UiBounds { x: 72.0, y: 4.0, width: 48.0, height: 8.0 }, layout: None, visible: true, enabled: false, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() });
        root.children.push(UiNode { node_id: UiNodeId("progress".into()), kind: UiNodeKind::ProgressBar, bounds: UiBounds { x: 72.0, y: 20.0, width: 48.0, height: 8.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() });
        let pixels = render_hit_ids_for_test(&device, &queue, &HashMap::from([(UiFragmentId("component-gallery".into()), UiFragment { fragment_id: UiFragmentId("component-gallery".into()), revision: Revision(1), root, effects: Vec::new() })]), [256, 128]);
        for index in 0..kinds.len() { assert_ne!(pixels[(8 + index * 12) * 256 + 12], u32::MAX); }
        assert_eq!(pixels[8 * 256 + 84], u32::MAX, "disabled controls must not receive focusable hits");
        assert_eq!(pixels[24 * 256 + 84], u32::MAX, "progress is display-only and has no local hit target");
    }

    fn test_device(label: &'static str) -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
            apply_limit_buckets: false,
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            }))
        })
        .expect("a headless adapter is required");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .expect("a device is required")
    }

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
            surface: None,
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
    fn declared_modal_subtree_escapes_parent_clip() {
        let mut root = node();
        root.layout = Some(UiLayout { clip: UiClipPolicy::Bounds, ..UiLayout::default() });
        root.bounds = UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 20.0 };
        let mut modal = node();
        modal.node_id = UiNodeId("modal".into());
        modal.kind = UiNodeKind::Modal;
        modal.bounds = UiBounds { x: 30.0, y: 30.0, width: 40.0, height: 30.0 };
        root.children.push(modal);
        let fragment_id = neon_ui_schema::UiFragmentId("fixture".into());
        let flattened = flatten_fragments(&HashMap::from([(
            fragment_id.clone(),
            UiFragment { fragment_id, revision: Revision(1), root, effects: Vec::new() },
        )]), None);
        let modal = flattened.iter().find(|(id, _, _, _)| id == "fixture/modal").unwrap();
        assert!(modal.2.clip.width > 1_000_000.0);
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
            surface: None,
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
        let nodes = flatten_fragments(&fragments, None);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes[1].2.bounds,
            UiBounds {
                x: 18.0,
                y: 26.0,
                width: 40.0,
                height: 24.0
            }
        );
    }

    #[test]
    fn wgpu_layout_resolves_flex_grow_shrink_alignment_and_intrinsic_text() {
        let mut root = node();
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 40.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Row,
            padding: [4.0; 4],
            gap: 4.0,
            align_items: UiAlignItems::Center,
            justify_content: UiJustifyContent::Start,
            ..UiLayout::default()
        });
        for (id, grow, width, text) in [
            ("fixed", 0.0, 20.0, None),
            ("grow", 1.0, 0.0, None),
            ("text", 0.0, 0.0, Some("abc")),
        ] {
            root.children.push(UiNode {
                node_id: UiNodeId(id.into()),
                kind: UiNodeKind::Label,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height: 0.0,
                },
                layout: Some(UiLayout {
                    flex_grow: grow,
                    ..UiLayout::default()
                }),
                visible: true,
                enabled: true,
                text_key: None,
                text: text.map(|value| TextRef::Literal {
                    value: value.into(),
                }),
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            });
        }
        let fragments = HashMap::from([(
            UiFragmentId("flex".into()),
            UiFragment {
                fragment_id: UiFragmentId("flex".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let nodes = flatten_fragments(&fragments, None);
        assert_eq!(nodes[1].2.bounds.x, 4.0);
        assert!(
            nodes[2].2.bounds.width > 100.0,
            "grow consumes available main axis space"
        );
        assert!(
            nodes[3].2.bounds.width > 20.0,
            "auto text uses renderer intrinsic fallback before font residency"
        );
        assert_eq!(
            nodes[1].2.bounds.y, nodes[2].2.bounds.y,
            "center alignment uses common cross-axis placement"
        );
    }

    #[test]
    fn render_surface_samples_a_renderer_owned_gpu_texture() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-render-surface");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-render-surface-source"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-ui-render-surface-source-encoder"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-ui-render-surface-source-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit(Some(encoder.finish()));

        let root = UiNode {
            node_id: UiNodeId("preview".into()),
            kind: UiNodeKind::RenderSurface,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(RenderSurfaceRef {
                target_id: "ai.terrain.preview".into(),
            }),
            style: UiStyle {
                opacity: 1.0,
                ..UiStyle::default()
            },
            enter_transition: None,
            children: Vec::new(),
        };
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("ai-preview".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("ai-preview".into()), fragment)]),
            [64, 64],
            1.0,
            &[],
            vec![("ai.terrain.preview".into(), texture)],
        );
        let center = &pixels[4 * (32 * 64 + 32)..][..4];
        assert_eq!(center, [0, 255, 0, 255]);
    }

    #[test]
    fn render_surface_refreshes_when_the_same_target_is_replaced_repeatedly() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-render-surface-refresh");
        let root = UiNode {
            node_id: UiNodeId("preview".into()),
            kind: UiNodeKind::RenderSurface,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(RenderSurfaceRef {
                target_id: "ai.terrain.preview".into(),
            }),
            style: UiStyle {
                opacity: 1.0,
                ..UiStyle::default()
            },
            enter_transition: None,
            children: Vec::new(),
        };
        let fragments = HashMap::from([(
            UiFragmentId("ai-preview".into()),
            UiFragment {
                fragment_id: UiFragmentId("ai-preview".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        for (color, expected) in [
            (
                wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                [255, 0, 0, 255],
            ),
            (
                wgpu::Color {
                    r: 0.0,
                    g: 1.0,
                    b: 0.0,
                    a: 1.0,
                },
                [0, 255, 0, 255],
            ),
            (
                wgpu::Color {
                    r: 0.0,
                    g: 0.0,
                    b: 1.0,
                    a: 1.0,
                },
                [0, 0, 255, 255],
            ),
        ] {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("neon3-ui-render-surface-refresh-source"),
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-ui-render-surface-refresh-encoder"),
            });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("neon3-ui-render-surface-refresh-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(color),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit(Some(encoder.finish()));
            renderer.register_render_surface(&device, "ai.terrain.preview", texture);
            let pixels = render_renderer_offscreen_for_test(
                &mut renderer,
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                [64, 64],
                1.0,
            );
            assert_eq!(&pixels[4 * (32 * 64 + 32)..][..4], expected);
        }
    }

    #[test]
    fn bound_intents_compile_to_a_local_flexible_hit_map() {
        let mut root = node();
        root.kind = UiNodeKind::Button;
        root.enter_transition = None;
        let intent = UiIntent::Invoke {
            action: "ui.surface.event".into(),
            params: serde_json::json!({"schema_version": 1, "surface_id": "surface.test", "event": {"type": "DIAGNOSTICS_TOGGLE"}}),
        };
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("surface.test".into()),
            revision: Revision(4),
            root,
            effects: vec![UiEffect::BoundSemanticIntent {
                node_id: UiNodeId("root".into()),
                intent: intent.clone(),
            }],
        };
        let bindings = collect_hit_declarations(&HashMap::from([(
            UiFragmentId("surface.test".into()),
            fragment,
        )]));
        let binding = bindings
            .get("surface.test/root")
            .expect("bound node must resolve locally");
        assert_eq!(binding.fragment.revision, Revision(4));
        assert_eq!(binding.intent, Some(intent));
    }

    #[test]
    fn nested_gallery_controls_register_hit_bindings_and_dispatch_semantic_events() {
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        ))
        .expect("component gallery must parse");
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        fragment.effects.push(UiEffect::ControlPresentation {
            node_id: UiNodeId("feature-toggle".into()),
            state: UiControlPresentation::Toggle { selected: true },
        });
        let fragments = HashMap::from([(fragment.fragment_id.clone(), fragment.clone())]);
        let bindings = collect_hit_declarations(&fragments);
        assert!(matches!(
            flatten_fragments(&fragments, None)
                .iter()
                .find(|node| node.0 == "component-gallery/feature-toggle")
                .and_then(|node| node.2.presentation.as_ref()),
            Some(UiControlPresentation::Toggle { selected: true })
        ));
        let enabled_controls = [
            "feature-toggle", "mode-radio", "exposure-slider", "count-drag", "mode-combo",
            "mode-dropdown", "item-selectable", "item-list", "gallery-scroll",
        ];
        for key in enabled_controls {
            assert!(
                bindings.contains_key(&format!("component-gallery/{key}")),
                "{key} must have a renderer-local hit binding"
            );
        }

        let mut runtime = UiRuntime::new(7, "component-gallery-hit-test");
        let client = ClientIdentity {
            kind: ClientKind::WgpuRuntime,
            instance_id: "renderer-test".into(),
            pid: 1,
            origin: "test".into(),
        };
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("gallery-submit".into()),
            client: client.clone(),
            target: ServiceName("ui-runtime".into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment),
            }),
            expected_revision: None,
            idempotency_key: Some("gallery-submit".into()),
        };
        assert_eq!(runtime.handle_service_request(submit).status, RpcStatus::Accepted);
        for (sequence, key) in [
            "feature-toggle", "mode-radio", "exposure-slider", "mode-combo", "item-selectable",
        ]
        .into_iter()
        .enumerate()
        {
            let binding = bindings[&format!("component-gallery/{key}")].clone();
            let event = UiSemanticEvent {
                event: UiSemanticEventType::PointerClick,
                event_id: format!("gallery-{key}"),
                renderer_epoch: 7,
                composition_revision: Revision(1),
                fragment: binding.fragment,
                intent: binding.intent.expect("enabled control has declared intent"),
                pointer: Some(neon_ui_schema::UiPointerMetadata {
                    id: 0,
                    sequence: sequence as u64 + 1,
                }),
                focus: None,
                text: None,
                control_value: None,
                drag_drop: None,
            };
            let response = runtime.handle_service_request(RpcRequest {
                protocol: "neon3.rpc".into(),
                version: ProtocolVersion { major: 1, minor: 0 },
                request_id: RequestId(format!("gallery-request-{key}")),
                client: client.clone(),
                target: ServiceName("ui-runtime".into()),
                method: "ui.input.event".into(),
                params: json!(event),
                expected_revision: Some(Revision(1)),
                idempotency_key: Some(format!("gallery-key-{key}")),
            });
            assert_eq!(response.status, RpcStatus::Accepted, "{key}: {:?}", response.error);
        }
    }

    #[test]
    fn owner_font_and_text_ref_produce_glyph_pixels_without_background_fill() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-text-acceptance");
        let font = fixture_font();
        let text = UiNode {
            node_id: UiNodeId("text".into()),
            kind: UiNodeKind::Label,
            bounds: UiBounds {
                x: 4.0,
                y: 4.0,
                width: 56.0,
                height: 24.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: Some(TextRef::Literal { value: "A".into() }),
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.0; 4],
                border_color: [0.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: Vec::new(),
        };
        let fragment = UiFragment {
            fragment_id: UiFragmentId("text".into()),
            revision: Revision(1),
            root: text,
            effects: Vec::new(),
        };
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("text".into()), fragment)]),
            [64, 32],
            1.0,
            &[font],
            Vec::new(),
        );
        assert!(
            pixels.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "text must produce glyph alpha without a panel background"
        );
    }

    #[test]
    fn bundled_font_renders_cjk_text_without_an_owner_font_asset() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-bundled-cjk-text");
        let text = UiNode {
            node_id: UiNodeId("text".into()),
            kind: UiNodeKind::Label,
            bounds: UiBounds {
                x: 4.0,
                y: 4.0,
                width: 84.0,
                height: 24.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: Some(TextRef::Literal {
                value: "地形 UI".into(),
            }),
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.0; 4],
                border_color: [0.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: Vec::new(),
        };
        let fragment = UiFragment {
            fragment_id: UiFragmentId("bundled-cjk-text".into()),
            revision: Revision(1),
            root: text,
            effects: Vec::new(),
        };
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("bundled-cjk-text".into()), fragment)]),
            [96, 32],
            1.0,
            &[],
            Vec::new(),
        );
        assert!(
            pixels.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "bundled CJK font must produce glyph alpha"
        );
    }

    #[test]
    fn text_wraps_within_label_width_and_respects_parent_clip() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-text-wrap-clip");
        let font = fixture_font();
        let label = UiNode {
            node_id: UiNodeId("wrapped-text".into()),
            kind: UiNodeKind::Label,
            bounds: UiBounds {
                x: 4.0,
                y: 0.0,
                width: 24.0,
                height: 96.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: Some(TextRef::Literal {
                value: "AAA".into(),
            }),
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.0; 4],
                border_color: [0.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: Vec::new(),
        };
        let root = UiNode {
            node_id: UiNodeId("clip-root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 64.0,
            },
            layout: Some(neon_ui_schema::UiLayout {
                clip: neon_ui_schema::UiClipPolicy::Bounds,
                ..neon_ui_schema::UiLayout::default()
            }),
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.0; 4],
                border_color: [0.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: vec![label],
        };
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(
                UiFragmentId("wrap-clip".into()),
                UiFragment {
                    fragment_id: UiFragmentId("wrap-clip".into()),
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [64, 96],
            1.0,
            &[font],
            Vec::new(),
        );
        let has_alpha_in_rows = |from: usize, until: usize| {
            pixels
                .chunks_exact(4)
                .enumerate()
                .any(|(index, pixel)| index / 64 >= from && index / 64 < until && pixel[3] > 0)
        };
        assert!(
            has_alpha_in_rows(8, 36),
            "first wrapped line must produce glyph coverage"
        );
        assert!(
            has_alpha_in_rows(18, 64),
            "second wrapped line must produce glyph coverage"
        );
        assert!(
            !has_alpha_in_rows(64, 96),
            "parent clip must discard glyph coverage outside its bounds"
        );
    }

    #[test]
    fn flatten_uses_declared_column_layout_and_scroll_offset() {
        let mut root = node();
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        root.layout = Some(neon_ui_schema::UiLayout {
            mode: UiLayoutMode::Column,
            padding: [4.0; 4],
            gap: 2.0,
            scroll_offset: [0.0, 3.0],
            ..neon_ui_schema::UiLayout::default()
        });
        root.children = vec![
            UiNode {
                node_id: UiNodeId("first".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            },
            UiNode {
                node_id: UiNodeId("second".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            },
        ];
        let fragments = HashMap::from([(
            UiFragmentId("layout".into()),
            UiFragment {
                fragment_id: UiFragmentId("layout".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let nodes = flatten_fragments(&fragments, None);
        assert_eq!(nodes[1].2.bounds.y, 1.0);
        assert_eq!(nodes[2].2.bounds.y, 13.0);
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
            clip: UiBounds {
                x: -1_000_000.0,
                y: -1_000_000.0,
                width: 2_000_000.0,
                height: 2_000_000.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
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
            clip: UiBounds {
                x: -1_000_000.0,
                y: -1_000_000.0,
                width: 2_000_000.0,
                height: 2_000_000.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
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
            clip: UiBounds {
                x: -1_000_000.0,
                y: -1_000_000.0,
                width: 2_000_000.0,
                height: 2_000_000.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
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
    fn animation_activity_expires_after_transition_end() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-animation-activity");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let target = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
        };
        UiWgpuRenderer::sample(
            &mut renderer.current,
            &mut renderer.active,
            "animated",
            &target,
            Some(&UiTransition {
                delay_ms: 0,
                duration_ms: 10,
                easing: UiEasing::Linear,
                from: UiTransitionState {
                    opacity: Some(0.0),
                    ..UiTransitionState::default()
                },
            }),
            1.0,
        );
        assert!(renderer.has_active_animation(1.005));
        assert!(!renderer.has_active_animation(1.020));
    }

    #[test]
    fn srgb_conversion_matches_the_surface_encoding_contract() {
        let convert = |value: f32| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        assert!((convert(0.25) - 0.050876).abs() < 0.00001);
        assert!((convert(0.5) - 0.214041).abs() < 0.00001);
        assert!((convert(0.75) - 0.522522).abs() < 0.00001);
    }

    #[test]
    fn hit_readback_ring_copies_one_r32uint_texel_asynchronously() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-hit-readback-test");
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-hit-readback-source"),
            size: wgpu::Extent3d {
                width: 64,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Uint,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut pixels = vec![0_u8; HIT_READBACK_BYTES_PER_ROW as usize];
        pixels[4..8].copy_from_slice(&37_u32.to_ne_bytes());
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(HIT_READBACK_BYTES_PER_ROW),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 64,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let mut ring = HitReadbackRing::new(&device, 2);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-hit-readback-encoder"),
        });
        let slot = ring
            .enqueue(&mut encoder, &target, [1, 0])
            .expect("a ring slot must be available");
        queue.submit(Some(encoder.finish()));
        assert!(ring.begin_mapping(slot));
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        assert_eq!(ring.try_complete(slot).unwrap().unwrap(), 37);
    }
}
