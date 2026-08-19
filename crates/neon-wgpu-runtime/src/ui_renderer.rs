//! Minimal GPU UI composition pass adapted from Neon2's instanced panel renderer.
//! It deliberately consumes only Neon3's public UI schema, not old ECS state.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use bytemuck::{Pod, Zeroable};
use neon_protocol::{AssetBytes, AssetRef};
use neon_ui_schema::{
    RenderSurfaceRef, TextRef, UiAlignItems, UiBounds, UiClipPolicy, UiControlPresentation,
    UiDataGridCellTarget, UiDataGridWindowRequest, UiDragAxis, UiDragBinding, UiDragBoundary,
    UiDropPlacement, UiEasing, UiFragment, UiFragmentRevision, UiIntent, UiJustifyContent,
    UiLayout, UiLayoutMode, UiNode, UiNodeKind, UiSemanticPayloadValue, UiStyle, UiTransition,
};
use serde_json::{Value, json};

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
    if (input.params.y < 0.0) {
        let cut = min(-input.params.y, input.size.x * 0.25);
        let point = input.local * input.size;
        let left = cut * (1.0 - input.local.y);
        let right = input.size.x - cut * input.local.y;
        let edge_distance = min(
            min(point.x - left, right - point.x),
            min(point.y, input.size.y - point.y)
        );
        let shape_alpha = smoothstep(-1.0, 1.0, edge_distance);
        let border_alpha = 1.0 - smoothstep(
            input.params.x - 1.0,
            input.params.x + 1.0,
            edge_distance
        );
        let color = mix(input.fill, input.border, border_alpha);
        return vec4<f32>(srgb_to_linear(color.rgb), color.a * input.params.z * shape_alpha);
    }
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
  if (input.params.y < 0.0) {
   let cut=min(-input.params.y,input.size.x*0.25); let point=input.local*input.size;
   let left=cut*(1.0-input.local.y); let right=input.size.x-cut*input.local.y;
   let edge_distance=min(min(point.x-left,right-point.x),min(point.y,input.size.y-point.y));
   if (edge_distance < 0.0 || input.params.z <= 0.0) { discard; } return input.hit_id;
  }
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

const DEPTH_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
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
    @location(5) depth: f32,
}
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) clip: vec4<f32>,
    @location(1) params: vec4<f32>,
    @location(2) pixel: vec2<f32>,
    @location(3) depth: f32,
}
@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let local = corners[vertex_index];
    let pixel = input.rect.xy + local * input.rect.zw;
    var output: VsOut;
    output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0);
    output.clip = input.clip;
    output.params = input.params;
    output.pixel = pixel;
    output.depth = input.depth;
    return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) f32 {
    if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
    if (input.depth <= 0.0) { discard; }
    return input.depth;
}
"#;

const IMAGE_SHADER: &str = r#"
struct View { viewport: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
 let low = value / 12.92; let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)); return select(low, high, value > vec3<f32>(0.04045));
}
struct VsIn { @location(0) rect: vec4<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) uv: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32>, @location(4) uv: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
  output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.tint = input.tint; output.clip = input.clip; output.pixel = pixel; output.uv = input.uv.xy + local * input.uv.zw; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
   let sample = textureSample(image_texture, image_sampler, input.uv);
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
    /// Normalized occlusion depth (0.0 = near/always-on-top, 1.0 = far). Written
    /// to the optional depth target so the consumer can depth-test the overlay.
    depth: f32,
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
    pub data_grid_cell: Option<UiDataGridCellTarget>,
    pub control_value: Option<UiSemanticPayloadValue>,
    pub max_text_length: Option<u32>,
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PendingLocalPresentationKey {
    pub semantic_sequence: u64,
    pub fragment_id: String,
    pub fragment_revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LocalPresentationCommit {
    Value {
        node_path: String,
        value: UiSemanticPayloadValue,
    },
    Drag {
        source_path: String,
        offset: [f32; 2],
    },
}

impl LocalPresentationCommit {
    fn node_path(&self) -> &str {
        match self {
            Self::Value { node_path, .. } => node_path,
            Self::Drag { source_path, .. } => source_path,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingLocalPresentationCommit {
    presentation: LocalPresentationCommit,
    delivery_accepted: bool,
    presentation_applied: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct UiResolvedDragDrop {
    pub fragment: UiFragmentRevision,
    pub intent: UiIntent,
    pub source_key: String,
    pub target_key: String,
    pub placement: UiDropPlacement,
    pub presentation_template_key: Option<String>,
    pub local_presentation: LocalPresentationCommit,
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
    uv: [f32; 4],
    depth: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiTextInstance {
    rect: [f32; 4],
    color: [f32; 4],
    clip: [f32; 4],
    uv: [f32; 4],
    /// Paint-group depth inherited from the owning panel. The color shader
    /// ignores this field; the CPU uses it to keep panel and text together.
    depth: f32,
}

struct ResidentImage {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    uv: [f32; 4],
}

struct ResidentImageAtlas {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
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
const IMAGE_ATLAS_WIDTH: u32 = 2048;
const IMAGE_ATLAS_PADDING: u32 = 1;
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
    scroll: bool,
    declared_scroll_offset: [f32; 2],
    /// Normalized occlusion depth inherited from a projected world panel
    /// (`None` for screen UI → rendered as "always on top", depth 0.0).
    world_depth: Option<f32>,
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

/// Private bridge from a declaration's fragment-local node key to its current
/// rendered plan node. Renderer paths stay internal to the debug API.
struct DebugSemanticNode {
    fragment_id: String,
    node_key: String,
    plan_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollMetrics {
    viewport: UiBounds,
    content_size: [f32; 2],
    max_offset: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollAxis {
    X,
    Y,
}

#[derive(Clone, Debug)]
struct ScrollDrag {
    node_path: String,
    axis: ScrollAxis,
    pointer_start: f32,
    offset_start: f32,
}

#[derive(Clone, Debug)]
struct ScrollPan {
    node_path: String,
    pointer_start: [f32; 2],
    offset_start: [f32; 2],
}

#[derive(Clone, Debug)]
struct DataGridScrollHold {
    body_offset: [f32; 2],
    desired_offset: [f32; 2],
    fallback_frame: neon_ui_schema::UiDataGridFrame,
    release_fragment_revision: Option<neon_protocol::Revision>,
    pending_sequence: Option<u64>,
}

/// Renderer-local identity for a virtual cell. It deliberately excludes
/// renderer topology, GPU hit IDs, and the row's current frame position.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DataGridCellIdentity {
    source_key: String,
    stable_row_key: String,
    column_key: String,
}

#[derive(Clone, Debug)]
struct CachedDataGridTextDisplay {
    text: String,
}

pub struct UiWgpuRenderer {
    color_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    depth_format: Option<wgpu::TextureFormat>,
    depth_pipeline: Option<wgpu::RenderPipeline>,
    view_buffer: wgpu::Buffer,
    view_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    popup_instance_buffer: wgpu::Buffer,
    popup_instance_capacity: usize,
    plan_revisions: HashMap<neon_ui_schema::UiFragmentId, neon_protocol::Revision>,
    plan: Vec<PlannedNode>,
    debug_semantic_nodes: Vec<DebugSemanticNode>,
    sampled: Vec<UiVisual>,
    instances: Vec<UiInstance>,
    viewport_physical_size: [u32; 2],
    viewport_logical_size: [f32; 2],
    viewport_revision: u64,
    plan_viewport_revision: u64,
    view_buffer_viewport_revision: u64,
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
    image_atlas: Option<ResidentImageAtlas>,
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
    pending_local_presentations:
        HashMap<PendingLocalPresentationKey, PendingLocalPresentationCommit>,
    open_dropdown: Option<String>,
    scroll_offsets: HashMap<String, [f32; 2]>,
    scroll_metrics: HashMap<String, ScrollMetrics>,
    scroll_drag: Option<ScrollDrag>,
    scroll_pan: Option<ScrollPan>,
    data_grid_frames: HashMap<String, neon_ui_schema::UiDataGridFrame>,
    data_grid_scroll_holds: HashMap<String, DataGridScrollHold>,
    data_grid_text_display_cache: HashMap<DataGridCellIdentity, CachedDataGridTextDisplay>,
    available_cameras: HashSet<(neon_world_bridge::CameraId, neon_world_bridge::CameraKind)>,
}

impl UiWgpuRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_internal(device, format, None)
    }

    /// Renderer that also emits a per-pixel occlusion depth target (R32Float).
    /// `draw` writes color as usual; `draw_depth` re-emits the same instances
    /// with their normalized depth into a separate depth pass.
    pub(crate) fn new_with_depth(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        Self::new_internal(device, format, Some(depth_format))
    }

    fn new_internal(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
    ) -> Self {
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
        let depth_pipeline = depth_format.map(|depth_format| {
            let depth_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("neon3-ui-depth-shader"),
                source: wgpu::ShaderSource::Wgsl(DEPTH_SHADER.into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("neon3-ui-depth-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &depth_shader,
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
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 80,
                                shader_location: 5,
                            },
                        ],
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &depth_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: depth_format,
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
        });
        Self {
            color_format: format,
            pipeline,
            depth_format,
            depth_pipeline,
            view_buffer,
            view_bind_group,
            instance_buffer: create_instance_buffer(device, 1),
            instance_capacity: 1,
            popup_instance_buffer: create_instance_buffer(device, 1),
            popup_instance_capacity: 1,
            plan_revisions: HashMap::new(),
            plan: Vec::new(),
            debug_semantic_nodes: Vec::new(),
            sampled: Vec::new(),
            instances: Vec::new(),
            viewport_physical_size: [0, 0],
            viewport_logical_size: [0.0, 0.0],
            viewport_revision: 0,
            plan_viewport_revision: 0,
            view_buffer_viewport_revision: 0,
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
            image_atlas: None,
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
            pending_local_presentations: HashMap::new(),
            open_dropdown: None,
            scroll_offsets: HashMap::new(),
            scroll_metrics: HashMap::new(),
            scroll_drag: None,
            scroll_pan: None,
            data_grid_frames: HashMap::new(),
            data_grid_scroll_holds: HashMap::new(),
            data_grid_text_display_cache: HashMap::new(),
            available_cameras: HashSet::new(),
        }
    }

    pub(crate) fn draw_hit_id<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport_physical_size: [u32; 2],
        viewport_logical_size: [f32; 2],
        time_seconds: f32,
    ) {
        pass.set_pipeline(&self.hit_clear_pipeline);
        pass.draw(0..3, 0..1);
        self.update_viewport(viewport_physical_size, viewport_logical_size);
        self.refresh_plan(fragments, viewport_logical_size);
        self.compose_sampled_visuals(time_seconds);
        let hit_nodes = self.refresh_hit_bindings(fragments);
        let mut instances = Vec::new();
        for (hit_id, index) in hit_nodes {
            let visual = self.visual_at(index);
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
                viewport: self.viewport_logical_size,
                _pad: [0.0; 2],
            }),
        );
        self.view_buffer_viewport_revision = self.viewport_revision;
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

    /// Makes CPU-side pointer handling independent of a prior redraw or GPU hit readback.
    /// Rendering still performs the full composed visual pass; this prepares the current
    /// declaration sample and its renderer-local semantic bindings for an incoming press.
    pub(crate) fn prepare_interaction(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport_physical_size: [u32; 2],
        viewport_logical_size: [f32; 2],
        time_seconds: f32,
    ) {
        self.update_viewport(viewport_physical_size, viewport_logical_size);
        self.refresh_plan(fragments, viewport_logical_size);
        self.compose_sampled_visuals(time_seconds);
        self.refresh_hit_bindings(fragments);
    }

    fn compose_sampled_visuals(&mut self, time_seconds: f32) -> Vec<Option<usize>> {
        self.update_scroll_metrics();
        for index in 0..self.plan.len() {
            let node = &self.plan[index];
            // Always begin with a canonical transition sample. Inherited composition
            // below must never accumulate in `sampled` across renderer entry points.
            self.sampled[index] = Self::sample(
                &mut self.current,
                &mut self.active,
                &node.id,
                &node.target,
                node.transition.as_ref(),
                time_seconds,
            );
        }

        let plan_index = self
            .plan
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect::<HashMap<_, _>>();
        let top_layer = top_layer_roots(&self.plan, &plan_index);
        let mut subtree_translation = vec![[0.0_f32; 2]; self.plan.len()];
        let mut subtree_scroll = vec![[0.0_f32; 2]; self.plan.len()];
        let mut subtree_scroll_clip = vec![None; self.plan.len()];
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
            let inherited_scroll = parent.map_or([0.0; 2], |parent| subtree_scroll[parent]);
            let inherited_scroll_clip = parent.and_then(|parent| subtree_scroll_clip[parent]);
            let own_translation = [
                self.sampled[index].bounds.x - target.bounds.x,
                self.sampled[index].bounds.y - target.bounds.y,
            ];
            let own_opacity = if target.style.opacity > 0.0 {
                self.sampled[index].style.opacity / target.style.opacity
            } else {
                1.0
            };
            let sticky_vertical = self.plan[index]
                .id
                .rsplit('/')
                .next()
                .is_some_and(|segment| {
                    segment == "data-grid-header" || segment.starts_with("data-grid-header-")
                });
            let fixed_data_grid_body_clip = self.plan[index]
                .id
                .split('/')
                .any(|segment| segment.starts_with("data-grid-row-"));
            let data_grid_scroll = if sticky_vertical || fixed_data_grid_body_clip {
                let mut ancestor = parent;
                let mut scroll = None;
                while let Some(ancestor_index) = ancestor {
                    let ancestor_node = &self.plan[ancestor_index];
                    if ancestor_node.target.kind == UiNodeKind::DataGrid {
                        let desired = self
                            .scroll_offsets
                            .get(&ancestor_node.id)
                            .copied()
                            .unwrap_or(ancestor_node.target.declared_scroll_offset);
                        let body = self
                            .data_grid_scroll_holds
                            .get(&ancestor_node.id)
                            .map_or(desired, |hold| hold.body_offset);
                        scroll = Some((desired, body));
                        break;
                    }
                    ancestor = ancestor_node
                        .parent_id
                        .as_deref()
                        .and_then(|parent_id| plan_index.get(parent_id).copied());
                }
                scroll.unwrap_or(([0.0; 2], [0.0; 2]))
            } else {
                ([0.0; 2], [0.0; 2])
            };
            let applied_scroll = if sticky_vertical {
                [
                    inherited_scroll[0] - data_grid_scroll.0[0] + data_grid_scroll.1[0],
                    inherited_scroll[1] - data_grid_scroll.0[1],
                ]
            } else if fixed_data_grid_body_clip {
                [
                    inherited_scroll[0] - data_grid_scroll.0[0] + data_grid_scroll.1[0],
                    inherited_scroll[1] - data_grid_scroll.0[1] + data_grid_scroll.1[1],
                ]
            } else {
                inherited_scroll
            };
            self.sampled[index].bounds.x += inherited_translation[0];
            self.sampled[index].bounds.y += inherited_translation[1];
            self.sampled[index].bounds.x -= applied_scroll[0];
            self.sampled[index].bounds.y -= applied_scroll[1];
            self.sampled[index].clip.x += inherited_translation[0];
            self.sampled[index].clip.y += inherited_translation[1];
            if self.sampled[index].clip == target.bounds {
                self.sampled[index].clip.x += own_translation[0];
                self.sampled[index].clip.y += own_translation[1];
            }
            if fixed_data_grid_body_clip {
                self.sampled[index].clip.x -= inherited_scroll[0] - data_grid_scroll.0[0];
                self.sampled[index].clip.y -= inherited_scroll[1] - data_grid_scroll.0[1];
            } else {
                self.sampled[index].clip.x -= applied_scroll[0];
                self.sampled[index].clip.y -= applied_scroll[1];
            }
            if let Some(scroll_clip) = inherited_scroll_clip {
                self.sampled[index].clip =
                    intersect_clip(Some(scroll_clip), self.sampled[index].clip);
            }
            self.sampled[index].style.opacity *= inherited_opacity;
            if top_layer[index].is_some() {
                self.sampled[index].clip = UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: self.viewport_logical_size[0],
                    height: self.viewport_logical_size[1],
                };
                self.sampled[index].clip_radius = 0.0;
            }
            if let Some(offset) = self.drag_offset_for_node(index, &plan_index) {
                self.sampled[index].bounds.x += offset[0];
                self.sampled[index].bounds.y += offset[1];
                self.sampled[index].clip = UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: self.viewport_logical_size[0],
                    height: self.viewport_logical_size[1],
                };
                self.sampled[index].clip_radius = 0.0;
            }
            subtree_translation[index] = [
                inherited_translation[0] + own_translation[0],
                inherited_translation[1] + own_translation[1],
            ];
            let own_scroll = if target.scroll {
                self.scroll_offsets
                    .get(&self.plan[index].id)
                    .copied()
                    .unwrap_or(target.declared_scroll_offset)
            } else {
                [0.0; 2]
            };
            subtree_scroll[index] = [
                inherited_scroll[0] + own_scroll[0],
                inherited_scroll[1] + own_scroll[1],
            ];
            subtree_scroll_clip[index] = if target.scroll {
                Some(intersect_clip(
                    inherited_scroll_clip,
                    self.sampled[index].clip,
                ))
            } else {
                inherited_scroll_clip
            };
            subtree_opacity[index] = inherited_opacity * own_opacity;
        }
        top_layer
    }

    /// Semantic declaration paths only. Numeric renderer hit IDs remain private
    /// to this process and are never included in a diagnostic response.
    pub(crate) fn semantic_hit_nodes(&self) -> Vec<String> {
        let mut paths = self
            .hit_bindings
            .values()
            .filter(|binding| binding.intent.is_some() && binding.data_grid_cell.is_none())
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
        self.plan
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, node)| {
                let visual = self.visual_at(index);
                if !visual.enabled
                    || !contains(visual.bounds, pointer)
                    || !contains(visual.clip, pointer)
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

    /// Renderer-local RenderSurface hit testing. The stable target is retained
    /// for diagnostics; no node path or GPU hit ID crosses the process boundary.
    pub(crate) fn render_surface_contains(&self, target_id: &str, pointer: [f32; 2]) -> bool {
        self.plan.iter().enumerate().rev().any(|(index, _node)| {
            let visual = self.visual_at(index);
            visual.kind == UiNodeKind::RenderSurface
                && visual
                    .surface
                    .as_ref()
                    .is_some_and(|surface| surface.target_id == target_id)
                && contains(visual.bounds, pointer)
                && contains(visual.clip, pointer)
        })
    }

    /// Debug-only semantic diagnostics for a prepared pointer sample. Renderer
    /// hit IDs remain process-local, including on this diagnostic path.
    pub(crate) fn pointer_probe_snapshot(&self) -> Value {
        let fallback = self
            .hit_id_at_pointer()
            .and_then(|hit_id| self.hit_binding(hit_id));
        let modal = self.active_modal_index().map(|index| {
            let visual = self.visual_at(index);
            json!({
                "active": true,
                "semantic_node_path": self.plan[index].id,
                "blocks_pointer": self.modal_blocks_pointer(),
                "bounds": {"x": visual.bounds.x, "y": visual.bounds.y, "width": visual.bounds.width, "height": visual.bounds.height},
            })
        }).unwrap_or_else(|| json!({"active": false, "blocks_pointer": false}));
        let scroll_container = self.pointer_position.and_then(|pointer| {
            self.plan.iter().rev().find_map(|node| {
                let metrics = self.scroll_metrics.get(&node.id)?;
                (node.target.scroll && contains(metrics.viewport, pointer)).then(|| {
                    let offset = self
                        .scroll_offsets
                        .get(&node.id)
                        .copied()
                        .unwrap_or(node.target.declared_scroll_offset);
                    json!({
                        "semantic_node_path": node.id,
                        "offset": {"x": offset[0], "y": offset[1]},
                        "max_offset": {"x": metrics.max_offset[0], "y": metrics.max_offset[1]},
                    })
                })
            })
        });
        json!({
            "fallback_hit": match fallback {
                Some(binding) => json!({"status": "hit", "semantic_node_path": binding.node_path}),
                None => json!({"status": "miss", "semantic_node_path": Value::Null}),
            },
            "modal": modal,
            "scroll_container": scroll_container,
        })
    }

    /// Test-only semantic scroll control. Production input continues to update
    /// scrollports exclusively through pointer pan, drag, and wheel events.
    pub(crate) fn debug_scroll_to_max(&mut self, node_path: &str) -> Result<Value, &'static str> {
        let node = self
            .plan
            .iter()
            .find(|node| node.id == node_path)
            .ok_or("unknown_scroll_container")?;
        if !node.target.scroll {
            return Err("semantic_target_is_not_scrollable");
        }
        let metrics = self
            .scroll_metrics
            .get(node_path)
            .copied()
            .ok_or("scroll_metrics_unavailable")?;
        self.scroll_offsets
            .insert(node_path.to_owned(), metrics.max_offset);
        self.pointer_visual_dirty = true;
        Ok(json!({
            "semantic_node_path": node_path,
            "offset": {"x": metrics.max_offset[0], "y": metrics.max_offset[1]},
            "max_offset": {"x": metrics.max_offset[0], "y": metrics.max_offset[1]},
        }))
    }

    /// Resolves a declared semantic target to its current visual center solely
    /// for the window-input scenario's debug activation path.
    pub(crate) fn debug_semantic_target_binding(
        &self,
        node_path: &str,
    ) -> Result<UiHitBinding, &'static str> {
        let index = self
            .plan
            .iter()
            .position(|node| node.id == node_path)
            .ok_or("unknown_semantic_target")?;
        let visual = self.visual_at(index);
        if !visual.enabled || visual.bounds.width <= 0.0 || visual.bounds.height <= 0.0 {
            return Err("semantic_target_not_visible");
        }
        let visible_left = visual.bounds.x.max(visual.clip.x);
        let visible_top = visual.bounds.y.max(visual.clip.y);
        let visible_right =
            (visual.bounds.x + visual.bounds.width).min(visual.clip.x + visual.clip.width);
        let visible_bottom =
            (visual.bounds.y + visual.bounds.height).min(visual.clip.y + visual.clip.height);
        if visible_right <= visible_left || visible_bottom <= visible_top {
            return Err("semantic_target_clipped");
        }
        self.hit_bindings
            .values()
            .find(|binding| binding.node_path == node_path)
            .cloned()
            .ok_or("semantic_target_not_hittable")
    }

    /// Returns visible centers for declared semantic node keys. This is debug
    /// automation only; normal pointer input still owns the gesture.
    pub(crate) fn debug_drag_gesture_points(
        &self,
        source_node_key: &str,
        target_node_key: &str,
    ) -> Result<([f32; 2], [f32; 2]), &'static str> {
        let resolve = |node_key: &str| {
            let matches = self
                .debug_semantic_nodes
                .iter()
                .filter(|node| node.node_key == node_key)
                .collect::<Vec<_>>();
            let [node] = matches.as_slice() else {
                return Err(if matches.is_empty() {
                    "unknown_semantic_node_key"
                } else {
                    "ambiguous_semantic_node_key"
                });
            };
            let expected_plan_path = format!("{}/{}", node.fragment_id, node.node_key);
            self.plan
                .iter()
                .position(|planned| {
                    planned.id == node.plan_path && planned.id == expected_plan_path
                })
                .ok_or("semantic_node_not_in_current_plan")
        };
        let point = |node_key: &str| {
            let index = resolve(node_key)?;
            let visual = self.visual_at(index);
            let left = visual.bounds.x.max(visual.clip.x);
            let top = visual.bounds.y.max(visual.clip.y);
            let right =
                (visual.bounds.x + visual.bounds.width).min(visual.clip.x + visual.clip.width);
            let bottom =
                (visual.bounds.y + visual.bounds.height).min(visual.clip.y + visual.clip.height);
            if right <= left || bottom <= top {
                return Err("semantic_target_clipped");
            }
            Ok([(left + right) * 0.5, (top + bottom) * 0.5])
        };
        let source_index = resolve(source_node_key)?;
        let source_bounds = self.visual_at(source_index).bounds;
        if source_bounds.width <= 0.0 || source_bounds.height <= 0.0 {
            return Err("drag_source_not_visible");
        }
        let source = [
            source_bounds.x + source_bounds.width * 0.5,
            source_bounds.y + source_bounds.height * 0.5,
        ];
        let target = point(target_node_key).map_err(|error| {
            if error == "semantic_target_clipped" {
                "drop_target_clipped"
            } else {
                error
            }
        })?;
        Ok((source, target))
    }

    /// Resolves deterministic pointer points for the debug window gesture path.
    /// The points still enter the normal hit, capture, preview, and release code.
    pub(crate) fn debug_value_gesture_points(
        &self,
        node_path: &str,
        target_fraction: f32,
    ) -> Result<([f32; 2], [f32; 2]), &'static str> {
        if !target_fraction.is_finite() || !(0.0..=1.0).contains(&target_fraction) {
            return Err("invalid_gesture_fraction");
        }
        let index = self
            .plan
            .iter()
            .position(|node| node.id == node_path)
            .ok_or("unknown_semantic_target")?;
        let visual = self.visual_at(index);
        let (bounds, current_fraction) = match (&visual.kind, &visual.presentation) {
            (UiNodeKind::Slider, Some(UiControlPresentation::Numeric { value, min, max })) => (
                UiBounds {
                    x: visual.bounds.x + visual.bounds.width * 0.57,
                    y: visual.bounds.y,
                    width: visual.bounds.width * 0.34,
                    height: visual.bounds.height,
                },
                numeric_fraction(*value, *min, *max),
            ),
            (UiNodeKind::DragValue, Some(UiControlPresentation::Numeric { value, min, max })) => (
                drag_value_bounds(visual.bounds),
                numeric_fraction(*value, *min, *max),
            ),
            (UiNodeKind::Scrollbar, Some(UiControlPresentation::Scroll { position })) => (
                UiBounds {
                    x: visual.bounds.x + 10.0,
                    y: visual.bounds.y,
                    width: (visual.bounds.width - 20.0).max(1.0),
                    height: visual.bounds.height,
                },
                position.clamp(0.0, 1.0),
            ),
            _ => return Err("semantic_target_is_not_value_control"),
        };
        let point = |fraction: f32| {
            [
                bounds.x + bounds.width * fraction,
                bounds.y + bounds.height * 0.5,
            ]
        };
        Ok((point(current_fraction), point(target_fraction)))
    }

    pub(crate) fn scroll_wheel_at_pointer(&mut self, delta: [f32; 2]) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(node) = self.plan.iter().enumerate().rev().find(|(_, node)| {
            node.target.scroll
                && contains(node.target.bounds, pointer)
                && self.scroll_metrics.get(&node.id).is_some_and(|metrics| {
                    metrics.max_offset[0] > 0.0 || metrics.max_offset[1] > 0.0
                })
        }) else {
            return false;
        };
        let metrics = self.scroll_metrics[&node.1.id];
        let offset = self
            .scroll_offsets
            .entry(node.1.id.clone())
            .or_insert(node.1.target.declared_scroll_offset);
        let next = [
            (offset[0] - delta[0]).clamp(0.0, metrics.max_offset[0]),
            (offset[1] - delta[1]).clamp(0.0, metrics.max_offset[1]),
        ];
        if *offset == next {
            return false;
        }
        *offset = next;
        self.pointer_visual_dirty = true;
        true
    }

    /// Calculates replacement windows from renderer-local scroll state. A request
    /// is emitted only when the current bounded frame no longer covers the desired
    /// viewport plus overscan.
    pub(crate) fn data_grid_window_requests(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        renderer_epoch: u64,
        composition_revision: neon_protocol::Revision,
        sequence: &mut u64,
        only_grid_path: Option<&str>,
        force_request: bool,
    ) -> Vec<UiDataGridWindowRequest> {
        if only_grid_path.is_none() && self.data_grid_scroll_drag_active() {
            return Vec::new();
        }
        let mut requests = Vec::new();
        let mut settled = Vec::new();
        for fragment in fragments.values() {
            for effect in &fragment.effects {
                let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                    continue;
                };
                let grid_path = format!("{}/{}", fragment.fragment_id.0, declaration.node_key);
                if only_grid_path.is_some_and(|path| path != grid_path) {
                    continue;
                }
                if force_request
                    && self
                        .data_grid_scroll_holds
                        .get(&grid_path)
                        .is_some_and(|hold| hold.pending_sequence.is_some())
                {
                    continue;
                }
                let release_fragment_revision = fragment.revision;
                let Some(grid) = self.plan.iter().find(|node| node.id == grid_path) else {
                    continue;
                };
                let offset_y = self
                    .scroll_offsets
                    .get(&grid_path)
                    .copied()
                    .unwrap_or(grid.target.declared_scroll_offset)[1]
                    .max(0.0);
                let Some((requested_first_row, required_rows)) = data_grid_requested_range(
                    frame,
                    declaration,
                    offset_y,
                    grid.target.bounds.height,
                ) else {
                    if self.data_grid_scroll_holds.contains_key(&grid_path) {
                        settled.push(grid_path);
                    }
                    continue;
                };
                let requested_end = requested_first_row
                    .saturating_add(required_rows)
                    .min(frame.total_rows);
                let frame_end = frame
                    .first_row
                    .saturating_add(frame.window_rows.len() as u64)
                    .min(frame.total_rows);
                if !force_request
                    && requested_first_row >= frame.first_row
                    && requested_end <= frame_end
                {
                    if self.data_grid_scroll_holds.contains_key(&grid_path) {
                        settled.push(grid_path);
                    }
                    continue;
                }
                *sequence += 1;
                let request = UiDataGridWindowRequest {
                    renderer_epoch,
                    composition_revision,
                    fragment: UiFragmentRevision {
                        id: fragment.fragment_id.clone(),
                        revision: fragment.revision,
                    },
                    source_key: declaration.source_key.clone(),
                    expected_list_revision: frame.list_revision,
                    requested_first_row,
                    max_window_rows: declaration.max_window_rows,
                    sequence: *sequence,
                };
                if let Some(hold) = self.data_grid_scroll_holds.get_mut(&grid_path) {
                    hold.release_fragment_revision = Some(release_fragment_revision);
                    hold.pending_sequence = Some(request.sequence);
                }
                requests.push(request);
            }
        }
        for grid_path in settled {
            self.data_grid_scroll_holds.remove(&grid_path);
        }
        if !requests.is_empty() || only_grid_path.is_some() {
            self.pointer_visual_dirty = true;
        }
        requests
    }

    pub(crate) fn begin_scroll_drag_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some((node_path, axis, offset)) = self.scroll_thumb_at(pointer) else {
            return false;
        };
        let pointer_start = match axis {
            ScrollAxis::X => pointer[0],
            ScrollAxis::Y => pointer[1],
        };
        self.scroll_drag = Some(ScrollDrag {
            node_path: node_path.clone(),
            axis,
            pointer_start,
            offset_start: offset,
        });
        if self
            .plan
            .iter()
            .any(|node| node.id == node_path && node.target.kind == UiNodeKind::DataGrid)
            && let Some(frame) = self.data_grid_frames.get(&node_path).cloned()
        {
            let body_offset = self
                .scroll_offsets
                .get(&node_path)
                .copied()
                .unwrap_or_default();
            self.data_grid_scroll_holds.insert(
                node_path,
                DataGridScrollHold {
                    body_offset,
                    desired_offset: body_offset,
                    fallback_frame: frame,
                    release_fragment_revision: None,
                    pending_sequence: None,
                },
            );
        }
        true
    }

    pub(crate) fn update_scroll_drag(&mut self) -> bool {
        let Some(drag) = self.scroll_drag.clone() else {
            return false;
        };
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(metrics) = self.scroll_metrics.get(&drag.node_path).copied() else {
            return false;
        };
        let Some(track) = scroll_track(metrics, drag.axis) else {
            return false;
        };
        let thumb_length = scroll_thumb_length(track, metrics, drag.axis);
        let travel = (scroll_axis_length(track, drag.axis) - thumb_length).max(1.0);
        let axis = scroll_axis_index(drag.axis);
        let pointer_position = match drag.axis {
            ScrollAxis::X => pointer[0],
            ScrollAxis::Y => pointer[1],
        };
        let offset = (drag.offset_start
            + (pointer_position - drag.pointer_start) * metrics.max_offset[axis] / travel)
            .clamp(0.0, metrics.max_offset[axis]);
        let offsets = self
            .scroll_offsets
            .entry(drag.node_path.clone())
            .or_insert([0.0; 2]);
        offsets[axis] = offset;
        if let Some(hold) = self.data_grid_scroll_holds.get_mut(&drag.node_path) {
            hold.desired_offset[axis] = offset;
        }
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn end_scroll_drag(&mut self) -> Option<String> {
        let data_grid = self.scroll_drag.as_ref().and_then(|drag| {
            self.data_grid_scroll_holds
                .contains_key(&drag.node_path)
                .then(|| drag.node_path.clone())
        });
        self.scroll_drag = None;
        data_grid
    }

    pub(crate) fn scroll_drag_active(&self) -> bool {
        self.scroll_drag.is_some()
    }

    pub(crate) fn data_grid_scroll_drag_active(&self) -> bool {
        self.scroll_drag
            .as_ref()
            .is_some_and(|drag| self.data_grid_scroll_holds.contains_key(&drag.node_path))
    }

    pub(crate) fn cancel_scroll_drag(&mut self) -> bool {
        let Some(drag) = self.scroll_drag.take() else {
            return false;
        };
        let Some(hold) = self.data_grid_scroll_holds.remove(&drag.node_path) else {
            return false;
        };
        self.scroll_offsets.insert(drag.node_path, hold.body_offset);
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn fail_data_grid_window_request(&mut self, sequence: u64) -> bool {
        let Some((grid_path, body_offset)) =
            self.data_grid_scroll_holds
                .iter()
                .find_map(|(grid_path, hold)| {
                    (hold.pending_sequence == Some(sequence))
                        .then(|| (grid_path.clone(), hold.body_offset))
                })
        else {
            return false;
        };
        self.data_grid_scroll_holds.remove(&grid_path);
        self.scroll_offsets.insert(grid_path, body_offset);
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn begin_scroll_pan_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(node) = self.plan.iter().rev().find(|node| {
            node.target.scroll
                && self.scroll_metrics.get(&node.id).is_some_and(|metrics| {
                    contains(metrics.viewport, pointer)
                        && (metrics.max_offset[0] > 0.0 || metrics.max_offset[1] > 0.0)
                })
        }) else {
            return false;
        };
        let offset = self
            .scroll_offsets
            .get(&node.id)
            .copied()
            .unwrap_or(node.target.declared_scroll_offset);
        self.scroll_pan = Some(ScrollPan {
            node_path: node.id.clone(),
            pointer_start: pointer,
            offset_start: offset,
        });
        true
    }

    pub(crate) fn update_scroll_pan(&mut self) -> bool {
        let Some(pan) = self.scroll_pan.clone() else {
            return false;
        };
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(metrics) = self.scroll_metrics.get(&pan.node_path).copied() else {
            return false;
        };
        let next = [
            (pan.offset_start[0] - (pointer[0] - pan.pointer_start[0]))
                .clamp(0.0, metrics.max_offset[0]),
            (pan.offset_start[1] - (pointer[1] - pan.pointer_start[1]))
                .clamp(0.0, metrics.max_offset[1]),
        ];
        let offsets = self
            .scroll_offsets
            .entry(pan.node_path)
            .or_insert(pan.offset_start);
        if *offsets == next {
            return false;
        }
        *offsets = next;
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn end_scroll_pan(&mut self) {
        self.scroll_pan = None;
    }

    pub(crate) fn scroll_pan_active(&self) -> bool {
        self.scroll_pan.is_some()
    }

    fn scroll_thumb_at(&self, pointer: [f32; 2]) -> Option<(String, ScrollAxis, f32)> {
        self.plan.iter().enumerate().rev().find_map(|(_, node)| {
            if !node.target.scroll {
                return None;
            }
            let metrics = self.scroll_metrics.get(&node.id).copied()?;
            let offsets = self
                .scroll_offsets
                .get(&node.id)
                .copied()
                .unwrap_or(node.target.declared_scroll_offset);
            [ScrollAxis::Y, ScrollAxis::X].into_iter().find_map(|axis| {
                let index = scroll_axis_index(axis);
                let track = scroll_track(metrics, axis)?;
                let thumb_length = scroll_thumb_length(track, metrics, axis);
                let thumb = scroll_thumb(track, metrics, axis, offsets[index], thumb_length);
                contains(thumb, pointer).then_some((node.id.clone(), axis, offsets[index]))
            })
        })
    }

    /// Starts a renderer-local high-frequency gesture. The value is previewed
    /// per frame and sent once as a typed commit when the pointer is released.
    pub(crate) fn begin_value_gesture(&mut self, binding: &UiHitBinding) -> bool {
        let Some(index) = self
            .plan
            .iter()
            .position(|node| node.id == binding.node_path)
        else {
            return false;
        };
        let visual = self.visual_at(index);
        let kind = self.plan[index].target.kind.clone();
        let (min, max) = match (&kind, &visual.presentation) {
            (
                UiNodeKind::Slider | UiNodeKind::DragValue,
                Some(UiControlPresentation::Numeric { min, max, .. }),
            ) => (*min, *max),
            (UiNodeKind::Scrollbar, Some(UiControlPresentation::Scroll { .. })) => (0.0, 1.0),
            _ => return false,
        };
        let bounds = match &kind {
            UiNodeKind::Slider => UiBounds {
                x: visual.bounds.x + visual.bounds.width * 0.57,
                y: visual.bounds.y,
                width: visual.bounds.width * 0.34,
                height: visual.bounds.height,
            },
            UiNodeKind::Scrollbar => UiBounds {
                x: visual.bounds.x + 10.0,
                y: visual.bounds.y,
                width: (visual.bounds.width - 20.0).max(1.0),
                height: visual.bounds.height,
            },
            UiNodeKind::DragValue => drag_value_bounds(visual.bounds),
            _ => visual.bounds,
        };
        if !self
            .pointer_position
            .is_some_and(|pointer| contains(bounds, pointer))
        {
            return false;
        }
        self.discard_pending_local_presentation_for(&binding.node_path);
        self.value_gesture = Some(UiValueGesture {
            node_path: binding.node_path.clone(),
            kind,
            bounds,
            min,
            max,
        });
        self.update_value_gesture()
    }

    pub(crate) fn requires_value_gesture(&self, binding: &UiHitBinding) -> bool {
        self.plan
            .iter()
            .find(|node| node.id == binding.node_path)
            .is_some_and(|node| {
                matches!(
                    node.target.kind,
                    UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar
                )
            })
    }

    pub(crate) fn update_value_gesture(&mut self) -> bool {
        let Some(gesture) = self.value_gesture.as_ref() else {
            return false;
        };
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let fraction =
            ((pointer[0] - gesture.bounds.x) / gesture.bounds.width.max(1.0)).clamp(0.0, 1.0);
        let value = gesture.min + (gesture.max - gesture.min) * fraction;
        let payload = match gesture.kind {
            UiNodeKind::DragValue => UiSemanticPayloadValue::I32 {
                value: value.round() as i32,
            },
            UiNodeKind::Slider | UiNodeKind::Scrollbar => UiSemanticPayloadValue::F32 { value },
            _ => return false,
        };
        self.value_previews
            .insert(gesture.node_path.clone(), payload);
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn finish_value_gesture(
        &mut self,
    ) -> Option<(UiSemanticPayloadValue, LocalPresentationCommit)> {
        let gesture = self.value_gesture.take()?;
        let value = self.value_previews.get(&gesture.node_path)?.clone();
        Some((
            value.clone(),
            LocalPresentationCommit::Value {
                node_path: gesture.node_path,
                value,
            },
        ))
    }

    pub(crate) fn cancel_value_gesture(&mut self) {
        if let Some(gesture) = self.value_gesture.take() {
            self.value_previews.remove(&gesture.node_path);
        }
    }

    pub(crate) fn value_gesture_active(&self) -> bool {
        self.value_gesture.is_some()
    }

    pub(crate) fn toggle_dropdown_at_pointer(&mut self) -> bool {
        if self.modal_active() {
            return false;
        }
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(node) = self
            .plan
            .iter()
            .enumerate()
            .find(|(index, node)| {
                node.target.kind == UiNodeKind::Dropdown
                    && contains(self.visual_at(*index).bounds, pointer)
                    && contains(self.visual_at(*index).clip, pointer)
            })
            .map(|(_, node)| node)
        else {
            return false;
        };
        if self.open_dropdown.as_deref() == Some(node.id.as_str()) {
            self.open_dropdown = None;
        } else {
            self.open_dropdown = Some(node.id.clone());
        }
        self.pointer_visual_dirty = true;
        true
    }

    pub(crate) fn dropdown_option_at_pointer(
        &self,
    ) -> Option<(UiHitBinding, UiSemanticPayloadValue)> {
        if self.modal_active() {
            return None;
        }
        let node_path = self.open_dropdown.as_ref()?;
        let pointer = self.pointer_position?;
        let (plan_index, rows) = self.dropdown_popup_layout()?;
        let node = &self.plan[plan_index];
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()?
        else {
            return None;
        };
        let index = rows.iter().position(|row| contains(*row, pointer))?;
        let value = options.get(index)?.clone();
        let binding = self
            .hit_bindings
            .values()
            .find(|binding| binding.node_path == *node_path)?
            .clone();
        Some((binding, UiSemanticPayloadValue::Enum { value }))
    }

    pub(crate) fn list_option_at_pointer(&self) -> Option<(UiHitBinding, UiSemanticPayloadValue)> {
        if self.modal_active() {
            return None;
        }
        let pointer = self.pointer_position?;
        let (index, node) = self.plan.iter().enumerate().find(|(index, node)| {
            node.target.kind == UiNodeKind::ListBox
                && contains(self.visual_at(*index).bounds, pointer)
                && contains(self.visual_at(*index).clip, pointer)
        })?;
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()?
        else {
            return None;
        };
        let index = list_box_rows(self.visual_at(index).bounds, options.len())
            .iter()
            .position(|row| contains(*row, pointer))?;
        let value = options.get(index)?.clone();
        let binding = self
            .hit_bindings
            .values()
            .find(|binding| binding.node_path == node.id)?
            .clone();
        Some((binding, UiSemanticPayloadValue::Enum { value }))
    }

    pub(crate) fn tab_option_at_pointer(&self) -> Option<(UiHitBinding, UiSemanticPayloadValue)> {
        if self.modal_active() {
            return None;
        }
        let pointer = self.pointer_position?;
        let (index, node) = self.plan.iter().enumerate().find(|(index, node)| {
            node.target.kind == UiNodeKind::Tabs
                && self.visual_at(*index).enabled
                && contains(self.visual_at(*index).bounds, pointer)
                && contains(self.visual_at(*index).clip, pointer)
        })?;
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()?
        else {
            return None;
        };
        let index = tab_segments(self.visual_at(index).bounds, options.len())
            .iter()
            .position(|segment| tag_contains(*segment, pointer))?;
        let value = options.get(index)?.clone();
        let binding = self
            .hit_bindings
            .values()
            .find(|binding| binding.node_path == node.id)?
            .clone();
        Some((binding, UiSemanticPayloadValue::Enum { value }))
    }

    pub(crate) fn close_dropdown(&mut self) {
        self.open_dropdown = None;
        self.pointer_visual_dirty = true;
    }

    pub(crate) fn dropdown_debug_snapshot(&self) -> Value {
        let diagnostic_node_path = |node_path: &str| {
            self.hit_bindings
                .values()
                .find(|binding| binding.node_path == node_path && binding.data_grid_cell.is_none())
                .map(|binding| binding.node_path.as_str())
        };
        let popup = self.dropdown_popup_layout().map(|(plan_index, rows)| {
            let anchor = self.visual_at(plan_index).bounds;
            json!({
                "node_path": diagnostic_node_path(& self.plan[plan_index].id),
                "anchor": {"x": anchor.x, "y": anchor.y, "width": anchor.width, "height": anchor.height},
                "rows": rows.iter().map(|row| json!({"x": row.x, "y": row.y, "width": row.width, "height": row.height})).collect::<Vec<_>>(),
            })
        });
        let open_dropdown = self.open_dropdown.as_deref().and_then(diagnostic_node_path);
        json!({
            "viewport": self.viewport_physical_size,
            "viewport_logical": self.viewport_logical_size,
            "viewport_revision": self.viewport_revision,
            "open_dropdown":open_dropdown,
            "popup": popup,
        })
    }

    /// A top-level popup consumes an outside press before underlying controls
    /// see it, matching conventional menu dismissal behavior.
    pub(crate) fn dismiss_dropdown_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else {
            return false;
        };
        let source = self.visual_at(plan_index).bounds;
        if contains(source, pointer) || rows.iter().any(|row| contains(*row, pointer)) {
            return false;
        }
        self.close_dropdown();
        true
    }

    /// Modal dismissal is intentionally renderer-local: it only consumes an
    /// outside press and clears local text focus. Closing remains declarative.
    pub(crate) fn dismiss_modal_at_pointer(&mut self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(index) = self.active_modal_index() else {
            return false;
        };
        if contains(self.plan[index].target.bounds, pointer) {
            return false;
        }
        self.clear_text_focus();
        self.pointer_visual_dirty = true;
        true
    }

    fn active_modal_index(&self) -> Option<usize> {
        self.plan
            .iter()
            .rposition(|node| matches!(node.target.kind, UiNodeKind::Modal | UiNodeKind::Dialog))
    }

    fn modal_active(&self) -> bool {
        self.active_modal_index().is_some()
    }

    fn modal_blocks_pointer(&self) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        self.active_modal_index()
            .is_some_and(|index| !contains(self.plan[index].target.bounds, pointer))
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
            .find_map(|input| {
                let (bounds, clip) = self
                    .plan
                    .iter()
                    .position(|node| node.id == input.node_path)
                    .map(|index| {
                        let visual = self.visual_at(index);
                        (visual.bounds, visual.clip)
                    })
                    .unwrap_or((input.bounds, input.bounds));
                (self.active_modal_allows_node(&input.node_path)
                    && contains(bounds, pointer)
                    && contains(clip, pointer))
                .then(|| UiTextInputBinding {
                    bounds,
                    ..input.clone()
                })
            })
    }

    /// Focus is renderer-local presentation state. Semantic events still carry
    /// only their declared intent and never this path or a hit identifier.
    pub(crate) fn focus_control_at_pointer(&mut self) -> bool {
        if self.modal_blocks_pointer() {
            return false;
        }
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(path) = self.hit_bindings.values().find_map(|binding| {
            self.plan
                .iter()
                .enumerate()
                .find(|(_, node)| node.id == binding.node_path)
                .filter(|(index, node)| {
                    self.active_modal_allows_node(&node.id)
                        && self.visual_at(*index).enabled
                        && contains(self.visual_at(*index).bounds, pointer)
                        && contains(self.visual_at(*index).clip, pointer)
                })
                .map(|_| binding.node_path.clone())
        }) else {
            return false;
        };
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

    fn visual_at(&self, index: usize) -> &UiVisual {
        self.sampled.get(index).unwrap_or(&self.plan[index].target)
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
                        width: self.viewport_logical_size[0].max(1.0),
                        height: self.viewport_logical_size[1].max(1.0),
                    }),
                    UiDragBoundary::Free => None,
                };
                self.discard_pending_local_presentation_for(&source_path);
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
        let result = result.flatten().map(|mut resolved| {
            let offset = self
                .drag_offsets
                .get(&active.source_path)
                .copied()
                .unwrap_or(active.origin);
            resolved.local_presentation = LocalPresentationCommit::Drag {
                source_path: active.source_path.clone(),
                offset,
            };
            resolved
        });
        if result.is_none() {
            self.drag_offsets.remove(&active.source_path);
        }
        self.pointer_visual_dirty = true;
        result
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

    pub(crate) fn active_drag_semantic_source(&self) -> Option<(String, UiFragmentRevision)> {
        self.drag.as_ref().map(|active| {
            (
                active.binding.source_node_id.0.clone(),
                active.fragment.clone(),
            )
        })
    }

    pub(crate) fn active_drag_moved(&self) -> bool {
        self.drag.as_ref().is_some_and(|active| active.moved)
    }

    pub(crate) fn current_drag_drop_target(
        &self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> Option<UiResolvedDragDrop> {
        let active = self.drag.as_ref()?;
        let pointer = self.pointer_position?;
        active
            .moved
            .then(|| self.resolve_drop_target(fragments, active, pointer))
            .flatten()
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
                            local_presentation: LocalPresentationCommit::Drag {
                                source_path: active.source_path.clone(),
                                offset: [0.0; 2],
                            },
                        })
                })
            })
    }

    pub(crate) fn retain_local_presentation(
        &mut self,
        semantic_sequence: u64,
        fragment: &UiFragmentRevision,
        presentation: LocalPresentationCommit,
    ) -> PendingLocalPresentationKey {
        let node_path = presentation.node_path().to_owned();
        self.pending_local_presentations
            .retain(|_, pending| pending.presentation.node_path() != node_path);
        let key = PendingLocalPresentationKey {
            semantic_sequence,
            fragment_id: fragment.id.0.clone(),
            fragment_revision: fragment.revision.0,
        };
        self.pending_local_presentations.insert(
            key.clone(),
            PendingLocalPresentationCommit {
                presentation,
                delivery_accepted: false,
                presentation_applied: true,
            },
        );
        key
    }

    pub(crate) fn complete_local_presentation(
        &mut self,
        key: &PendingLocalPresentationKey,
        accepted: bool,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> bool {
        if accepted {
            let Some(pending) = self.pending_local_presentations.get_mut(key) else {
                return false;
            };
            pending.delivery_accepted = true;
            return self.reconcile_pending_local_presentations(fragments);
        }
        let Some(pending) = self.pending_local_presentations.remove(key) else {
            return false;
        };
        self.clear_local_presentation(&pending.presentation)
    }

    pub(crate) fn reconcile_pending_local_presentations(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> bool {
        let advanced = self
            .pending_local_presentations
            .iter()
            .filter(|(key, _)| {
                fragments
                    .get(&neon_ui_schema::UiFragmentId(key.fragment_id.clone()))
                    .is_none_or(|fragment| fragment.revision.0 > key.fragment_revision)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for key in advanced {
            let (presentation, applied, accepted) = {
                let pending = self
                    .pending_local_presentations
                    .get_mut(&key)
                    .expect("pending presentation key was collected from this map");
                let applied = pending.presentation_applied;
                pending.presentation_applied = false;
                (
                    pending.presentation.clone(),
                    applied,
                    pending.delivery_accepted,
                )
            };
            if applied {
                changed |= self.clear_local_presentation(&presentation);
            }
            if accepted {
                self.pending_local_presentations.remove(&key);
            }
        }
        changed
    }

    pub(crate) fn rollback_local_presentation(
        &mut self,
        presentation: &LocalPresentationCommit,
    ) -> bool {
        self.clear_local_presentation(presentation)
    }

    pub(crate) fn cancel_pending_local_presentations(&mut self) -> bool {
        let presentations = self
            .pending_local_presentations
            .drain()
            .map(|(_, pending)| pending.presentation)
            .collect::<Vec<_>>();
        let mut changed = false;
        for presentation in presentations {
            changed |= self.clear_local_presentation(&presentation);
        }
        changed
    }

    fn discard_pending_local_presentation_for(&mut self, node_path: &str) {
        self.pending_local_presentations
            .retain(|_, pending| pending.presentation.node_path() != node_path);
    }

    fn clear_local_presentation(&mut self, presentation: &LocalPresentationCommit) -> bool {
        let changed = match presentation {
            LocalPresentationCommit::Value { node_path, value } => {
                self.value_previews
                    .get(node_path)
                    .is_some_and(|current| current == value)
                    && self.value_previews.remove(node_path).is_some()
            }
            LocalPresentationCommit::Drag {
                source_path,
                offset,
            } => {
                self.drag_offsets
                    .get(source_path)
                    .is_some_and(|current| current == offset)
                    && self.drag_offsets.remove(source_path).is_some()
            }
        };
        if changed {
            self.pointer_visual_dirty = true;
        }
        changed
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
        if self.editing.node_path.as_deref() == Some(binding.node_path.as_str()) {
            return;
        }
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
            .position(|node| node.id == node_path)
            .map(|index| self.visual_at(index).bounds)
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
        if self
            .text_input_binding(&node_path)
            .is_some_and(|binding| binding.data_grid_cell.is_some())
        {
            return None;
        }
        let binding = self.text_input_binding(&node_path)?;
        Some((binding, committed))
    }

    pub(crate) fn backspace_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let committed = self.editing.backspace()?;
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        if self
            .text_input_binding(&node_path)
            .is_some_and(|binding| binding.data_grid_cell.is_some())
        {
            return None;
        }
        Some((self.text_input_binding(&node_path)?, committed))
    }

    pub(crate) fn delete_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let committed = self.editing.delete()?;
        self.ensure_text_input_caret_visible();
        self.pointer_visual_dirty = true;
        if self
            .text_input_binding(&node_path)
            .is_some_and(|binding| binding.data_grid_cell.is_some())
        {
            return None;
        }
        Some((self.text_input_binding(&node_path)?, committed))
    }

    pub(crate) fn data_grid_text_input_active(&self) -> bool {
        self.editing
            .node_path
            .as_deref()
            .and_then(|path| self.text_input_binding(path))
            .is_some_and(|binding| binding.data_grid_cell.is_some())
    }

    pub(crate) fn active_text_input_path(&self) -> Option<&str> {
        self.editing.node_path.as_deref()
    }

    pub(crate) fn finish_data_grid_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let binding = self.text_input_binding(&node_path)?;
        if binding.data_grid_cell.is_none() {
            return None;
        }
        let value = self.editing.committed.clone();
        if let Some(identity) = data_grid_cell_identity(&binding) {
            self.data_grid_text_display_cache.insert(
                identity,
                CachedDataGridTextDisplay {
                    text: value.clone(),
                },
            );
        }
        self.editing.clear();
        self.pointer_visual_dirty = true;
        Some((binding, value))
    }

    pub(crate) fn cancel_data_grid_text_input(&mut self) -> bool {
        if !self.data_grid_text_input_active() {
            return false;
        }
        self.editing.clear();
        self.pointer_visual_dirty = true;
        true
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
            .position(|node| &node.id == node_path)
            .map(|index| self.visual_at(index).bounds)?;
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
            .position(|node| &node.id == node_path)
            .map(|index| self.visual_at(index).bounds)
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

    pub(crate) fn image_debug_snapshot(&self) -> serde_json::Value {
        let sampled_images = self
            .sampled
            .iter()
            .filter_map(|visual| visual.image.as_ref().map(|asset| (visual, asset)))
            .map(|(visual, asset)| {
                let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
                serde_json::json!({
                    "asset": asset,
                    "bounds": visual.bounds,
                    "clip": visual.clip,
                    "resident": self.resident_images.contains_key(&key),
                    "uv": self.resident_images.get(&key).map(|image| image.uv),
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "atlas_ready": self.image_atlas.is_some(),
            "resident_count": self.resident_images.len(),
            "sampled_images": sampled_images,
        })
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
        let Some(byte_len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return Err("invalid_image_bytes");
        };
        if width == 0
            || height == 0
            || width > IMAGE_ATLAS_WIDTH - IMAGE_ATLAS_PADDING * 2
            || content.bytes.len() != byte_len
        {
            return Err("invalid_image_bytes");
        }
        self.resident_images.insert(
            (
                content.asset.project_id.clone(),
                content.asset.asset_id,
                content.asset.revision.0,
            ),
            ResidentImage {
                width,
                height,
                bytes: content.bytes.clone(),
                uv: [0.0; 4],
            },
        );
        self.rebuild_image_atlas(device, queue);
        Ok(())
    }

    fn rebuild_image_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut keys = self.resident_images.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut placements = Vec::with_capacity(keys.len());
        let mut x = IMAGE_ATLAS_PADDING;
        let mut y = IMAGE_ATLAS_PADDING;
        let mut row_height = 0;
        for key in &keys {
            let image = &self.resident_images[key];
            if x + image.width + IMAGE_ATLAS_PADDING > IMAGE_ATLAS_WIDTH {
                x = IMAGE_ATLAS_PADDING;
                y += row_height + IMAGE_ATLAS_PADDING;
                row_height = 0;
            }
            placements.push((key.clone(), x, y));
            x += image.width + IMAGE_ATLAS_PADDING;
            row_height = row_height.max(image.height);
        }
        let atlas_height = (y + row_height + IMAGE_ATLAS_PADDING).max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-ui-image-atlas"),
            size: wgpu::Extent3d {
                width: IMAGE_ATLAS_WIDTH,
                height: atlas_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (key, x, y) in placements {
            let image = self
                .resident_images
                .get_mut(&key)
                .expect("image placement has a resident image");
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &image.bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
            image.uv = [
                (x as f32 + 0.5) / IMAGE_ATLAS_WIDTH as f32,
                (y as f32 + 0.5) / atlas_height as f32,
                (image.width as f32 - 1.0) / IMAGE_ATLAS_WIDTH as f32,
                (image.height as f32 - 1.0) / atlas_height as f32,
            ];
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-image-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-image-atlas-bind-group"),
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
        self.image_atlas = Some(ResidentImageAtlas {
            _texture: texture,
            _view: view,
            _sampler: sampler,
            bind_group,
        });
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
                | wgpu::TextureUsages::RENDER_ATTACHMENT
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

    /// Creates a renderer-private color target suitable for drawing an ordinary
    /// UiNode subtree with this renderer's panel/text pipelines.
    pub(crate) fn ensure_ui_render_surface(
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
            label: Some("neon3-ui-color-render-surface"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.color_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        self.register_render_surface(device, target_id, texture);
        self.resident_render_surfaces[target_id]._view.clone()
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
        viewport_physical_size: [u32; 2],
        viewport_logical_size: [f32; 2],
        time_seconds: f32,
    ) {
        self.ensure_builtin_font(device, queue);
        self.update_viewport(viewport_physical_size, viewport_logical_size);
        if self.view_buffer_viewport_revision != self.viewport_revision {
            queue.write_buffer(
                &self.view_buffer,
                0,
                bytemuck::bytes_of(&UiView {
                    viewport: self.viewport_logical_size,
                    _pad: [0.0; 2],
                }),
            );
            self.view_buffer_viewport_revision = self.viewport_revision;
        }
        self.refresh_plan(fragments, viewport_logical_size);
        self.instances.clear();
        let top_layer = self.compose_sampled_visuals(time_seconds);
        let plan_index = self
            .plan
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect::<HashMap<_, _>>();
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
                let visual = &self.sampled[index];
                self.instances
                    .push(self.instance(visual, &self.plan[index].id, time_seconds));
                self.instances
                    .extend(self.component_chrome_instances(visual, &self.plan[index].id));
            }
        }
        // CPU first-press handling must be ready as soon as the visible frame is
        // drawn; asynchronous GPU hit readback is only supplemental.
        self.refresh_hit_bindings(fragments);
        // Reuse the dropdown popup resource path for declarative top-level layers.
        // Modal backdrops are inserted immediately before their own subtree.
        let mut popup_instances = self.dropdown_popup_instances();
        for index in 0..self.plan.len() {
            if self.plan[index].target.scroll {
                popup_instances.extend(
                    self.scroll_chrome_instances(&self.sampled[index], &self.plan[index].id),
                );
            }
        }
        for index in 0..self.plan.len() {
            let Some(root) = top_layer[index] else {
                continue;
            };
            if self.plan[root].target.kind == UiNodeKind::Tooltip && !self.tooltip_hovered(root) {
                continue;
            }
            if root == index
                && matches!(
                    self.plan[index].target.kind,
                    UiNodeKind::Modal | UiNodeKind::Dialog
                )
            {
                popup_instances.push(overlay_instance(
                    UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: self.viewport_logical_size[0],
                        height: self.viewport_logical_size[1],
                    },
                    UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: self.viewport_logical_size[0],
                        height: self.viewport_logical_size[1],
                    },
                    [0.0, 0.0, 0.0, 0.45],
                ));
            }
            if self.plan[index].instance_index.is_some() {
                popup_instances.push(self.instance(
                    &self.sampled[index],
                    &self.plan[index].id,
                    time_seconds,
                ));
                popup_instances.extend(
                    self.component_chrome_instances(&self.sampled[index], &self.plan[index].id),
                );
            }
        }
        self.pointer_visual_dirty = false;
        self.append_text_input_overlays();
        self.last_panel_instance_count = self.instances.len();
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
        }
        if popup_instances.len() > self.popup_instance_capacity {
            self.popup_instance_capacity = popup_instances.len().next_power_of_two();
            self.popup_instance_buffer =
                create_instance_buffer(device, self.popup_instance_capacity);
        }
        // The instance vector is rebuilt from the current plan on every draw.
        // Upload it as a complete snapshot so a composition update cannot draw
        // a newly sized DataGrid from stale buffer contents.
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
        let images = self
            .sampled
            .iter()
            .filter_map(|visual| {
                let asset = visual.image.as_ref()?;
                let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
                self.resident_images.get(&key).map(|image| UiImageInstance {
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
                    uv: image.uv,
                    depth: visual.world_depth.unwrap_or(0.0),
                })
            })
            .collect::<Vec<_>>();
        if !images.is_empty() {
            if images.len() > self.image_capacity {
                self.image_capacity = images.len().next_power_of_two();
                self.image_buffer = create_image_buffer(device, self.image_capacity);
            }
            queue.write_buffer(&self.image_buffer, 0, bytemuck::cast_slice(&images));
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
                            uv: [0.0, 0.0, 1.0, 1.0],
                            depth: visual.world_depth.unwrap_or(0.0),
                        },
                    ))
            })
            .collect::<Vec<_>>();
        let dropdown_texts = self.dropdown_option_texts();
        let list_box_texts = self.list_box_option_texts();
        let tab_texts = self.tab_option_texts();
        let drag_value_texts = self.drag_value_texts();
        let visible_top_layer = top_layer
            .iter()
            .map(|root| {
                root.is_none_or(|root| {
                    self.plan[root].target.kind != UiNodeKind::Tooltip || self.tooltip_hovered(root)
                })
            })
            .collect::<Vec<_>>();
        let (texts, popup_texts) = self
            .resident_font
            .as_mut()
            .map(|font| {
                let texts = self
                    .sampled
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
                            UiNodeKind::Label
                                | UiNodeKind::Button
                                | UiNodeKind::TextInput
                                | UiNodeKind::Checkbox
                                | UiNodeKind::RadioButton
                                | UiNodeKind::Slider
                                | UiNodeKind::DragValue
                                | UiNodeKind::Combo
                                | UiNodeKind::Dropdown
                                | UiNodeKind::Selectable
                                | UiNodeKind::Scrollbar
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
                for (visual, text) in &tab_texts {
                    if let Some(instances) = layout_text(device, queue, font, visual, text, None) {
                        texts.extend(instances);
                    }
                }
                for (visual, text) in &drag_value_texts {
                    if let Some(instances) = layout_text(device, queue, font, visual, text, None) {
                        texts.extend(instances);
                    }
                }
                let mut popup_texts = dropdown_texts
                    .iter()
                    .flat_map(|(visual, text)| {
                        layout_text(device, queue, font, visual, text, None).unwrap_or_default()
                    })
                    .collect::<Vec<_>>();
                for (index, visual) in self.sampled.iter().enumerate() {
                    if top_layer[index].is_none() {
                        continue;
                    }
                    if !visible_top_layer[index] {
                        continue;
                    }
                    if let Some(text) = visual.text.as_ref().and_then(text_ref_value)
                        && matches!(
                            visual.kind,
                            UiNodeKind::Label
                                | UiNodeKind::Button
                                | UiNodeKind::TextInput
                                | UiNodeKind::Checkbox
                                | UiNodeKind::RadioButton
                                | UiNodeKind::Slider
                                | UiNodeKind::DragValue
                                | UiNodeKind::Combo
                                | UiNodeKind::Dropdown
                                | UiNodeKind::Tabs
                                | UiNodeKind::Selectable
                                | UiNodeKind::Scrollbar
                                | UiNodeKind::ProgressBar
                                | UiNodeKind::Tooltip
                        )
                        && let Some(instances) =
                            layout_text(device, queue, font, visual, text, None)
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
        }
        // Treat equal world depth as one paint group. Groups are emitted far
        // to near; within a group the panel batch is emitted before its text
        // batch, so text stays above its owning panel while a nearer panel
        // still covers a farther group's text.
        let mut rect_groups: Vec<(u32, Vec<UiInstance>)> = Vec::new();
        for instance in &self.instances {
            let key = instance.depth.to_bits();
            if let Some((_, group)) = rect_groups.iter_mut().find(|(group_key, _)| *group_key == key) {
                group.push(*instance);
            } else {
                rect_groups.push((key, vec![*instance]));
            }
        }
        let mut text_groups: Vec<(u32, Vec<UiTextInstance>)> = Vec::new();
        for text in &texts {
            let key = text.depth.to_bits();
            if let Some((_, group)) = text_groups.iter_mut().find(|(group_key, _)| *group_key == key) {
                group.push(*text);
            } else {
                text_groups.push((key, vec![*text]));
            }
        }
        let mut image_groups: Vec<(u32, Vec<UiImageInstance>)> = Vec::new();
        for image in &images {
            let key = image.depth.to_bits();
            if let Some((_, group)) = image_groups.iter_mut().find(|(group_key, _)| *group_key == key) {
                group.push(*image);
            } else {
                image_groups.push((key, vec![*image]));
            }
        }
        let surface_groups = surfaces
            .iter()
            .map(|(surface_id, surface)| (surface.depth.to_bits(), surface_id, *surface))
            .collect::<Vec<_>>();
        let mut depth_keys = rect_groups
            .iter()
            .map(|(key, _)| *key)
            .chain(image_groups.iter().map(|(key, _)| *key))
            .chain(surface_groups.iter().map(|(key, _, _)| *key))
            .chain(text_groups.iter().map(|(key, _)| *key))
            .collect::<Vec<_>>();
        depth_keys.sort_by(|a, b| f32::from_bits(*b).partial_cmp(&f32::from_bits(*a)).unwrap_or(std::cmp::Ordering::Equal));
        depth_keys.dedup();
        for key in depth_keys {
            if let Some((_, group)) = rect_groups.iter().find(|(group_key, _)| *group_key == key) {
                queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(group));
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..6, 0..group.len() as u32);
            }
            if let Some((_, group)) = image_groups.iter().find(|(group_key, _)| *group_key == key) {
                queue.write_buffer(&self.image_buffer, 0, bytemuck::cast_slice(group));
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_bind_group(1, &self.image_atlas.as_ref().expect("resident image atlas").bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                pass.draw(0..6, 0..group.len() as u32);
            }
            for (_, surface_id, surface) in surface_groups.iter().filter(|(group_key, _, _)| *group_key == key) {
                queue.write_buffer(&self.image_buffer, 0, bytemuck::bytes_of(surface));
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_bind_group(1, &self.resident_render_surfaces[*surface_id].bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
            if let Some((_, group)) = text_groups.iter().find(|(group_key, _)| *group_key == key) {
                queue.write_buffer(&self.text_buffer, 0, bytemuck::cast_slice(group));
                pass.set_pipeline(&self.text_pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_bind_group(1, &self.resident_font.as_ref().unwrap().bind_group, &[]);
                pass.set_vertex_buffer(0, self.text_buffer.slice(..));
                pass.draw(0..6, 0..group.len() as u32);
            }
        }

        // Group draws use the shared instance buffer as a small upload scratch
        // area. Restore the complete snapshot because draw_depth() follows
        // this pass and needs the original instance indexing.
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
        if !popup_instances.is_empty() {
            queue.write_buffer(
                &self.popup_instance_buffer,
                0,
                bytemuck::cast_slice(&popup_instances),
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            pass.set_vertex_buffer(0, self.popup_instance_buffer.slice(..));
            pass.draw(0..6, 0..popup_instances.len() as u32);
        }
        if !popup_texts.is_empty() {
            queue.write_buffer(
                &self.popup_text_buffer,
                0,
                bytemuck::cast_slice(&popup_texts),
            );
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

    /// World-anchor projection changes node bounds without changing the UI
    /// program revision. External-surface rendering must rebuild the layout
    /// plan before consuming that projected snapshot.
    pub(crate) fn invalidate_plan(&mut self) {
        self.plan_revisions.clear();
    }

    fn update_viewport(&mut self, physical_size: [u32; 2], logical_size: [f32; 2]) -> bool {
        let physical_size = [physical_size[0].max(1), physical_size[1].max(1)];
        let logical_size = normalize_logical_viewport(logical_size, physical_size);
        if self.viewport_physical_size == physical_size
            && self.viewport_logical_size == logical_size
        {
            return false;
        }
        self.viewport_physical_size = physical_size;
        self.viewport_logical_size = logical_size;
        self.viewport_revision = self.viewport_revision.wrapping_add(1).max(1);
        true
    }

    /// Re-emits the already-composed panel instances into a depth-only pass.
    /// This target describes UI-to-host-scene occlusion; it is deliberately
    /// separate from the 2D painter order used by the color pass.
    pub(crate) fn draw_depth<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        let Some(rect_pipeline) = &self.depth_pipeline else {
            return;
        };
        pass.set_pipeline(rect_pipeline);
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, 0..self.instances.len() as u32);
    }

    fn refresh_plan(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport_logical_size: [f32; 2],
    ) -> bool {
        self.reconcile_pending_local_presentations(fragments);
        let data_grid_hold_changed = self.reconcile_data_grid_scroll_holds(fragments);
        let viewport_logical_size =
            normalize_logical_viewport(viewport_logical_size, self.viewport_physical_size);
        if self.viewport_logical_size != viewport_logical_size {
            self.viewport_logical_size = viewport_logical_size;
            self.viewport_revision = self.viewport_revision.wrapping_add(1).max(1);
        }
        let viewport_changed = self.plan_viewport_revision != self.viewport_revision;
        let matches = self.plan_revisions.len() == fragments.len()
            && !viewport_changed
            && fragments
                .iter()
                .all(|(id, fragment)| self.plan_revisions.get(id) == Some(&fragment.revision));
        if matches && !data_grid_hold_changed {
            return false;
        }
        let display_fragments = self.data_grid_display_fragments(fragments);
        self.reconcile_data_grid_text_display_cache(&display_fragments);
        let nodes = flatten_fragments_with_data_grid_display_cache(
            &display_fragments,
            viewport_logical_size,
            self.resident_font.as_ref(),
            &self.data_grid_text_display_cache,
            &self.available_cameras,
        );
        let live: HashSet<_> = nodes.iter().map(|(id, _, _, _)| id.clone()).collect();
        if viewport_changed {
            self.current.clear();
            self.active.clear();
        } else {
            self.current.retain(|id, _| live.contains(id));
            self.active.retain(|id, _| live.contains(id));
        }
        self.plan.clear();
        self.debug_semantic_nodes.clear();
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
        for fragment in fragments.values() {
            for (node_key, plan_path) in collect_node_paths(&fragment.fragment_id.0, &fragment.root)
            {
                if self.plan.iter().any(|node| node.id == plan_path) {
                    self.debug_semantic_nodes.push(DebugSemanticNode {
                        fragment_id: fragment.fragment_id.0.clone(),
                        node_key,
                        plan_path,
                    });
                }
            }
        }
        self.scroll_offsets.retain(|id, _| {
            self.plan
                .iter()
                .any(|node| node.id == *id && node.target.scroll)
        });
        for node in &self.plan {
            if node.target.scroll {
                self.scroll_offsets
                    .entry(node.id.clone())
                    .or_insert(node.target.declared_scroll_offset);
            }
        }
        self.plan_revisions.clear();
        self.plan_revisions.extend(
            fragments
                .iter()
                .map(|(id, fragment)| (id.clone(), fragment.revision)),
        );
        self.plan_viewport_revision = self.viewport_revision;
        true
    }

    fn reconcile_data_grid_scroll_holds(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> bool {
        let mut live = HashSet::new();
        let mut covered = Vec::new();
        self.data_grid_frames.clear();
        for fragment in fragments.values() {
            for effect in &fragment.effects {
                let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                    continue;
                };
                let grid_path = format!("{}/{}", fragment.fragment_id.0, declaration.node_key);
                live.insert(grid_path.clone());
                self.data_grid_frames
                    .insert(grid_path.clone(), frame.clone());
                let Some(hold) = self.data_grid_scroll_holds.get(&grid_path) else {
                    continue;
                };
                if self
                    .scroll_drag
                    .as_ref()
                    .is_some_and(|drag| drag.node_path == grid_path)
                {
                    continue;
                }
                let viewport_height = self
                    .plan
                    .iter()
                    .find(|node| node.id == grid_path)
                    .map_or(0.0, |node| node.target.bounds.height);
                let replacement_allowed =
                    match (hold.pending_sequence, hold.release_fragment_revision) {
                        (Some(_), Some(revision)) => fragment.revision > revision,
                        (None, None) => true,
                        _ => false,
                    };
                if replacement_allowed
                    && data_grid_frame_covers_offset(
                        frame,
                        declaration,
                        hold.desired_offset[1],
                        viewport_height,
                    )
                {
                    covered.push(grid_path);
                }
            }
        }
        self.data_grid_scroll_holds
            .retain(|grid_path, _| live.contains(grid_path));
        let changed = !covered.is_empty();
        for grid_path in covered {
            self.data_grid_scroll_holds.remove(&grid_path);
        }
        changed
    }

    fn data_grid_display_fragments(
        &self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> HashMap<neon_ui_schema::UiFragmentId, UiFragment> {
        if self.data_grid_scroll_holds.is_empty() {
            return fragments.clone();
        }
        let mut display = fragments.clone();
        for fragment in display.values_mut() {
            for effect in &mut fragment.effects {
                let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                    continue;
                };
                let grid_path = format!("{}/{}", fragment.fragment_id.0, declaration.node_key);
                if let Some(hold) = self.data_grid_scroll_holds.get(&grid_path) {
                    *frame = hold.fallback_frame.clone();
                }
            }
        }
        display
    }

    fn reconcile_data_grid_text_display_cache(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) {
        let mut frame_cells = HashMap::new();
        let mut sources = HashSet::new();

        for fragment in fragments.values() {
            for effect in &fragment.effects {
                let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                    continue;
                };
                sources.insert(declaration.source_key.clone());
                for row in frame
                    .window_rows
                    .iter()
                    .take(declaration.max_window_rows as usize)
                {
                    for (column_key, cell) in &row.cells {
                        let identity = DataGridCellIdentity {
                            source_key: declaration.source_key.clone(),
                            stable_row_key: row.stable_row_key.clone(),
                            column_key: column_key.clone(),
                        };
                        frame_cells.insert(identity, cell);
                    }
                }
            }
        }

        self.data_grid_text_display_cache.retain(|identity, _| {
            sources.contains(&identity.source_key)
                && frame_cells.get(identity).is_some_and(|cell| {
                    matches!(cell.value, neon_ui_schema::UiInputValue::TextHandle { .. })
                })
        });
    }

    fn refresh_hit_bindings(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) -> Vec<(u32, usize)> {
        let declarations = collect_hit_declarations(&self.data_grid_display_fragments(fragments));
        self.hit_bindings.clear();
        let mut hit_nodes = Vec::new();
        for index in 0..self.plan.len() {
            let node_path = self.plan[index].id.clone();
            let visual = self.visual_at(index);
            let kind = visual.kind.clone();
            let enabled = visual.enabled;
            let opacity = visual.style.opacity;
            let bounds = visual.bounds;
            let has_drag_binding = fragments.iter().any(|(fragment_id, fragment)| {
                let source_path = format!("{}/", fragment_id.0);
                node_path.strip_prefix(&source_path).is_some_and(|node_id| {
                    fragment.effects.iter().any(|effect| {
                        matches!(effect, neon_ui_schema::UiEffect::DragBinding { binding } if binding.source_node_id.0 == node_id)
                    })
                })
            });
            if (!is_interactive_control(&kind) && !has_drag_binding)
                || !enabled
                || opacity <= 0.0
                || (is_data_grid_cell_path(&node_path) && !declarations.contains_key(&node_path))
            {
                continue;
            }
            let hit_id = hit_nodes.len() as u32 + 1;
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
                    data_grid_cell: None,
                    control_value: None,
                    max_text_length: None,
                });
            if kind == UiNodeKind::TextInput {
                binding.text_input = Some(UiTextInputBinding {
                    node_path,
                    max_length: binding.max_text_length.unwrap_or(256),
                    bounds,
                });
            }
            self.hit_bindings.insert(hit_id, binding);
            hit_nodes.push((hit_id, index));
        }
        hit_nodes
    }

    fn update_scroll_metrics(&mut self) {
        self.scroll_metrics.clear();
        for node in &self.plan {
            if !node.target.scroll {
                continue;
            }
            let viewport = node.target.bounds;
            let raw_content_extent = self
                .plan
                .iter()
                .filter_map(|child| {
                    (child.parent_id.as_deref() == Some(node.id.as_str())).then_some([
                        child.target.bounds.x + child.target.bounds.width,
                        child.target.bounds.y + child.target.bounds.height,
                    ])
                })
                .fold([viewport.x, viewport.y], |extent, child| {
                    [extent[0].max(child[0]), extent[1].max(child[1])]
                });
            let raw_content_size = [
                (raw_content_extent[0] - viewport.x).max(0.0),
                (raw_content_extent[1] - viewport.y).max(0.0),
            ];
            let content_size = if node.target.kind == UiNodeKind::DataGrid {
                let mut horizontal = false;
                let mut vertical = false;
                for _ in 0..3 {
                    horizontal = raw_content_size[0]
                        > (viewport.width
                            - if vertical {
                                DATA_GRID_SCROLLBAR_GUTTER
                            } else {
                                0.0
                            })
                        .max(0.0);
                    vertical = raw_content_size[1]
                        > (viewport.height
                            - if horizontal {
                                DATA_GRID_SCROLLBAR_GUTTER
                            } else {
                                0.0
                            })
                        .max(0.0);
                }
                [
                    (raw_content_size[0]
                        + if vertical {
                            DATA_GRID_SCROLLBAR_GUTTER
                        } else {
                            0.0
                        })
                    .max(viewport.width),
                    (raw_content_size[1]
                        + if horizontal {
                            DATA_GRID_SCROLLBAR_GUTTER
                        } else {
                            0.0
                        })
                    .max(viewport.height),
                ]
            } else {
                [
                    raw_content_size[0].max(viewport.width),
                    raw_content_size[1].max(viewport.height),
                ]
            };
            let metrics = ScrollMetrics {
                viewport,
                content_size,
                max_offset: [
                    (content_size[0] - viewport.width).max(0.0),
                    (content_size[1] - viewport.height).max(0.0),
                ],
            };
            if let Some(offset) = self.scroll_offsets.get_mut(&node.id) {
                offset[0] = offset[0].clamp(0.0, metrics.max_offset[0]);
                offset[1] = offset[1].clamp(0.0, metrics.max_offset[1]);
            }
            self.scroll_metrics.insert(node.id.clone(), metrics);
        }
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

    fn scroll_chrome_instances(&self, visual: &UiVisual, node_path: &str) -> Vec<UiInstance> {
        let Some(metrics) = self.scroll_metrics.get(node_path) else {
            return Vec::new();
        };
        let offsets = self
            .scroll_offsets
            .get(node_path)
            .copied()
            .unwrap_or(visual.declared_scroll_offset);
        let clip = [
            visual.clip.x,
            visual.clip.y,
            visual.clip.x + visual.clip.width,
            visual.clip.y + visual.clip.height,
        ];
        [ScrollAxis::X, ScrollAxis::Y]
            .into_iter()
            .filter_map(|axis| {
                let track = scroll_track(*metrics, axis)?;
                let thumb = scroll_thumb(
                    track,
                    *metrics,
                    axis,
                    offsets[scroll_axis_index(axis)],
                    scroll_thumb_length(track, *metrics, axis),
                );
                Some([
                    UiInstance {
                        rect: [track.x, track.y, track.width, track.height],
                        fill: [0.10, 0.14, 0.14, 0.82],
                        border: [0.18, 0.29, 0.26, 0.82],
                        params: [0.0, 4.0, visual.style.opacity, visual.clip_radius],
                        clip,
                        depth: 0.0,
                    },
                    UiInstance {
                        rect: [thumb.x, thumb.y, thumb.width, thumb.height],
                        fill: [0.34, 0.80, 0.64, 0.95],
                        border: [0.34, 0.80, 0.64, 0.95],
                        params: [0.0, 4.0, visual.style.opacity, visual.clip_radius],
                        clip,
                        depth: 0.0,
                    },
                ])
            })
            .flatten()
            .collect()
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
        let mut bounds = visual.bounds;
        let pointer_over = self
            .pointer_position
            .is_some_and(|position| contains(bounds, position));
        if is_interactive_control(&visual.kind) && pointer_over {
            let factor = if time_seconds < self.pressed_until_seconds {
                1.28
            } else {
                1.14
            };
            fill[0] = (fill[0] * factor).min(1.0);
            fill[1] = (fill[1] * factor).min(1.0);
            fill[2] = (fill[2] * factor).min(1.0);
        }
        if visual.kind == UiNodeKind::Button
            && pointer_over
            && time_seconds < self.pressed_until_seconds
        {
            bounds.y += 1.0;
            bounds.height = (bounds.height - 1.0).max(0.0);
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
            depth: visual.world_depth.unwrap_or(0.0),
        }
    }

    fn component_chrome_instances(&self, visual: &UiVisual, node_path: &str) -> Vec<UiInstance> {
        let mut preview = visual.clone();
        if let Some(value) = self.value_previews.get(node_path) {
            preview.presentation = match (&preview.presentation, value) {
                (
                    Some(UiControlPresentation::Numeric { min, max, .. }),
                    UiSemanticPayloadValue::F32 { value },
                ) => Some(UiControlPresentation::Numeric {
                    value: *value,
                    min: *min,
                    max: *max,
                }),
                (
                    Some(UiControlPresentation::Numeric { min, max, .. }),
                    UiSemanticPayloadValue::I32 { value },
                ) => Some(UiControlPresentation::Numeric {
                    value: *value as f32,
                    min: *min,
                    max: *max,
                }),
                (
                    Some(UiControlPresentation::Scroll { .. }),
                    UiSemanticPayloadValue::F32 { value },
                ) => Some(UiControlPresentation::Scroll { position: *value }),
                _ => preview.presentation,
            };
        }
        let mut instances = component_chrome_instances(&preview);
        if preview.kind == UiNodeKind::Tabs
            && preview.enabled
            && let Some(pointer) = self.pointer_position
            && let Some(segment) =
                preview
                    .presentation
                    .as_ref()
                    .and_then(|presentation| match presentation {
                        UiControlPresentation::Choice { options, .. } => {
                            tab_segments(preview.bounds, options.len())
                                .into_iter()
                                .find(|segment| tag_contains(*segment, pointer))
                        }
                        _ => None,
                    })
        {
            instances.push(UiInstance {
                rect: [segment.x, segment.y, segment.width, segment.height],
                fill: [0.48, 0.76, 0.64, 0.13],
                border: [0.62, 0.92, 0.78, 0.80],
                params: [1.0, -4.0, preview.style.opacity, preview.clip_radius],
                clip: [
                    preview.clip.x,
                    preview.clip.y,
                    preview.clip.x + preview.clip.width,
                    preview.clip.y + preview.clip.height,
                ],
                depth: 0.0,
            });
        }
        instances
    }

    fn dropdown_popup_layout(&self) -> Option<(usize, Vec<UiBounds>)> {
        let node_path = self.open_dropdown.as_ref()?;
        let plan_index = self.plan.iter().position(|node| &node.id == node_path)?;
        let node = &self.plan[plan_index];
        let anchor = self.visual_at(plan_index).bounds;
        let UiControlPresentation::Choice { options, .. } = node.target.presentation.as_ref()?
        else {
            return None;
        };
        let row_height = 24.0;
        let margin = 4.0;
        let popup_height = options.len() as f32 * row_height;
        let viewport_height = self.viewport_logical_size[1].max(1.0);
        let y = if anchor.y + anchor.height + margin + popup_height <= viewport_height - margin {
            anchor.y + anchor.height + margin
        } else {
            (anchor.y - margin - popup_height).max(margin)
        };
        Some((
            plan_index,
            (0..options.len())
                .map(|index| UiBounds {
                    x: anchor.x,
                    y: y + index as f32 * row_height,
                    width: anchor.width,
                    height: row_height,
                })
                .collect(),
        ))
    }

    fn tooltip_hovered(&self, tooltip_index: usize) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(parent_id) = self.plan[tooltip_index].parent_id.as_ref() else {
            return false;
        };
        let Some(parent_index) = self.plan.iter().position(|node| &node.id == parent_id) else {
            return false;
        };
        contains(self.sampled[parent_index].bounds, pointer)
            && contains(self.sampled[parent_index].clip, pointer)
    }

    fn dropdown_popup_instances(&self) -> Vec<UiInstance> {
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else {
            return Vec::new();
        };
        let visual = &self.sampled[plan_index];
        let Some(UiControlPresentation::Choice { token, options, .. }) = &visual.presentation
        else {
            return Vec::new();
        };
        let clip = [
            0.0,
            0.0,
            self.viewport_logical_size[0].max(1.0),
            self.viewport_logical_size[1].max(1.0),
        ];
        let panel_y = rows
            .first()
            .map(|row| row.y - 2.0)
            .unwrap_or(visual.bounds.y);
        let panel_height = rows
            .last()
            .map(|row| row.y + row.height - panel_y + 2.0)
            .unwrap_or(0.0);
        let mut instances = vec![UiInstance {
            rect: [
                visual.bounds.x - 2.0,
                panel_y,
                visual.bounds.width + 4.0,
                panel_height,
            ],
            fill: [0.045, 0.075, 0.07, 0.99],
            border: [0.42, 0.68, 0.57, 0.96],
            params: [1.0, 4.0, 1.0, 0.0],
            clip,
            depth: 0.0,
        }];
        for (row, option) in rows.into_iter().zip(options) {
            instances.push(UiInstance {
                rect: [row.x, row.y, row.width, row.height],
                fill: if option == token {
                    [0.16, 0.35, 0.28, 1.0]
                } else {
                    [0.075, 0.12, 0.11, 1.0]
                },
                border: [0.22, 0.40, 0.34, 0.84],
                params: [1.0, 2.0, 1.0, 0.0],
                clip,
                depth: 0.0,
            });
        }
        instances
    }

    fn dropdown_option_texts(&self) -> Vec<(UiVisual, String)> {
        let Some((plan_index, rows)) = self.dropdown_popup_layout() else {
            return Vec::new();
        };
        let Some(UiControlPresentation::Choice { options, .. }) =
            self.sampled[plan_index].presentation.as_ref()
        else {
            return Vec::new();
        };
        options
            .iter()
            .zip(rows)
            .map(|(option, row)| {
                let mut visual = self.sampled[plan_index].clone();
                visual.kind = UiNodeKind::Label;
                visual.text = None;
                visual.bounds = row;
                visual.clip = UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: self.viewport_logical_size[0].max(1.0),
                    height: self.viewport_logical_size[1].max(1.0),
                };
                visual.clip_radius = 0.0;
                (visual, option.clone())
            })
            .collect()
    }

    fn list_box_option_texts(&self) -> Vec<(UiVisual, String)> {
        self.plan
            .iter()
            .enumerate()
            .flat_map(|(plan_index, node)| {
                if node.target.kind != UiNodeKind::ListBox {
                    return Vec::new();
                }
                let Some(UiControlPresentation::Choice { options, .. }) =
                    self.sampled[plan_index].presentation.as_ref()
                else {
                    return Vec::new();
                };
                list_box_rows(self.sampled[plan_index].bounds, options.len())
                    .into_iter()
                    .zip(options)
                    .map(|(row, option)| {
                        let mut visual = self.sampled[plan_index].clone();
                        visual.kind = UiNodeKind::Label;
                        visual.text = None;
                        visual.bounds = row;
                        (visual, option.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn tab_option_texts(&self) -> Vec<(UiVisual, String)> {
        self.plan
            .iter()
            .enumerate()
            .flat_map(|(plan_index, node)| {
                if node.target.kind != UiNodeKind::Tabs {
                    return Vec::new();
                }
                let Some(UiControlPresentation::Choice { options, .. }) =
                    self.sampled[plan_index].presentation.as_ref()
                else {
                    return Vec::new();
                };
                tab_segments(self.sampled[plan_index].bounds, options.len())
                    .into_iter()
                    .zip(options)
                    .map(|(segment, option)| {
                        let mut visual = self.sampled[plan_index].clone();
                        visual.kind = UiNodeKind::Label;
                        visual.text = None;
                        visual.bounds = segment;
                        (visual, option.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn drag_value_texts(&self) -> Vec<(UiVisual, String)> {
        self.plan
            .iter()
            .enumerate()
            .filter_map(|(plan_index, node)| {
                if node.target.kind != UiNodeKind::DragValue {
                    return None;
                }
                let Some(UiControlPresentation::Numeric { value, min: _, max }) =
                    self.sampled[plan_index].presentation.as_ref()
                else {
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
                Some((
                    visual,
                    format!("{} / {}", value.round() as i32, max.round() as i32),
                ))
            })
            .collect()
    }
}

fn data_grid_requested_range(
    frame: &neon_ui_schema::UiDataGridFrame,
    declaration: &neon_ui_schema::UiDataGridDeclaration,
    offset_y: f32,
    viewport_height: f32,
) -> Option<(u64, u64)> {
    let row_height = declaration.row_height as f32;
    if row_height <= 0.0
        || frame.total_rows == 0
        || !offset_y.is_finite()
        || !viewport_height.is_finite()
    {
        return None;
    }
    // Row zero begins below the grid header, so convert from scroll content space
    // to a data-row index only after accounting for that leading header row.
    let first_visible = ((offset_y - row_height).max(0.0) / row_height).floor() as u64;
    let requested_first_row = first_visible.saturating_sub(u64::from(declaration.overscan));
    let viewport_rows = (viewport_height.max(0.0) / row_height).ceil().max(1.0) as u64;
    let required_rows = (viewport_rows + u64::from(declaration.overscan) * 2)
        .min(u64::from(declaration.max_window_rows));
    let final_window_first_row = frame.total_rows.saturating_sub(required_rows);
    let requested_first_row = if first_visible.saturating_add(viewport_rows) >= frame.total_rows {
        final_window_first_row
    } else {
        requested_first_row.min(final_window_first_row)
    };
    Some((requested_first_row, required_rows))
}

fn data_grid_frame_covers_offset(
    frame: &neon_ui_schema::UiDataGridFrame,
    declaration: &neon_ui_schema::UiDataGridDeclaration,
    offset_y: f32,
    viewport_height: f32,
) -> bool {
    let Some((requested_first_row, required_rows)) =
        data_grid_requested_range(frame, declaration, offset_y, viewport_height)
    else {
        return false;
    };
    let requested_end = requested_first_row
        .saturating_add(required_rows)
        .min(frame.total_rows);
    let frame_end = frame
        .first_row
        .saturating_add(frame.window_rows.len() as u64)
        .min(frame.total_rows);
    requested_first_row >= frame.first_row && requested_end <= frame_end
}

fn list_box_rows(bounds: UiBounds, option_count: usize) -> Vec<UiBounds> {
    if option_count == 0 {
        return Vec::new();
    }
    let inset = 6.0;
    let row_height = ((bounds.height - inset * 2.0) / option_count as f32).max(18.0);
    (0..option_count)
        .map(|index| UiBounds {
            x: bounds.x + inset,
            y: bounds.y + inset + index as f32 * row_height,
            width: (bounds.width - inset * 2.0).max(0.0),
            height: row_height,
        })
        .collect()
}

fn tab_segments(bounds: UiBounds, option_count: usize) -> Vec<UiBounds> {
    if option_count == 0 {
        return Vec::new();
    }
    let inset = 2.0;
    let gap = 0.0;
    let width = ((bounds.width - inset * 2.0 - gap * option_count.saturating_sub(1) as f32)
        .max(0.0))
        / option_count as f32;
    (0..option_count)
        .map(|index| UiBounds {
            x: bounds.x + inset + index as f32 * (width + gap),
            y: bounds.y + inset,
            width,
            height: (bounds.height - inset * 2.0).max(0.0),
        })
        .collect()
}

fn tag_contains(bounds: UiBounds, point: [f32; 2]) -> bool {
    if point[1] < bounds.y || point[1] > bounds.y + bounds.height || bounds.height <= 0.0 {
        return false;
    }
    let cut = 4.0_f32.min(bounds.width * 0.25);
    let local_y = (point[1] - bounds.y) / bounds.height;
    let left = bounds.x + cut * (1.0 - local_y);
    let right = bounds.x + bounds.width - cut * local_y;
    point[0] >= left && point[0] <= right
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

fn numeric_fraction(value: f32, minimum: f32, maximum: f32) -> f32 {
    if maximum > minimum {
        ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0)
    } else {
        0.0
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
        UiNodeKind::Button => UiStyle {
            background_color: [0.12, 0.32, 0.31, 1.0],
            border_color: [0.43, 0.78, 0.73, 1.0],
            border_width: 1.0,
            corner_radius: 4.0,
            opacity: 1.0,
        },
        UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar => UiStyle {
            background_color: [0.09, 0.12, 0.12, 1.0],
            border_color: [0.27, 0.46, 0.40, 0.82],
            border_width: 1.0,
            corner_radius: 5.0,
            opacity: 1.0,
        },
        UiNodeKind::Tabs => UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
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
        depth: 0.0,
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
        Some(UiControlPresentation::Choice { token, .. }) if token == "alpha" => {
            [0.86, 0.59, 0.33, 0.95]
        }
        Some(UiControlPresentation::Choice { token, .. }) if token == "gamma" => {
            [0.48, 0.66, 0.95, 0.95]
        }
        _ => mint,
    };
    match visual.kind {
        UiNodeKind::Button => vec![
            chrome(
                UiBounds {
                    x: bounds.x + 1.0,
                    y: bounds.y + bounds.height - 4.0,
                    width: (bounds.width - 2.0).max(0.0),
                    height: 3.0,
                },
                [0.035, 0.11, 0.11, 0.95],
                [0.035, 0.11, 0.11, 0.95],
                2.0,
            ),
            chrome(
                UiBounds {
                    x: bounds.x + 2.0,
                    y: bounds.y + 2.0,
                    width: (bounds.width - 4.0).max(0.0),
                    height: 1.0,
                },
                [0.68, 0.95, 0.88, 0.56],
                [0.68, 0.95, 0.88, 0.56],
                1.0,
            ),
        ],
        UiNodeKind::Checkbox => vec![chrome(
            UiBounds {
                x: bounds.x + 8.0,
                y: center_y - 7.0,
                width: 14.0,
                height: 14.0,
            },
            if selected { mint } else { inactive },
            if selected {
                mint
            } else {
                [0.53, 0.55, 0.60, 1.0]
            },
            3.0,
        )],
        UiNodeKind::RadioButton => vec![chrome(
            UiBounds {
                x: bounds.x + 8.0,
                y: center_y - 7.0,
                width: 14.0,
                height: 14.0,
            },
            if selected { mint } else { inactive },
            if selected {
                mint
            } else {
                [0.53, 0.55, 0.60, 1.0]
            },
            7.0,
        )],
        UiNodeKind::Selectable => vec![chrome(
            UiBounds {
                x: bounds.x + 5.0,
                y: bounds.y + 5.0,
                width: 3.0,
                height: (bounds.height - 10.0).max(0.0),
            },
            if selected { mint } else { muted },
            if selected {
                mint
            } else {
                [0.34, 0.54, 0.47, 0.82]
            },
            1.5,
        )],
        UiNodeKind::Slider => {
            let track = UiBounds {
                x: bounds.x + bounds.width * 0.57,
                y: center_y - 2.0,
                width: bounds.width * 0.34,
                height: 4.0,
            };
            vec![
                chrome(track, muted, muted, 2.0),
                chrome(
                    UiBounds {
                        x: track.x + track.width * normalized - 5.0,
                        y: center_y - 6.0,
                        width: 12.0,
                        height: 12.0,
                    },
                    mint,
                    mint,
                    6.0,
                ),
            ]
        }
        UiNodeKind::DragValue => {
            let well = drag_value_bounds(bounds);
            let progress = UiBounds {
                x: well.x + 1.0,
                y: well.y + 1.0,
                width: ((well.width - 2.0) * normalized).max(0.0),
                height: (well.height - 2.0).max(0.0),
            };
            vec![
                chrome(well, muted, choice_tint, 4.0),
                UiInstance {
                    rect: [progress.x, progress.y, progress.width, progress.height],
                    fill: [0.18, 0.52, 0.90, 0.92],
                    border: [0.18, 0.52, 0.90, 0.92],
                    params: [0.0, 3.0, visual.style.opacity, visual.clip_radius],
                    clip,
                    depth: 0.0,
                },
            ]
        }
        UiNodeKind::TextInput => vec![chrome(
            UiBounds {
                x: bounds.x + 2.0,
                y: bounds.y + 2.0,
                width: (bounds.width - 4.0).max(0.0),
                height: (bounds.height - 4.0).max(0.0),
            },
            [0.08, 0.12, 0.15, 0.55],
            [0.22, 0.52, 0.50, 0.9],
            3.0,
        )],
        UiNodeKind::Combo | UiNodeKind::Dropdown => vec![chrome(
            UiBounds {
                x: bounds.x + bounds.width - 27.0,
                y: center_y - 5.0,
                width: 16.0,
                height: 10.0,
            },
            muted,
            choice_tint,
            3.0,
        )],
        UiNodeKind::ListBox => match &visual.presentation {
            Some(UiControlPresentation::Choice { token, options, .. }) => {
                list_box_rows(bounds, options.len())
                    .into_iter()
                    .zip(options)
                    .map(|(row, option)| {
                        let active = option == token;
                        chrome(
                            row,
                            if active {
                                [0.16, 0.35, 0.28, 1.0]
                            } else {
                                [0.075, 0.12, 0.11, 1.0]
                            },
                            if active { choice_tint } else { muted },
                            3.0,
                        )
                    })
                    .collect()
            }
            _ => Vec::new(),
        },
        UiNodeKind::Tabs => match &visual.presentation {
            Some(UiControlPresentation::Choice { token, options, .. }) => {
                let segments = tab_segments(bounds, options.len());
                let mut tags = segments
                    .iter()
                    .zip(options)
                    .map(|(segment, option)| {
                        let active = option == token;
                        chrome(
                            *segment,
                            if !visual.enabled {
                                [0.055, 0.075, 0.07, 0.72]
                            } else if active {
                                [0.16, 0.35, 0.28, 1.0]
                            } else {
                                [0.075, 0.12, 0.11, 1.0]
                            },
                            if !visual.enabled {
                                inactive
                            } else if active {
                                choice_tint
                            } else {
                                muted
                            },
                            -4.0,
                        )
                    })
                    .collect::<Vec<_>>();
                tags.extend(segments.windows(2).map(|pair| {
                    let boundary = pair[0].x + pair[0].width;
                    chrome(
                        UiBounds {
                            x: boundary - 3.0,
                            y: bounds.y + 7.0,
                            width: 6.0,
                            height: (bounds.height - 14.0).max(0.0),
                        },
                        [0.62, 0.94, 0.78, 0.9],
                        [0.72, 1.0, 0.84, 1.0],
                        -1.5,
                    )
                }));
                tags
            }
            _ => Vec::new(),
        },
        UiNodeKind::Scrollbar => {
            let track = UiBounds {
                x: bounds.x + 10.0,
                y: center_y - 3.0,
                width: (bounds.width - 20.0).max(0.0),
                height: 6.0,
            };
            vec![
                chrome(track, muted, muted, 3.0),
                chrome(
                    UiBounds {
                        x: track.x + (track.width - track.width * 0.28) * normalized,
                        y: center_y - 5.0,
                        width: track.width * 0.28,
                        height: 10.0,
                    },
                    mint,
                    mint,
                    5.0,
                ),
            ]
        }
        UiNodeKind::Image => vec![chrome(
            UiBounds {
                x: bounds.x + 1.0,
                y: bounds.y + 1.0,
                width: (bounds.width - 2.0).max(0.0),
                height: (bounds.height - 2.0).max(0.0),
            },
            [0.12, 0.28, 0.31, 0.92],
            [0.46, 0.72, 0.76, 0.95],
            3.0,
        )],
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
    render_renderer_with_viewport_offscreen_for_test(
        renderer,
        device,
        queue,
        format,
        fragments,
        size,
        [size[0] as f32, size[1] as f32],
        time_seconds,
    )
}

#[cfg(test)]
pub(crate) fn render_renderer_with_viewport_offscreen_for_test(
    renderer: &mut UiWgpuRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    size: [u32; 2],
    logical_viewport: [f32; 2],
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
        renderer.draw(
            device,
            queue,
            &mut pass,
            fragments,
            size,
            logical_viewport,
            time_seconds,
        );
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
    render_hit_ids_with_renderer_for_test(&mut renderer, device, queue, fragments, size)
}

#[cfg(test)]
fn render_hit_ids_with_renderer_for_test(
    renderer: &mut UiWgpuRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    size: [u32; 2],
) -> Vec<u32> {
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
    let row_bytes = size[0] * 4;
    let padded_bytes_per_row =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &vec![0xff; (row_bytes * size[1]) as usize],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_bytes),
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
        size: (padded_bytes_per_row * size[1]) as u64,
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
        renderer.draw_hit_id(
            device,
            queue,
            &mut pass,
            fragments,
            size,
            [size[0] as f32, size[1] as f32],
            1.0,
        );
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
    let bytes = readback.slice(..).get_mapped_range().unwrap();
    let mut hits = Vec::with_capacity((size[0] * size[1]) as usize);
    for row in bytes.chunks_exact(padded_bytes_per_row as usize) {
        hits.extend(
            row[..row_bytes as usize]
                .chunks_exact(4)
                .map(|pixel| u32::from_ne_bytes(pixel.try_into().unwrap())),
        );
    }
    hits
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
    let text_scale = if visual.bounds.height > 0.0 && visual.bounds.height <= 40.0 {
        (visual.bounds.height / 28.0).clamp(0.5, 2.0)
    } else {
        1.0
    };
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
        if !line.is_empty() && line_width + glyph.advance * text_scale > visual.bounds.width {
            lines.push(std::mem::take(&mut line));
            line_width = 0.0;
        }
        line_width += glyph.advance * text_scale;
        line.push(glyph);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }

    let block_height = font.line_height * text_scale * lines.len() as f32;
    let top = visual.bounds.y + ((visual.bounds.height - block_height).max(0.0) * 0.5);
    let mut result = Vec::new();
    for (line_index, glyphs) in lines.into_iter().enumerate() {
        let advance = glyphs.iter().map(|glyph| glyph.advance * text_scale).sum::<f32>();
        let mut x = if visual.kind == UiNodeKind::Button {
            visual.bounds.x + ((visual.bounds.width - advance).max(0.0) * 0.5)
        } else {
            visual.bounds.x
                + if visual.kind == UiNodeKind::TextInput {
                    TEXT_INPUT_INSET * text_scale - horizontal_scroll.unwrap_or(0.0)
                } else if matches!(
                    visual.kind,
                    UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
                ) {
                    30.0 * text_scale
                } else {
                    10.0 * text_scale
                }
        };
        let baseline = top + font.ascent * text_scale
            + line_index as f32 * font.line_height * text_scale;
        for glyph in glyphs {
            result.push(UiTextInstance {
                rect: [
                    x + glyph.xmin * text_scale,
                    baseline + glyph.plane_min_y * text_scale,
                    glyph.width * text_scale,
                    glyph.height * text_scale,
                ],
                color: [0.86, 0.95, 0.98, visual.style.opacity],
                clip,
                uv: glyph.uv,
                depth: visual.world_depth.unwrap_or(0.0),
            });
            x += glyph.advance * text_scale;
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
        depth: 0.0,
    }
}

fn top_layer_roots(plan: &[PlannedNode], indices: &HashMap<&str, usize>) -> Vec<Option<usize>> {
    let mut roots = vec![None; plan.len()];
    for (index, node) in plan.iter().enumerate() {
        roots[index] = if matches!(
            node.target.kind,
            UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog
        ) {
            Some(index)
        } else {
            node.parent_id
                .as_deref()
                .and_then(|parent| indices.get(parent).copied())
                .and_then(|parent| roots[parent])
        };
    }
    roots
}

#[cfg(test)]
fn flatten_fragments(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    viewport_logical_size: [f32; 2],
    font: Option<&ResidentFont>,
) -> Vec<(String, Option<String>, UiVisual, Option<UiTransition>)> {
    flatten_fragments_with_data_grid_display_cache(
        fragments,
        viewport_logical_size,
        font,
        &HashMap::new(),
        &HashSet::new(),
    )
}

fn flatten_fragments_with_data_grid_display_cache(
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    viewport_logical_size: [f32; 2],
    font: Option<&ResidentFont>,
    data_grid_text_display_cache: &HashMap<DataGridCellIdentity, CachedDataGridTextDisplay>,
    available_cameras: &HashSet<(neon_world_bridge::CameraId, neon_world_bridge::CameraKind)>,
) -> Vec<(String, Option<String>, UiVisual, Option<UiTransition>)> {
    let presentations = collect_control_presentations(fragments);
    let mut ordered = fragments.values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.fragment_id.0.cmp(&right.fragment_id.0));
    let mut result = Vec::new();
    for fragment in ordered {
        let hidden_world_nodes = fragment
            .effects
            .iter()
            .filter_map(|effect| match effect {
                neon_ui_schema::UiEffect::CameraVisibility { binding }
                    if !available_cameras
                        .contains(&(binding.camera_id.clone(), binding.camera_kind)) =>
                {
                    Some(binding.node_id.0.as_str())
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut root = fragment.root.clone();
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: viewport_logical_size[0],
            height: viewport_logical_size[1],
        };
        flatten_node(
            &mut result,
            &fragment.fragment_id.0,
            &root,
            [0.0, 0.0],
            None,
            None,
            None,
            font,
            Some(viewport_logical_size),
            false,
            &hidden_world_nodes,
            None,
        );
    }
    append_data_grid_frames(&mut result, fragments, data_grid_text_display_cache);
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
        let Some(UiControlPresentation::Choice { token, options, .. }) = &visual.presentation
        else {
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

/// DataGrid frames carry only the current virtual window. Expand that window
/// into ordinary renderer visuals so it follows the same composition path as
/// declared UI without materializing domain rows outside the frame.
const DATA_GRID_SCROLLBAR_GUTTER: f32 = 12.0;

fn data_grid_effective_columns(
    declaration: &neon_ui_schema::UiDataGridDeclaration,
    grid: UiBounds,
    total_rows: u64,
    row_height: f32,
) -> (Vec<f32>, f32, bool, bool) {
    let basis = declaration
        .columns
        .iter()
        .map(|column| column.width as f32)
        .sum::<f32>();
    let content_height = row_height * (total_rows as f32 + 1.0);
    let mut vertical_scrollbar = false;
    let mut horizontal_scrollbar = false;
    // Horizontal and vertical gutters can make each other necessary at the
    // boundary, so resolve the two scrollbar decisions to a fixed point.
    for _ in 0..3 {
        let viewport_width = (grid.width
            - if vertical_scrollbar {
                DATA_GRID_SCROLLBAR_GUTTER
            } else {
                0.0
            })
        .max(0.0);
        horizontal_scrollbar = basis > viewport_width;
        vertical_scrollbar = content_height
            > (grid.height
                - if horizontal_scrollbar {
                    DATA_GRID_SCROLLBAR_GUTTER
                } else {
                    0.0
                })
            .max(0.0);
    }
    let viewport_width = (grid.width
        - if vertical_scrollbar {
            DATA_GRID_SCROLLBAR_GUTTER
        } else {
            0.0
        })
    .max(0.0);
    let scale = if !horizontal_scrollbar && basis > 0.0 {
        viewport_width / basis
    } else {
        1.0
    };
    let widths = declaration
        .columns
        .iter()
        .map(|column| column.width as f32 * scale)
        .collect::<Vec<_>>();
    let content_width = widths.iter().sum();
    (
        widths,
        content_width,
        horizontal_scrollbar,
        vertical_scrollbar,
    )
}

fn append_data_grid_frames(
    out: &mut Vec<(String, Option<String>, UiVisual, Option<UiTransition>)>,
    fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    data_grid_text_display_cache: &HashMap<DataGridCellIdentity, CachedDataGridTextDisplay>,
) {
    for fragment in fragments.values() {
        for effect in &fragment.effects {
            let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                continue;
            };
            let grid_path = format!("{}/{}", fragment.fragment_id.0, declaration.node_key);
            let Some((_, _, grid, _)) = out
                .iter()
                .find(|(path, _, _, _)| path == &grid_path)
                .cloned()
            else {
                continue;
            };
            if grid.kind != UiNodeKind::DataGrid {
                continue;
            }
            // A virtual grid is itself a scroll viewport. Its bounded window remains
            // small while overflow metrics are derived from the logical row/column extent.
            if let Some((_, _, grid_target, _)) =
                out.iter_mut().find(|(path, _, _, _)| path == &grid_path)
            {
                grid_target.scroll = true;
            }

            let row_height = declaration.row_height as f32;
            if row_height <= 0.0 {
                continue;
            }
            let (column_widths, content_width, horizontal_scrollbar, vertical_scrollbar) =
                data_grid_effective_columns(declaration, grid.bounds, frame.total_rows, row_height);
            let horizontal_scrollbar_band = if horizontal_scrollbar {
                DATA_GRID_SCROLLBAR_GUTTER
            } else {
                0.0
            };
            let viewport_width = (grid.bounds.width
                - if vertical_scrollbar {
                    DATA_GRID_SCROLLBAR_GUTTER
                } else {
                    0.0
                })
            .max(0.0);
            let body_clip = UiBounds {
                x: grid.bounds.x,
                y: grid.bounds.y + row_height,
                width: viewport_width,
                height: (grid.bounds.height - row_height - horizontal_scrollbar_band).max(0.0),
            };
            let mut extent = grid.clone();
            extent.kind = UiNodeKind::Image;
            extent.bounds = UiBounds {
                x: grid.bounds.x,
                y: grid.bounds.y,
                width: content_width,
                height: row_height * (frame.total_rows as f32 + 1.0),
            };
            extent.image = None;
            out.push((
                format!("{grid_path}/data-grid-content-extent"),
                Some(grid_path.clone()),
                extent,
                None,
            ));
            let header = UiVisual {
                bounds: UiBounds {
                    x: grid.bounds.x,
                    y: grid.bounds.y,
                    width: content_width,
                    height: row_height,
                },
                style: UiStyle {
                    background_color: [0.10, 0.16, 0.19, 1.0],
                    border_color: [0.30, 0.48, 0.52, 1.0],
                    border_width: 1.0,
                    corner_radius: 0.0,
                    opacity: 1.0,
                },
                kind: UiNodeKind::Panel,
                enabled: false,
                clip: UiBounds {
                    x: grid.bounds.x,
                    y: grid.bounds.y,
                    width: content_width,
                    height: row_height,
                },
                clip_radius: 0.0,
                image: None,
                surface: None,
                text: None,
                presentation: None,
                scroll: false,
                declared_scroll_offset: [0.0; 2],
                world_depth: None,
            };
            let mut sticky_header = vec![(
                format!("{grid_path}/data-grid-header"),
                Some(grid_path.clone()),
                header,
                None,
            )];

            let mut x = grid.bounds.x;
            for (column_index, column) in declaration.columns.iter().enumerate() {
                let width = column_widths[column_index];
                let mut label = grid.clone();
                label.bounds = UiBounds {
                    x: x + 5.0,
                    y: grid.bounds.y,
                    width: (width - 10.0).max(0.0),
                    height: row_height,
                };
                label.style = UiStyle::default();
                label.kind = UiNodeKind::Label;
                label.enabled = false;
                label.clip = label.bounds;
                label.clip_radius = 0.0;
                label.image = None;
                label.surface = None;
                label.text = Some(TextRef::Literal {
                    value: column.label.clone(),
                });
                label.presentation = None;
                sticky_header.push((
                    format!("{grid_path}/data-grid-header-{column_index}"),
                    Some(grid_path.clone()),
                    label,
                    None,
                ));
                x += width;
            }

            for (row_index, row) in frame
                .window_rows
                .iter()
                .take(declaration.max_window_rows as usize)
                .enumerate()
            {
                let logical_row = frame.first_row.saturating_add(row_index as u64);
                let y = grid.bounds.y + row_height * (logical_row as f32 + 1.0);
                let row_visual = UiVisual {
                    bounds: UiBounds {
                        x: grid.bounds.x,
                        y,
                        width: content_width,
                        height: row_height,
                    },
                    style: UiStyle {
                        background_color: if row_index % 2 == 0 {
                            [0.055, 0.085, 0.10, 1.0]
                        } else {
                            [0.070, 0.105, 0.12, 1.0]
                        },
                        border_color: [0.18, 0.29, 0.32, 1.0],
                        border_width: 1.0,
                        corner_radius: 0.0,
                        opacity: 1.0,
                    },
                    kind: UiNodeKind::Panel,
                    enabled: false,
                    clip: body_clip,
                    clip_radius: 0.0,
                    image: None,
                    surface: None,
                    text: None,
                    presentation: None,
                    scroll: false,
                    declared_scroll_offset: [0.0; 2],
                    world_depth: None,
                };
                let row_path = format!("{grid_path}/data-grid-row-{}", row.stable_row_key);
                out.push((row_path.clone(), Some(grid_path.clone()), row_visual, None));

                let mut x = grid.bounds.x;
                for (column_index, column) in declaration.columns.iter().enumerate() {
                    let width = column_widths[column_index];
                    if let Some(cell) = row.cells.get(&column.key) {
                        let mut label = grid.clone();
                        label.bounds = UiBounds {
                            x: x + 5.0,
                            y,
                            width: (width - 10.0).max(0.0),
                            height: row_height,
                        };
                        label.style = UiStyle::default();
                        let presentation = cell
                            .presentation_override
                            .as_ref()
                            .map(data_grid_cell_presentation)
                            .unwrap_or_else(|| data_grid_column_presentation(&column.presentation));
                        if matches!(
                            column.presentation,
                            neon_ui_schema::UiDataGridPresentation::Select { .. }
                        ) && cell.presentation_override.is_none()
                            && let neon_ui_schema::UiInputValue::Bool { value } = cell.value
                        {
                            label.style = if value {
                                UiStyle {
                                    background_color: [0.10, 0.25, 0.20, 1.0],
                                    border_color: [0.34, 0.80, 0.64, 0.95],
                                    border_width: 1.0,
                                    corner_radius: 4.0,
                                    opacity: 1.0,
                                }
                            } else {
                                UiStyle {
                                    background_color: [0.30, 0.10, 0.12, 1.0],
                                    border_color: [0.92, 0.34, 0.38, 0.96],
                                    border_width: 1.0,
                                    corner_radius: 4.0,
                                    opacity: 1.0,
                                }
                            };
                        }
                        label.enabled = presentation.0 != UiNodeKind::Label;
                        label.kind = presentation.0;
                        label.clip = body_clip;
                        label.clip_radius = 0.0;
                        label.image = None;
                        label.surface = None;
                        label.text = Some(TextRef::Literal {
                            value: data_grid_cell_display_text(
                                cell,
                                data_grid_text_display_cache.get(&DataGridCellIdentity {
                                    source_key: declaration.source_key.clone(),
                                    stable_row_key: row.stable_row_key.clone(),
                                    column_key: column.key.clone(),
                                }),
                            ),
                        });
                        label.presentation = presentation.1;
                        // Stable renderer-local target metadata; it is never serialized.
                        out.push((
                            format!("{row_path}/cell-{}", column.key),
                            Some(row_path.clone()),
                            label,
                            None,
                        ));
                    }
                    x += width;
                }
            }
            out.extend(sticky_header);
        }
    }
}

fn data_grid_cell_display_text(
    cell: &neon_ui_schema::UiDataGridCell,
    cached: Option<&CachedDataGridTextDisplay>,
) -> String {
    match &cell.value {
        neon_ui_schema::UiInputValue::Bool { value } => value.to_string(),
        neon_ui_schema::UiInputValue::I32 { value } => value.to_string(),
        neon_ui_schema::UiInputValue::U32 { value } => value.to_string(),
        neon_ui_schema::UiInputValue::F32 { value } => format!("{value:.3}"),
        neon_ui_schema::UiInputValue::Enum { value } => value.clone(),
        neon_ui_schema::UiInputValue::TextHandle { .. } => cached
            .map(|cached| cached.text.clone())
            .unwrap_or_else(|| format!("text#{}:{}", cell.display.id, cell.display.generation)),
        neon_ui_schema::UiInputValue::Vec2 { value } => format!("{}, {}", value[0], value[1]),
        neon_ui_schema::UiInputValue::Vec4 { value }
        | neon_ui_schema::UiInputValue::Color { value } => {
            format!("{}, {}, {}, {}", value[0], value[1], value[2], value[3])
        }
        neon_ui_schema::UiInputValue::AssetHandle { id, generation } => {
            format!("asset#{id}:{generation}")
        }
    }
}

fn data_grid_cell_identity(binding: &UiHitBinding) -> Option<DataGridCellIdentity> {
    let cell = binding.data_grid_cell.as_ref()?;
    Some(DataGridCellIdentity {
        source_key: cell.source_key.clone(),
        stable_row_key: cell.stable_row_key.clone(),
        column_key: cell.column_key.clone(),
    })
}

fn data_grid_column_presentation(
    presentation: &neon_ui_schema::UiDataGridPresentation,
) -> (UiNodeKind, Option<UiControlPresentation>) {
    match presentation {
        neon_ui_schema::UiDataGridPresentation::Text => (UiNodeKind::Label, None),
        neon_ui_schema::UiDataGridPresentation::Select { .. } => (
            UiNodeKind::Combo,
            Some(UiControlPresentation::Choice {
                token: String::new(),
                options: Vec::new(),
                selected: false,
            }),
        ),
        neon_ui_schema::UiDataGridPresentation::Dropdown { options, .. } => (
            UiNodeKind::Dropdown,
            Some(UiControlPresentation::Choice {
                token: String::new(),
                options: options.clone(),
                selected: false,
            }),
        ),
        neon_ui_schema::UiDataGridPresentation::Edit { .. } => (UiNodeKind::TextInput, None),
    }
}

fn data_grid_cell_presentation(
    presentation: &neon_ui_schema::UiDataGridCellPresentation,
) -> (UiNodeKind, Option<UiControlPresentation>) {
    match presentation {
        neon_ui_schema::UiDataGridCellPresentation::Text => (UiNodeKind::Label, None),
        neon_ui_schema::UiDataGridCellPresentation::Dropdown { options } => (
            UiNodeKind::Dropdown,
            Some(UiControlPresentation::Choice {
                token: String::new(),
                options: options.clone(),
                selected: false,
            }),
        ),
        neon_ui_schema::UiDataGridCellPresentation::Edit { .. } => (UiNodeKind::TextInput, None),
    }
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
                        data_grid_cell: None,
                        control_value: None,
                        max_text_length: None,
                    },
                );
            }
        }
        for effect in &fragment.effects {
            let neon_ui_schema::UiEffect::DataGridFrame { declaration, frame } = effect else {
                continue;
            };
            for row in frame
                .window_rows
                .iter()
                .take(declaration.max_window_rows as usize)
            {
                for column in &declaration.columns {
                    let Some(cell) = row.cells.get(&column.key) else {
                        continue;
                    };
                    let presentation = cell
                        .presentation_override
                        .as_ref()
                        .map(data_grid_cell_presentation)
                        .unwrap_or_else(|| data_grid_column_presentation(&column.presentation));
                    let (intent, control_value, max_text_length) = match (
                        &column.presentation,
                        cell.presentation_override.as_ref(),
                        &cell.value,
                    ) {
                        (
                            neon_ui_schema::UiDataGridPresentation::Select { intent },
                            None,
                            neon_ui_schema::UiInputValue::Bool { value },
                        ) => (
                            intent,
                            Some(UiSemanticPayloadValue::Bool { value: !value }),
                            None,
                        ),
                        (
                            neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }
                            | neon_ui_schema::UiDataGridPresentation::Select { intent }
                            | neon_ui_schema::UiDataGridPresentation::Edit { intent, .. },
                            Some(neon_ui_schema::UiDataGridCellPresentation::Dropdown { .. })
                            | None,
                            _,
                        ) if presentation.0 == UiNodeKind::Dropdown => (intent, None, None),
                        (
                            neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }
                            | neon_ui_schema::UiDataGridPresentation::Select { intent }
                            | neon_ui_schema::UiDataGridPresentation::Edit { intent, .. },
                            Some(neon_ui_schema::UiDataGridCellPresentation::Edit { .. }) | None,
                            neon_ui_schema::UiInputValue::TextHandle { value },
                        ) if presentation.0 == UiNodeKind::TextInput => (
                            intent,
                            Some(UiSemanticPayloadValue::TextHandle { value: *value }),
                            match cell.presentation_override.as_ref() {
                                Some(neon_ui_schema::UiDataGridCellPresentation::Edit {
                                    max_chars,
                                }) => Some(*max_chars),
                                _ => match &column.presentation {
                                    neon_ui_schema::UiDataGridPresentation::Edit {
                                        max_chars,
                                        ..
                                    } => Some(*max_chars),
                                    _ => None,
                                },
                            },
                        ),
                        _ => continue,
                    };
                    let node_path = format!(
                        "{}/{}{}",
                        fragment.fragment_id.0,
                        declaration.node_key,
                        format!("/data-grid-row-{}/cell-{}", row.stable_row_key, column.key),
                    );
                    declarations.insert(
                        node_path.clone(),
                        UiHitBinding {
                            node_path,
                            fragment: UiFragmentRevision {
                                id: fragment.fragment_id.clone(),
                                revision: fragment.revision,
                            },
                            intent: Some(UiIntent::Invoke {
                                action: intent.clone(),
                                params: Value::Object(Default::default()),
                            }),
                            text_input: None,
                            data_grid_cell: Some(UiDataGridCellTarget {
                                source_key: declaration.source_key.clone(),
                                stable_row_key: row.stable_row_key.clone(),
                                column_key: column.key.clone(),
                            }),
                            control_value,
                            max_text_length,
                        },
                    );
                }
            }
        }
    }
    declarations
}

fn is_data_grid_cell_path(node_path: &str) -> bool {
    node_path.contains("/data-grid-row-") && node_path.contains("/cell-")
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
    hidden_world_nodes: &HashSet<&str>,
    inherited_depth: Option<f32>,
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
    let top_layer = inherited_top_layer
        || matches!(
            node.kind,
            UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog
        );
    let own_clip = if top_layer {
        None
    } else {
        match node_layout.clip {
            UiClipPolicy::None => inherited_clip,
            UiClipPolicy::Bounds | UiClipPolicy::Rounded | UiClipPolicy::Scroll => {
                Some(intersect_clip(inherited_clip, bounds))
            }
        }
    };
    let own_clip_radius = if top_layer {
        None
    } else {
        match node_layout.clip {
            UiClipPolicy::Rounded => Some(node.style.corner_radius),
            UiClipPolicy::None => inherited_clip_radius,
            UiClipPolicy::Bounds | UiClipPolicy::Scroll => None,
        }
    };
    let effective_clip = own_clip.unwrap_or(UiBounds {
        x: -1_000_000.0,
        y: -1_000_000.0,
        width: 2_000_000.0,
        height: 2_000_000.0,
    });
    if !node.visible || hidden_world_nodes.contains(node.node_id.0.as_str()) {
        return;
    }
    if node.style.opacity > 0.0 {
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
                scroll: node_layout.clip == UiClipPolicy::Scroll,
                declared_scroll_offset: node_layout.scroll_offset,
                world_depth: node.world_depth.or(inherited_depth),
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
    let child_inherited_clip = if node_layout.clip == UiClipPolicy::Scroll {
        None
    } else {
        own_clip
    };
    let child_inherited_clip_radius = if node_layout.clip == UiClipPolicy::Scroll {
        None
    } else {
        own_clip_radius
    };
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
            child_inherited_clip,
            child_inherited_clip_radius,
            Some(&node_path),
            font,
            Some([child_bounds.width, child_bounds.height]),
            top_layer,
            hidden_world_nodes,
            node.world_depth.or(inherited_depth),
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

fn dimension_limits(layout: &UiLayout, height: bool) -> (f32, f32) {
    let minimum = layout
        .min_size
        .map_or(0.0, |size| if height { size[1] } else { size[0] });
    let maximum = layout
        .max_size
        .map_or(f32::INFINITY, |size| if height { size[1] } else { size[0] })
        .max(minimum);
    (minimum, maximum)
}

fn clamp_dimension(value: f32, layout: &UiLayout, height: bool) -> f32 {
    let (minimum, maximum) = dimension_limits(layout, height);
    value.clamp(minimum, maximum)
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
    matches!(
        kind,
        UiNodeKind::Button
            | UiNodeKind::TextInput
            | UiNodeKind::Checkbox
            | UiNodeKind::RadioButton
            | UiNodeKind::Slider
            | UiNodeKind::DragValue
            | UiNodeKind::Combo
            | UiNodeKind::Dropdown
            | UiNodeKind::Tabs
            | UiNodeKind::Selectable
            | UiNodeKind::ListBox
            | UiNodeKind::Scrollbar
    )
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
            .map(|child| {
                let layout = child.layout.unwrap_or_default();
                let mut width = resolved_dimension(child.bounds.width, child, &layout, font, false);
                let mut height =
                    resolved_dimension(child.bounds.height, child, &layout, font, true);
                if parent_layout.mode == UiLayoutMode::Overlay
                    && parent_layout.align_items == UiAlignItems::Stretch
                {
                    if child.bounds.width == 0.0 {
                        width = clamp_dimension(inner.width, &layout, false);
                    }
                    if child.bounds.height == 0.0 {
                        height = clamp_dimension(inner.height, &layout, true);
                    }
                }
                UiBounds {
                    x: bounds.x + child.bounds.x - parent_layout.scroll_offset[0],
                    y: bounds.y + child.bounds.y - parent_layout.scroll_offset[1],
                    width,
                    height,
                }
            })
            .collect();
    }
    let row = parent_layout.mode == UiLayoutMode::Row;
    let available = if row { inner.width } else { inner.height };
    let participating_count = node.children.iter().filter(|child| child.visible).count();
    let mut main_sizes = node
        .children
        .iter()
        .map(|child| {
            if !child.visible {
                return 0.0;
            }
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
            if !child.visible {
                return 0.0;
            }
            let margin = child.layout.unwrap_or_default().margin;
            if row {
                margin[3] + margin[1]
            } else {
                margin[0] + margin[2]
            }
        })
        .collect::<Vec<_>>();
    for (size, child) in main_sizes.iter_mut().zip(&node.children) {
        if child.visible {
            *size = clamp_dimension(*size, &child.layout.unwrap_or_default(), !row);
        }
    }
    let fixed_space = outer.iter().sum::<f32>()
        + parent_layout.gap * participating_count.saturating_sub(1) as f32;
    let track_space = (available - fixed_space).max(0.0);
    // A track that reaches a bound is frozen; the next pass gives the residual
    // space to tracks that can still grow or shrink.
    for _ in 0..=node.children.len() {
        let free = track_space - main_sizes.iter().sum::<f32>();
        if free.abs() <= 0.001 {
            break;
        }
        let growing = free > 0.0;
        let factors = node
            .children
            .iter()
            .zip(&main_sizes)
            .map(|(child, size)| {
                if !child.visible {
                    return 0.0;
                }
                let layout = child.layout.unwrap_or_default();
                let (minimum, maximum) = dimension_limits(&layout, !row);
                if growing && *size < maximum - 0.001 {
                    layout.flex_grow
                } else if !growing && *size > minimum + 0.001 {
                    layout.flex_shrink * *size
                } else {
                    0.0
                }
            })
            .collect::<Vec<_>>();
        let total = factors.iter().sum::<f32>();
        if total <= 0.0 {
            break;
        }
        for ((size, child), factor) in main_sizes.iter_mut().zip(&node.children).zip(factors) {
            if factor > 0.0 {
                let layout = child.layout.unwrap_or_default();
                *size = clamp_dimension(*size + free * factor / total, &layout, !row);
            }
        }
    }
    let used = main_sizes.iter().sum::<f32>()
        + outer.iter().sum::<f32>()
        + parent_layout.gap * participating_count.saturating_sub(1) as f32;
    let remaining = (available - used).max(0.0);
    let count = participating_count as f32;
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
    let mut participating_index = 0usize;
    node.children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            if !child.visible {
                return UiBounds {
                    x: inner.x,
                    y: inner.y,
                    width: 0.0,
                    height: 0.0,
                };
            }
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
                cross_size = clamp_dimension(
                    (cross_available
                        - if row {
                            margin[0] + margin[2]
                        } else {
                            margin[3] + margin[1]
                        })
                    .max(0.0),
                    &layout,
                    row,
                );
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
            cursor += main_sizes[index] + main_margin_end;
            participating_index += 1;
            if participating_index < participating_count {
                cursor += gap;
            }
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
        scroll: target.scroll,
        declared_scroll_offset: target.declared_scroll_offset,
        world_depth: target.world_depth,
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
        scroll: active.target.scroll,
        declared_scroll_offset: active.target.declared_scroll_offset,
        world_depth: active.target.world_depth,
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

fn scroll_axis_index(axis: ScrollAxis) -> usize {
    match axis {
        ScrollAxis::X => 0,
        ScrollAxis::Y => 1,
    }
}

fn scroll_axis_length(bounds: UiBounds, axis: ScrollAxis) -> f32 {
    match axis {
        ScrollAxis::X => bounds.width,
        ScrollAxis::Y => bounds.height,
    }
}

fn scroll_track(metrics: ScrollMetrics, axis: ScrollAxis) -> Option<UiBounds> {
    let horizontal = metrics.max_offset[0] > 0.0;
    let vertical = metrics.max_offset[1] > 0.0;
    let viewport = metrics.viewport;
    match axis {
        ScrollAxis::X if horizontal => Some(UiBounds {
            x: viewport.x + 4.0,
            y: viewport.y + viewport.height - 12.0,
            width: (viewport.width - 8.0 - if vertical { 8.0 } else { 0.0 }).max(0.0),
            height: 8.0,
        }),
        ScrollAxis::Y if vertical => Some(UiBounds {
            x: viewport.x + viewport.width - 12.0,
            y: viewport.y + 4.0,
            width: 8.0,
            height: (viewport.height - 8.0 - if horizontal { 8.0 } else { 0.0 }).max(0.0),
        }),
        _ => None,
    }
}

fn scroll_thumb_length(track: UiBounds, metrics: ScrollMetrics, axis: ScrollAxis) -> f32 {
    let index = scroll_axis_index(axis);
    let viewport_size = scroll_axis_length(metrics.viewport, axis);
    (scroll_axis_length(track, axis) * viewport_size / metrics.content_size[index])
        .max(18.0)
        .min(scroll_axis_length(track, axis))
}

fn scroll_thumb(
    track: UiBounds,
    metrics: ScrollMetrics,
    axis: ScrollAxis,
    offset: f32,
    length: f32,
) -> UiBounds {
    let index = scroll_axis_index(axis);
    let position =
        (scroll_axis_length(track, axis) - length) * offset / metrics.max_offset[index].max(1.0);
    match axis {
        ScrollAxis::X => UiBounds {
            x: track.x + position,
            y: track.y,
            width: length,
            height: track.height,
        },
        ScrollAxis::Y => UiBounds {
            x: track.x,
            y: track.y + position,
            width: track.width,
            height: length,
        },
    }
}

fn contains(bounds: UiBounds, position: [f32; 2]) -> bool {
    position[0] >= bounds.x
        && position[0] <= bounds.x + bounds.width
        && position[1] >= bounds.y
        && position[1] <= bounds.y + bounds.height
}

fn normalize_logical_viewport(logical_size: [f32; 2], physical_size: [u32; 2]) -> [f32; 2] {
    [0, 1].map(|axis| {
        if logical_size[axis].is_finite() && logical_size[axis] > 0.0 {
            logical_size[axis]
        } else {
            physical_size[axis].max(1) as f32
        }
    })
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
    use neon_protocol::{
        ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
        ServiceName,
    };
    use neon_ui_runtime::{
        UiRuntime, demo_domain::DemoDragDropDomain, lower_nui_flow_effects, parse_nui_flow,
    };
    use neon_ui_schema::{
        TextRef, UiAlignItems, UiCommand, UiDropPlacement, UiEffect, UiFragmentId,
        UiFragmentSubmission, UiIntent, UiJustifyContent, UiLayout, UiNodeId, UiSemanticEvent,
        UiSemanticEventType, UiTransitionState,
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
    fn debug_drag_keys_resolve_nested_nodes_and_reject_missing_or_ambiguous_keys() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-debug-drag-semantic-keys");
        let mut source = node();
        source.node_id = UiNodeId("nested-source".into());
        source.bounds = UiBounds {
            x: 5.0,
            y: 6.0,
            width: 20.0,
            height: 10.0,
        };
        source.enter_transition = None;
        source.children.clear();
        let mut group = node();
        group.node_id = UiNodeId("group".into());
        group.bounds = UiBounds {
            x: 10.0,
            y: 12.0,
            width: 40.0,
            height: 30.0,
        };
        group.enter_transition = None;
        group.children = vec![source];
        let mut target = node();
        target.node_id = UiNodeId("drop-target".into());
        target.bounds = UiBounds {
            x: 60.0,
            y: 15.0,
            width: 30.0,
            height: 20.0,
        };
        target.enter_transition = None;
        target.children.clear();
        let mut root = node();
        root.node_id = UiNodeId("root".into());
        root.enter_transition = None;
        root.children = vec![group, target];
        let fragment_id = UiFragmentId("first".into());
        let fragment = UiFragment {
            fragment_id: fragment_id.clone(),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::from([(fragment_id, fragment)]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        assert!(renderer.refresh_plan(&fragments, [100.0, 80.0]));
        assert_eq!(
            renderer.debug_drag_gesture_points("nested-source", "drop-target"),
            Ok(([25.0, 23.0], [75.0, 25.0]))
        );
        assert_eq!(
            renderer.debug_drag_gesture_points("missing", "drop-target"),
            Err("unknown_semantic_node_key")
        );

        let mut duplicate_root = node();
        duplicate_root.node_id = UiNodeId("other-root".into());
        duplicate_root.enter_transition = None;
        let mut duplicate = node();
        duplicate.node_id = UiNodeId("nested-source".into());
        duplicate.enter_transition = None;
        duplicate.children.clear();
        duplicate_root.children = vec![duplicate];
        fragments.insert(
            UiFragmentId("second".into()),
            UiFragment {
                fragment_id: UiFragmentId("second".into()),
                revision: Revision(1),
                root: duplicate_root,
                effects: Vec::new(),
            },
        );
        assert!(renderer.refresh_plan(&fragments, [100.0, 80.0]));
        assert_eq!(
            renderer.debug_drag_gesture_points("nested-source", "drop-target"),
            Err("ambiguous_semantic_node_key")
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
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
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
                data_grid_cell: None,
                control_value: None,
                max_text_length: None,
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
    fn data_grid_pointer_hit_uses_scrolled_sampled_cell_bounds() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-data-grid-scrolled-hit");
        let logical = UiBounds {
            x: 120.0,
            y: 30.0,
            width: 80.0,
            height: 24.0,
        };
        let visible = UiBounds {
            x: 20.0,
            y: 30.0,
            width: 80.0,
            height: 24.0,
        };
        let mut visual = UiVisual {
            bounds: logical,
            style: UiStyle::default(),
            kind: UiNodeKind::TextInput,
            enabled: true,
            clip: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: Some(TextRef::Literal {
                value: "display".into(),
            }),
            presentation: None,
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
        };
        let node_path = "grid/assets/data-grid-row-asset-42/cell-name".to_owned();
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: node_path.clone(),
            parent_id: None,
            target: visual.clone(),
            transition: None,
            instance_index: None,
        });
        visual.bounds = visible;
        renderer.sampled.push(visual);
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: node_path.clone(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("grid".into()),
                    revision: Revision(1),
                },
                intent: Some(UiIntent::Invoke {
                    action: "asset.name.edit".into(),
                    params: json!({}),
                }),
                text_input: Some(UiTextInputBinding {
                    node_path,
                    max_length: 16,
                    bounds: logical,
                }),
                data_grid_cell: Some(UiDataGridCellTarget {
                    source_key: "assets_window".into(),
                    stable_row_key: "asset-42".into(),
                    column_key: "name".into(),
                }),
                control_value: Some(UiSemanticPayloadValue::TextHandle {
                    value: neon_ui_schema::UiTextHandle {
                        id: 7,
                        generation: 2,
                    },
                }),
                max_text_length: Some(16),
            },
        );
        renderer.set_pointer_position([30.0, 40.0]);
        assert_eq!(renderer.hit_id_at_pointer(), Some(1));
        assert_eq!(renderer.text_input_at_pointer().unwrap().bounds, visible);
    }

    #[test]
    fn data_grid_text_edit_buffers_until_finish_and_escape_cancels() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-data-grid-text-edit");
        let bounds = UiBounds {
            x: 20.0,
            y: 30.0,
            width: 120.0,
            height: 28.0,
        };
        let input = UiTextInputBinding {
            node_path: "grid/assets/data-grid-row-asset-42/cell-name".into(),
            max_length: 8,
            bounds,
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: input.node_path.clone(),
            parent_id: None,
            target: UiVisual {
                bounds,
                style: UiStyle::default(),
                kind: UiNodeKind::TextInput,
                enabled: true,
                clip: bounds,
                clip_radius: 0.0,
                image: None,
                surface: None,
                text: Some(TextRef::Literal {
                    value: "display".into(),
                }),
                presentation: None,
                scroll: false,
                declared_scroll_offset: [0.0; 2],
                world_depth: None,
            },
            transition: None,
            instance_index: None,
        });
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: input.node_path.clone(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("grid".into()),
                    revision: Revision(1),
                },
                intent: Some(UiIntent::Invoke {
                    action: "asset.name.edit".into(),
                    params: json!({}),
                }),
                text_input: Some(input.clone()),
                data_grid_cell: Some(UiDataGridCellTarget {
                    source_key: "assets_window".into(),
                    stable_row_key: "asset-42".into(),
                    column_key: "name".into(),
                }),
                control_value: Some(UiSemanticPayloadValue::TextHandle {
                    value: neon_ui_schema::UiTextHandle {
                        id: 7,
                        generation: 2,
                    },
                }),
                max_text_length: Some(8),
            },
        );
        renderer.focus_text_input(input.clone());
        assert!(renderer.data_grid_text_input_active());
        assert!(renderer.commit_ime_text("X").is_none());
        assert_eq!(renderer.editing.committed, "displayX");
        renderer.focus_text_input(input.clone());
        assert_eq!(renderer.editing.committed, "displayX");
        let (binding, value) = renderer.finish_data_grid_text_input().unwrap();
        assert_eq!(value, "displayX");
        assert_eq!(binding.data_grid_cell.unwrap().column_key, "name");
        assert!(!renderer.data_grid_text_input_active());

        renderer.focus_text_input(input);
        assert!(renderer.commit_ime_text("Y").is_none());
        assert!(renderer.cancel_data_grid_text_input());
        assert!(!renderer.data_grid_text_input_active());
    }

    #[test]
    fn data_grid_committed_text_survives_a_replacement_text_handle_frame() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-data-grid-text-display-cache");
        let input = UiTextInputBinding {
            node_path: "grid/assets/data-grid-row-asset-42/cell-name".into(),
            max_length: 32,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 24.0,
            },
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: input.node_path.clone(),
            parent_id: None,
            target: UiVisual {
                bounds: input.bounds,
                style: UiStyle::default(),
                kind: UiNodeKind::TextInput,
                enabled: true,
                clip: input.bounds,
                clip_radius: 0.0,
                image: None,
                surface: None,
                text: Some(TextRef::Literal {
                    value: "before".into(),
                }),
                presentation: None,
                scroll: false,
                declared_scroll_offset: [0.0; 2],
                world_depth: None,
            },
            transition: None,
            instance_index: None,
        });
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: input.node_path.clone(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("grid".into()),
                    revision: Revision(1),
                },
                intent: None,
                text_input: Some(input.clone()),
                data_grid_cell: Some(UiDataGridCellTarget {
                    source_key: "assets_window".into(),
                    stable_row_key: "asset-42".into(),
                    column_key: "name".into(),
                }),
                control_value: None,
                max_text_length: Some(32),
            },
        );
        renderer.focus_text_input(input);
        assert!(renderer.commit_ime_text(" after").is_none());
        assert_eq!(
            renderer.finish_data_grid_text_input().unwrap().1,
            "before after"
        );

        let mut root = node();
        root.node_id = UiNodeId("assets".into());
        root.kind = UiNodeKind::DataGrid;
        root.bounds.width = 120.0;
        root.bounds.height = 48.0;
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 1,
            row_height: 24,
            overscan: 0,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 120,
                presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                    max_chars: 32,
                    intent: "asset.name.set".into(),
                },
            }],
        };
        let replacement = HashMap::from([(
            UiFragmentId("replacement-grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("replacement-grid".into()),
                revision: Revision(2),
                root,
                effects: vec![UiEffect::DataGridFrame {
                    declaration,
                    frame: neon_ui_schema::UiDataGridFrame {
                        list_revision: Revision(2),
                        total_rows: 1,
                        first_row: 0,
                        window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                            stable_row_key: "asset-42".into(),
                            cells: std::collections::BTreeMap::from([(
                                "name".into(),
                                neon_ui_schema::UiDataGridCell {
                                    value: neon_ui_schema::UiInputValue::TextHandle {
                                        value: neon_ui_schema::UiTextHandle {
                                            id: 70,
                                            generation: 4,
                                        },
                                    },
                                    display: neon_ui_schema::UiTextHandle {
                                        id: 71,
                                        generation: 5,
                                    },
                                    presentation_override: None,
                                },
                            )]),
                        }],
                        expected_program_revision: neon_ui_schema::UiProgramRevision {
                            program_id: "grid-test".into(),
                            revision: Revision(2),
                            schema_version: 1,
                            capabilities: Vec::new(),
                        },
                    },
                }],
            },
        )]);

        renderer.reconcile_data_grid_text_display_cache(&replacement);
        let flattened = flatten_fragments_with_data_grid_display_cache(
            &replacement,
            [128.0, 80.0],
            None,
            &renderer.data_grid_text_display_cache,
            &renderer.available_cameras,
        );
        let label = &flattened
            .iter()
            .find(|(path, _, _, _)| path.ends_with("data-grid-row-asset-42/cell-name"))
            .unwrap()
            .2;
        assert_eq!(
            text_ref_value(label.text.as_ref().expect("DataGrid cell has text")),
            Some("before after")
        );
    }

    #[test]
    fn data_grid_cell_keeps_display_text_separate_from_typed_handle() {
        let cell = neon_ui_schema::UiDataGridCell {
            value: neon_ui_schema::UiInputValue::TextHandle {
                value: neon_ui_schema::UiTextHandle {
                    id: 7,
                    generation: 2,
                },
            },
            display: neon_ui_schema::UiTextHandle {
                id: 70,
                generation: 4,
            },
            presentation_override: None,
        };
        assert_eq!(data_grid_cell_display_text(&cell, None), "text#70:4");
    }

    #[test]
    fn data_grid_dropdown_click_resolves_declared_typed_option() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-data-grid-dropdown");
        let node_path = "grid/assets/data-grid-row-asset-42/cell-state".to_owned();
        let bounds = UiBounds {
            x: 20.0,
            y: 30.0,
            width: 100.0,
            height: 24.0,
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([200, 160], [200.0, 160.0]);
        renderer.plan.push(PlannedNode {
            id: node_path.clone(),
            parent_id: None,
            target: UiVisual {
                bounds,
                style: UiStyle::default(),
                kind: UiNodeKind::Dropdown,
                enabled: true,
                clip: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 160.0,
                },
                clip_radius: 0.0,
                image: None,
                surface: None,
                text: Some(TextRef::Literal {
                    value: "ready".into(),
                }),
                presentation: Some(UiControlPresentation::Choice {
                    token: "ready".into(),
                    options: vec!["ready".into(), "review".into()],
                    selected: false,
                }),
                scroll: false,
                declared_scroll_offset: [0.0; 2],
                world_depth: None,
            },
            transition: None,
            instance_index: None,
        });
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path,
                fragment: UiFragmentRevision {
                    id: UiFragmentId("grid".into()),
                    revision: Revision(1),
                },
                intent: Some(UiIntent::Invoke {
                    action: "asset.state.select".into(),
                    params: json!({}),
                }),
                text_input: None,
                data_grid_cell: Some(UiDataGridCellTarget {
                    source_key: "assets_window".into(),
                    stable_row_key: "asset-42".into(),
                    column_key: "state".into(),
                }),
                control_value: None,
                max_text_length: None,
            },
        );
        renderer.set_pointer_position([30.0, 40.0]);
        assert!(renderer.toggle_dropdown_at_pointer());
        renderer.set_pointer_position([30.0, 90.0]);
        let (binding, value) = renderer.dropdown_option_at_pointer().unwrap();
        assert_eq!(binding.data_grid_cell.unwrap().column_key, "state");
        assert_eq!(
            value,
            UiSemanticPayloadValue::Enum {
                value: "review".into()
            }
        );
    }

    #[test]
    fn tabs_render_labeled_selected_segments_and_select_the_clicked_option() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-tabs");
        let node_path = "gallery/mode-tabs".to_owned();
        let bounds = UiBounds {
            x: 20.0,
            y: 30.0,
            width: 150.0,
            height: 32.0,
        };
        let visual = UiVisual {
            bounds,
            style: UiStyle::default(),
            kind: UiNodeKind::Tabs,
            enabled: true,
            clip: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: Some(TextRef::Literal {
                value: "Modes".into(),
            }),
            presentation: Some(UiControlPresentation::Choice {
                token: "beta".into(),
                options: vec!["alpha".into(), "beta".into(), "gamma".into()],
                selected: true,
            }),
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: node_path.clone(),
            parent_id: None,
            target: visual.clone(),
            transition: None,
            instance_index: None,
        });
        renderer.sampled.push(visual);
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: node_path.clone(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("gallery".into()),
                    revision: Revision(1),
                },
                intent: Some(UiIntent::Invoke {
                    action: "gallery.tabs.select".into(),
                    params: json!({}),
                }),
                text_input: None,
                data_grid_cell: None,
                control_value: None,
                max_text_length: None,
            },
        );

        assert_eq!(
            renderer
                .tab_option_texts()
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );
        let chrome = component_chrome_instances(&renderer.sampled[0]);
        assert_eq!(chrome.len(), 5);
        assert_eq!(chrome[1].fill, [0.16, 0.35, 0.28, 1.0]);
        assert_eq!(chrome[1].params[1], -4.0);
        assert_eq!(chrome[0].rect[0] + chrome[0].rect[2], chrome[1].rect[0]);
        assert_eq!(chrome[3].params[1], -1.5);

        renderer.set_pointer_position([23.0, 33.0]);
        assert!(renderer.tab_option_at_pointer().is_none());

        renderer.set_pointer_position([145.0, 46.0]);
        let (binding, value) = renderer.tab_option_at_pointer().unwrap();
        assert_eq!(binding.node_path, node_path);
        assert_eq!(
            value,
            UiSemanticPayloadValue::Enum {
                value: "gamma".into()
            }
        );
        assert_eq!(
            renderer
                .component_chrome_instances(&renderer.sampled[0].clone(), "gallery/mode-tabs")
                .len(),
            6
        );

        renderer.sampled[0].enabled = false;
        assert!(renderer.tab_option_at_pointer().is_none());
    }

    #[test]
    fn generic_control_focus_is_local_and_skips_disabled_controls() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-component-focus");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let visual = UiVisual {
            bounds: UiBounds {
                x: 10.0,
                y: 10.0,
                width: 40.0,
                height: 20.0,
            },
            style: UiStyle::default(),
            kind: UiNodeKind::Slider,
            enabled: true,
            clip: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
        };
        renderer.plan.push(PlannedNode {
            id: "gallery/slider".into(),
            parent_id: None,
            target: visual,
            transition: None,
            instance_index: None,
        });
        renderer.hit_bindings.insert(
            1,
            UiHitBinding {
                node_path: "gallery/slider".into(),
                fragment: UiFragmentRevision {
                    id: UiFragmentId("gallery".into()),
                    revision: Revision(1),
                },
                intent: None,
                text_input: None,
                data_grid_cell: None,
                control_value: None,
                max_text_length: None,
            },
        );
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
    fn scroll_view_handles_wheel_and_thumb_drag_locally() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-local-scroll");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let viewport = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let scroll = UiVisual {
            bounds: viewport,
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: viewport,
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
            scroll: true,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
        };
        let child = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            clip: viewport,
            ..scroll.clone()
        };
        renderer.plan = vec![
            PlannedNode {
                id: "f/scroll".into(),
                parent_id: None,
                target: scroll,
                transition: None,
                instance_index: None,
            },
            PlannedNode {
                id: "f/content".into(),
                parent_id: Some("f/scroll".into()),
                target: child,
                transition: None,
                instance_index: None,
            },
        ];
        renderer.update_scroll_metrics();
        renderer.set_pointer_position([50.0, 50.0]);
        assert!(renderer.scroll_wheel_at_pointer([0.0, -24.0]));
        assert_eq!(renderer.scroll_offsets["f/scroll"], [0.0, 24.0]);
        renderer.set_pointer_position([95.0, 15.0]);
        assert!(renderer.begin_scroll_drag_at_pointer());
        renderer.set_pointer_position([95.0, 70.0]);
        assert!(renderer.update_scroll_drag());
        assert!(renderer.scroll_offsets["f/scroll"][1] > 24.0);
        renderer.end_scroll_drag();
        assert!(!renderer.scroll_drag_active());
        renderer.set_pointer_position([50.0, 50.0]);
        assert!(renderer.scroll_wheel_at_pointer([-24.0, 0.0]));
        assert_eq!(renderer.scroll_offsets["f/scroll"][0], 24.0);
        renderer.set_pointer_position([15.0, 92.0]);
        assert!(renderer.begin_scroll_drag_at_pointer());
        renderer.set_pointer_position([70.0, 92.0]);
        assert!(renderer.update_scroll_drag());
        assert!(renderer.scroll_offsets["f/scroll"][0] > 24.0);
    }

    #[test]
    fn scroll_view_middle_pan_updates_both_axes_and_clamps() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-middle-scroll-pan");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let viewport = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let scroll = UiVisual {
            bounds: viewport,
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: viewport,
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
            scroll: true,
            declared_scroll_offset: [20.0, 30.0],
        };
        let child = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            clip: viewport,
            ..scroll.clone()
        };
        renderer.plan = vec![
            PlannedNode {
                id: "f/scroll".into(),
                parent_id: None,
                target: scroll,
                transition: None,
                instance_index: None,
            },
            PlannedNode {
                id: "f/content".into(),
                parent_id: Some("f/scroll".into()),
                target: child,
                transition: None,
                instance_index: None,
            },
        ];
        renderer.update_scroll_metrics();
        renderer.set_pointer_position([50.0, 50.0]);
        assert!(renderer.begin_scroll_pan_at_pointer());
        renderer.set_pointer_position([20.0, 10.0]);
        assert!(renderer.update_scroll_pan());
        assert_eq!(renderer.scroll_offsets["f/scroll"], [50.0, 70.0]);
        renderer.set_pointer_position([-500.0, -500.0]);
        assert!(renderer.update_scroll_pan());
        assert_eq!(renderer.scroll_offsets["f/scroll"], [200.0, 200.0]);
        renderer.end_scroll_pan();
        assert!(!renderer.scroll_pan_active());
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
            world_depth: None,
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
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-component-control-hits");
        let kinds = [
            UiNodeKind::Checkbox,
            UiNodeKind::RadioButton,
            UiNodeKind::Slider,
            UiNodeKind::DragValue,
            UiNodeKind::Combo,
            UiNodeKind::Dropdown,
            UiNodeKind::Selectable,
            UiNodeKind::ListBox,
            UiNodeKind::Scrollbar,
        ];
        let mut root = node();
        root.node_id = UiNodeId("gallery".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 128.0,
        };
        root.enter_transition = None;
        root.children = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| UiNode {
                node_id: UiNodeId(format!("control-{index}")),
                kind: kind.clone(),
                bounds: UiBounds {
                    x: 4.0,
                    y: 4.0 + index as f32 * 12.0,
                    width: 56.0,
                    height: 8.0,
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
            })
            .collect();
        root.children.push(UiNode {
            node_id: UiNodeId("disabled-slider".into()),
            kind: UiNodeKind::Slider,
            bounds: UiBounds {
                x: 72.0,
                y: 4.0,
                width: 48.0,
                height: 8.0,
            },
            layout: None,
            visible: true,
            enabled: false,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        });
        root.children.push(UiNode {
            node_id: UiNodeId("progress".into()),
            kind: UiNodeKind::ProgressBar,
            bounds: UiBounds {
                x: 72.0,
                y: 20.0,
                width: 48.0,
                height: 8.0,
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
            world_depth: None,
            children: Vec::new(),
        });
        let pixels = render_hit_ids_for_test(
            &device,
            &queue,
            &HashMap::from([(
                UiFragmentId("component-gallery".into()),
                UiFragment {
                    fragment_id: UiFragmentId("component-gallery".into()),
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [256, 128],
        );
        for index in 0..kinds.len() {
            assert_ne!(pixels[(8 + index * 12) * 256 + 12], u32::MAX);
        }
        assert_eq!(
            pixels[8 * 256 + 84],
            u32::MAX,
            "disabled controls must not receive focusable hits"
        );
        assert_eq!(
            pixels[24 * 256 + 84],
            u32::MAX,
            "progress is display-only and has no local hit target"
        );
    }

    #[test]
    fn data_grid_window_request_uses_scroll_viewport_and_overscan() {
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 12,
            row_height: 24,
            overscan: 2,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 96,
                presentation: neon_ui_schema::UiDataGridPresentation::Text,
            }],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 10_000,
            first_row: 0,
            window_rows: Vec::new(),
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        assert_eq!(
            data_grid_requested_range(&frame, &declaration, 240.0, 252.0),
            Some((7, 12))
        );
        assert_eq!(
            data_grid_requested_range(&frame, &declaration, 999_999.0, 252.0),
            Some((9_988, 12))
        );
    }

    #[test]
    fn data_grid_thumb_drag_holds_body_until_one_release_window_is_applied() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-data-grid-thumb-drag-hold");
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "grid".into(),
            source_key: "rows".into(),
            max_window_rows: 8,
            row_height: 20,
            overscan: 1,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 120,
                presentation: neon_ui_schema::UiDataGridPresentation::Text,
            }],
        };
        let row = |index| neon_ui_schema::UiDataGridWindowRow {
            stable_row_key: format!("row-{index}"),
            cells: std::collections::BTreeMap::from([(
                "name".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::U32 {
                        value: index as u32,
                    },
                    display: neon_ui_schema::UiTextHandle {
                        id: index + 1,
                        generation: 1,
                    },
                    presentation_override: None,
                },
            )]),
        };
        let fragments_at = |revision, first_row| {
            let root = UiNode {
                node_id: UiNodeId("grid".into()),
                kind: UiNodeKind::DataGrid,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 120.0,
                    height: 100.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle {
                    background_color: [0.08, 0.1, 0.12, 1.0],
                    ..UiStyle::default()
                },
                enter_transition: None,
                children: Vec::new(),
            };
            let fragment = UiFragment {
                fragment_id: UiFragmentId("f".into()),
                revision: Revision(revision),
                root,
                effects: vec![UiEffect::DataGridFrame {
                    declaration: declaration.clone(),
                    frame: neon_ui_schema::UiDataGridFrame {
                        list_revision: Revision(1),
                        total_rows: 100,
                        first_row,
                        window_rows: (first_row..first_row + 8).map(row).collect(),
                        expected_program_revision: neon_ui_schema::UiProgramRevision {
                            program_id: "drag-hold".into(),
                            revision: Revision(1),
                            schema_version: 1,
                            capabilities: Vec::new(),
                        },
                    },
                }],
            };
            HashMap::from([(fragment.fragment_id.clone(), fragment)])
        };
        let body_rows = |renderer: &UiWgpuRenderer| {
            renderer
                .plan
                .iter()
                .filter(|node| {
                    node.id.starts_with("f/grid/data-grid-row-") && !node.id.contains("/cell-")
                })
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        let visible_body_rows = |renderer: &UiWgpuRenderer| {
            renderer
                .plan
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.id.starts_with("f/grid/data-grid-row-") && !node.id.contains("/cell-")
                })
                .filter_map(|(index, node)| {
                    let visual = renderer.visual_at(index);
                    (intersect_clip(Some(visual.clip), visual.bounds).height > 0.0)
                        .then(|| node.id.clone())
                })
                .collect::<Vec<_>>()
        };
        let nonblank = |pixels: &[u8]| pixels.chunks_exact(4).any(|pixel| pixel[3] != 0);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let initial = fragments_at(1, 0);
        let initial_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &initial,
            [120, 100],
            0.0,
        );
        assert!(nonblank(&initial_pixels));
        let initial_rows = body_rows(&renderer);
        let initial_visible_rows = visible_body_rows(&renderer);
        assert!(!initial_visible_rows.is_empty());

        renderer.set_pointer_position([115.0, 10.0]);
        assert!(renderer.begin_scroll_drag_at_pointer());
        renderer.set_pointer_position([115.0, 70.0]);
        assert!(renderer.update_scroll_drag());
        let desired_offset = renderer.scroll_offsets["f/grid"][1];
        assert!(desired_offset > 0.0);
        let mut sequence = 0;
        assert!(
            renderer
                .data_grid_window_requests(&initial, 1, Revision(1), &mut sequence, None, false)
                .is_empty(),
            "CursorMoved must not schedule a DataGrid window request"
        );
        let drag_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &initial,
            [120, 100],
            1.0,
        );
        assert!(nonblank(&drag_pixels));
        assert_eq!(body_rows(&renderer), initial_rows);
        assert_eq!(visible_body_rows(&renderer), initial_visible_rows);

        assert_eq!(renderer.end_scroll_drag().as_deref(), Some("f/grid"));
        let requests = renderer.data_grid_window_requests(
            &initial,
            1,
            Revision(1),
            &mut sequence,
            Some("f/grid"),
            true,
        );
        assert_eq!(
            requests.len(),
            1,
            "release must schedule exactly one request"
        );
        let expected_first_row = data_grid_requested_range(
            renderer.data_grid_frames.get("f/grid").unwrap(),
            &declaration,
            desired_offset,
            100.0,
        )
        .unwrap()
        .0;
        assert_eq!(requests[0].requested_first_row, expected_first_row);
        assert!(
            renderer
                .data_grid_window_requests(
                    &initial,
                    1,
                    Revision(1),
                    &mut sequence,
                    Some("f/grid"),
                    true,
                )
                .is_empty(),
            "a pending release request must not be duplicated"
        );
        let pending_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &initial,
            [120, 100],
            2.0,
        );
        assert!(nonblank(&pending_pixels));
        assert_eq!(body_rows(&renderer), initial_rows);
        assert_eq!(visible_body_rows(&renderer), initial_visible_rows);

        let mut unrelated = initial.clone();
        unrelated
            .get_mut(&UiFragmentId("f".into()))
            .unwrap()
            .revision = Revision(2);
        let unrelated_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &unrelated,
            [120, 100],
            2.5,
        );
        assert!(nonblank(&unrelated_pixels));
        assert_eq!(body_rows(&renderer), initial_rows);
        assert_eq!(visible_body_rows(&renderer), initial_visible_rows);

        let replacement = fragments_at(3, requests[0].requested_first_row);
        let accepted_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &replacement,
            [120, 100],
            3.0,
        );
        assert!(nonblank(&accepted_pixels));
        assert!(!renderer.data_grid_scroll_holds.contains_key("f/grid"));
        assert_eq!(renderer.scroll_offsets["f/grid"][1], desired_offset);
        assert!(
            body_rows(&renderer)
                .iter()
                .all(|path| !initial_rows.contains(path))
        );

        let rollback = fragments_at(4, 0);
        renderer.scroll_offsets.insert("f/grid".into(), [0.0; 2]);
        render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &rollback,
            [120, 100],
            4.0,
        );
        renderer.set_pointer_position([115.0, 10.0]);
        assert!(renderer.begin_scroll_drag_at_pointer());
        renderer.set_pointer_position([115.0, 90.0]);
        assert!(renderer.update_scroll_drag());
        let rollback_offset = renderer.data_grid_scroll_holds["f/grid"].body_offset;
        renderer.end_scroll_drag();
        let failed = renderer.data_grid_window_requests(
            &rollback,
            1,
            Revision(4),
            &mut sequence,
            Some("f/grid"),
            true,
        );
        assert_eq!(failed.len(), 1);
        assert!(renderer.fail_data_grid_window_request(failed[0].sequence));
        assert!(!renderer.data_grid_scroll_holds.contains_key("f/grid"));
        assert_eq!(renderer.scroll_offsets["f/grid"], rollback_offset);
    }

    #[test]
    fn data_grid_wide_columns_stretch_proportionally_without_a_blank_track() {
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "grid".into(),
            source_key: "rows".into(),
            max_window_rows: 4,
            row_height: 24,
            overscan: 0,
            columns: [100_u32, 200]
                .into_iter()
                .enumerate()
                .map(|(index, width)| neon_ui_schema::UiDataGridColumn {
                    key: format!("column-{index}"),
                    label: format!("Column {index}"),
                    width,
                    presentation: neon_ui_schema::UiDataGridPresentation::Text,
                })
                .collect(),
        };
        let (widths, content_width, horizontal, vertical) = data_grid_effective_columns(
            &declaration,
            UiBounds {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 100.0,
            },
            100,
            24.0,
        );

        assert!(vertical);
        assert!(!horizontal);
        assert!((content_width - 488.0).abs() < 0.001);
        assert!((widths[1] / widths[0] - 2.0).abs() < 0.001);
    }

    #[test]
    fn data_grid_narrow_columns_preserve_basis_for_horizontal_scroll() {
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "grid".into(),
            source_key: "rows".into(),
            max_window_rows: 4,
            row_height: 24,
            overscan: 0,
            columns: [100_u32, 200]
                .into_iter()
                .enumerate()
                .map(|(index, width)| neon_ui_schema::UiDataGridColumn {
                    key: format!("column-{index}"),
                    label: format!("Column {index}"),
                    width,
                    presentation: neon_ui_schema::UiDataGridPresentation::Text,
                })
                .collect(),
        };
        let grid = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 250.0,
            height: 100.0,
        };
        let (widths, content_width, horizontal, _) =
            data_grid_effective_columns(&declaration, grid, 100, 24.0);

        assert!(horizontal);
        assert_eq!(widths, vec![100.0, 200.0]);
        assert_eq!(content_width - grid.width, 50.0);
    }

    #[test]
    fn data_grid_frame_keeps_rows_at_their_logical_offsets() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-data-grid-frame");
        let root = UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 8.0,
                y: 8.0,
                width: 96.0,
                height: 56.0,
            },
            layout: None,
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
            world_depth: None,
            children: Vec::new(),
        };
        let cell = |id| neon_ui_schema::UiDataGridCell {
            value: neon_ui_schema::UiInputValue::TextHandle {
                value: neon_ui_schema::UiTextHandle { id, generation: 1 },
            },
            display: neon_ui_schema::UiTextHandle {
                id: id + 100,
                generation: 2,
            },
            presentation_override: None,
        };
        let row = |key: &str, id| neon_ui_schema::UiDataGridWindowRow {
            stable_row_key: key.into(),
            cells: std::collections::BTreeMap::from([("name".into(), cell(id))]),
        };
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 2,
            row_height: 12,
            overscan: 0,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 96,
                presentation: neon_ui_schema::UiDataGridPresentation::Text,
            }],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 99,
            first_row: 40,
            window_rows: vec![
                row("asset-41", 1),
                row("asset-42", 2),
                row("must-not-render", 3),
            ],
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        let fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root: root.clone(),
                effects: vec![UiEffect::DataGridFrame {
                    declaration: declaration.clone(),
                    frame: frame.clone(),
                }],
            },
        )]);

        let flattened = flatten_fragments(&fragments, [128.0, 80.0], None);
        let row_y = |row| {
            flattened
                .iter()
                .find(|(path, _, _, _)| path == row)
                .unwrap()
                .2
                .bounds
                .y
        };
        assert_eq!(row_y("grid/assets/data-grid-row-asset-41"), 492.0);
        assert_eq!(row_y("grid/assets/data-grid-row-asset-42"), 504.0);
        assert_eq!(
            flattened
                .iter()
                .filter(|(path, _, _, _)| {
                    path.ends_with("data-grid-row-asset-41")
                        || path.ends_with("data-grid-row-asset-42")
                })
                .count(),
            2
        );

        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [128, 80],
            0.0,
            &[],
            Vec::new(),
        );
        let alpha = |x: usize, y: usize| pixels[(y * 128 + x) * 4 + 3];
        assert!(alpha(12, 6) > 0, "header must render");
        assert_eq!(
            alpha(12, 18),
            0,
            "unscrolled off-window rows must not render"
        );

        let mut initial_frame = frame.clone();
        initial_frame.first_row = 0;
        initial_frame.total_rows = 200;
        initial_frame.window_rows = vec![row("asset-1", 4), row("asset-2", 5)];
        let initial_fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root: root.clone(),
                effects: vec![UiEffect::DataGridFrame {
                    declaration: declaration.clone(),
                    frame: initial_frame,
                }],
            },
        )]);
        let mut replacement_frame = frame;
        replacement_frame.first_row = 98;
        replacement_frame.total_rows = 200;
        replacement_frame.window_rows = vec![row("asset-99", 6), row("asset-100", 7)];
        let replacement_fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(2),
                root,
                effects: vec![UiEffect::DataGridFrame {
                    declaration,
                    frame: replacement_frame,
                }],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &initial_fragments,
            [128, 80],
            0.0,
        );
        renderer
            .scroll_offsets
            .insert("grid/assets".into(), [0.0, 1_200.0]);
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &replacement_fragments,
            [128, 80],
            0.0,
        );
        let alpha = |x: usize, y: usize| pixels[(y * 128 + x) * 4 + 3];
        assert!(
            alpha(12, 6) > 0,
            "replacement rows must remain visible at row 100"
        );
    }

    #[test]
    fn data_grid_presentations_expand_to_stable_cell_visuals() {
        let root = UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 400.0,
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
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        };
        let cell = |id, presentation_override| neon_ui_schema::UiDataGridCell {
            value: neon_ui_schema::UiInputValue::TextHandle {
                value: neon_ui_schema::UiTextHandle { id, generation: 1 },
            },
            display: neon_ui_schema::UiTextHandle { id, generation: 1 },
            presentation_override,
        };
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 1,
            row_height: 24,
            overscan: 0,
            columns: vec![
                neon_ui_schema::UiDataGridColumn {
                    key: "state".into(),
                    label: "State".into(),
                    width: 100,
                    presentation: neon_ui_schema::UiDataGridPresentation::Select {
                        intent: "asset.state.select".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "owner".into(),
                    label: "Owner".into(),
                    width: 100,
                    presentation: neon_ui_schema::UiDataGridPresentation::Dropdown {
                        options: vec!["me".into(), "team".into()],
                        intent: "asset.owner.select".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "notes".into(),
                    label: "Notes".into(),
                    width: 100,
                    presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                        max_chars: 80,
                        intent: "asset.notes.edit".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "title".into(),
                    label: "Title".into(),
                    width: 100,
                    presentation: neon_ui_schema::UiDataGridPresentation::Text,
                },
            ],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 1,
            first_row: 0,
            window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: "asset-42".into(),
                cells: std::collections::BTreeMap::from([
                    ("state".into(), cell(1, None)),
                    (
                        "owner".into(),
                        cell(
                            2,
                            Some(neon_ui_schema::UiDataGridCellPresentation::Edit {
                                max_chars: 20,
                            }),
                        ),
                    ),
                    (
                        "notes".into(),
                        cell(
                            3,
                            Some(neon_ui_schema::UiDataGridCellPresentation::Dropdown {
                                options: vec!["short".into(), "long".into()],
                            }),
                        ),
                    ),
                    ("title".into(), cell(4, None)),
                ]),
            }],
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        let fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::DataGridFrame { declaration, frame }],
            },
        )]);
        let flattened = flatten_fragments(&fragments, [400.0, 80.0], None);
        let kind = |path| {
            flattened
                .iter()
                .find(|(candidate, _, _, _)| candidate == path)
                .unwrap()
                .2
                .kind
                .clone()
        };
        assert_eq!(
            kind("grid/assets/data-grid-row-asset-42/cell-state"),
            UiNodeKind::Combo
        );
        assert_eq!(
            kind("grid/assets/data-grid-row-asset-42/cell-owner"),
            UiNodeKind::TextInput
        );
        assert_eq!(
            kind("grid/assets/data-grid-row-asset-42/cell-notes"),
            UiNodeKind::Dropdown
        );
        assert_eq!(
            kind("grid/assets/data-grid-row-asset-42/cell-title"),
            UiNodeKind::Label
        );
        let visual = |path| {
            &flattened
                .iter()
                .find(|(candidate, _, _, _)| candidate == path)
                .unwrap()
                .2
        };
        assert!(
            !component_chrome_instances(visual("grid/assets/data-grid-row-asset-42/cell-state"))
                .is_empty()
        );
        assert!(
            !component_chrome_instances(visual("grid/assets/data-grid-row-asset-42/cell-owner"))
                .is_empty()
        );
        assert!(
            !component_chrome_instances(visual("grid/assets/data-grid-row-asset-42/cell-notes"))
                .is_empty()
        );
    }

    #[test]
    fn data_grid_interactive_cells_register_semantic_bindings() {
        let root = UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 320.0,
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
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        };
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 1,
            row_height: 24,
            overscan: 0,
            columns: vec![
                neon_ui_schema::UiDataGridColumn {
                    key: "selected".into(),
                    label: "Selected".into(),
                    width: 80,
                    presentation: neon_ui_schema::UiDataGridPresentation::Select {
                        intent: "asset.selected.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "state".into(),
                    label: "State".into(),
                    width: 80,
                    presentation: neon_ui_schema::UiDataGridPresentation::Dropdown {
                        options: vec!["ready".into(), "review".into()],
                        intent: "asset.state.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "name".into(),
                    label: "Name".into(),
                    width: 160,
                    presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                        max_chars: 5,
                        intent: "asset.name.set".into(),
                    },
                },
            ],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 1,
            first_row: 0,
            window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: "asset-42".into(),
                cells: std::collections::BTreeMap::from([
                    (
                        "selected".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Bool { value: false },
                            display: neon_ui_schema::UiTextHandle {
                                id: 1,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "state".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Enum {
                                value: "ready".into(),
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 2,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "name".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::TextHandle {
                                value: neon_ui_schema::UiTextHandle {
                                    id: 3,
                                    generation: 1,
                                },
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 3,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                ]),
            }],
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        let fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::DataGridFrame { declaration, frame }],
            },
        )]);
        let bindings = collect_hit_declarations(&fragments);
        let select = &bindings["grid/assets/data-grid-row-asset-42/cell-selected"];
        assert_eq!(
            select.data_grid_cell.as_ref().unwrap().stable_row_key,
            "asset-42"
        );
        assert_eq!(
            select.control_value,
            Some(UiSemanticPayloadValue::Bool { value: true })
        );
        assert_eq!(
            select.intent,
            Some(UiIntent::Invoke {
                action: "asset.selected.set".into(),
                params: Value::Object(Default::default())
            })
        );
        let dropdown = &bindings["grid/assets/data-grid-row-asset-42/cell-state"];
        assert!(dropdown.control_value.is_none());
        let edit = &bindings["grid/assets/data-grid-row-asset-42/cell-name"];
        assert_eq!(edit.max_text_length, Some(5));
        assert_eq!(
            edit.control_value,
            Some(UiSemanticPayloadValue::TextHandle {
                value: neon_ui_schema::UiTextHandle {
                    id: 3,
                    generation: 1
                }
            })
        );
    }

    #[test]
    fn data_grid_interactive_cells_hit_gpu_id_map_with_scroll_offsets() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-data-grid-hit-id-map");
        let root = UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 8.0,
                y: 8.0,
                width: 320.0,
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
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        };
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 1,
            row_height: 24,
            overscan: 0,
            columns: vec![
                neon_ui_schema::UiDataGridColumn {
                    key: "selected".into(),
                    label: "Selected".into(),
                    width: 96,
                    presentation: neon_ui_schema::UiDataGridPresentation::Select {
                        intent: "asset.selected.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "state".into(),
                    label: "State".into(),
                    width: 96,
                    presentation: neon_ui_schema::UiDataGridPresentation::Dropdown {
                        options: vec!["ready".into(), "review".into()],
                        intent: "asset.state.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "name".into(),
                    label: "Name".into(),
                    width: 96,
                    presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                        max_chars: 12,
                        intent: "asset.name.set".into(),
                    },
                },
            ],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 4,
            first_row: 0,
            window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: "asset-42".into(),
                cells: std::collections::BTreeMap::from([
                    (
                        "selected".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Bool { value: false },
                            display: neon_ui_schema::UiTextHandle {
                                id: 1,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "state".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Enum {
                                value: "ready".into(),
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 2,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "name".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::TextHandle {
                                value: neon_ui_schema::UiTextHandle {
                                    id: 3,
                                    generation: 1,
                                },
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 3,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                ]),
            }],
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        let fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::DataGridFrame { declaration, frame }],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let size = [384, 128];

        // Normal drawing resolves the same scrolled visuals consumed by the hit pass.
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            size,
            0.0,
        );
        renderer
            .scroll_offsets
            .insert("grid/assets".into(), [0.0, 8.0]);
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            size,
            1.0,
        );
        let normal_draw_bindings = renderer.hit_bindings.clone();
        let pixels =
            render_hit_ids_with_renderer_for_test(&mut renderer, &device, &queue, &fragments, size);
        assert_eq!(renderer.hit_bindings.len(), normal_draw_bindings.len());
        for (hit_id, binding) in normal_draw_bindings {
            assert_eq!(
                renderer.hit_binding(hit_id).unwrap().node_path,
                binding.node_path
            );
        }

        for (column_key, kind, intent, control_value, max_text_length) in [
            (
                "selected",
                UiNodeKind::Combo,
                "asset.selected.set",
                Some(UiSemanticPayloadValue::Bool { value: true }),
                None,
            ),
            ("state", UiNodeKind::Dropdown, "asset.state.set", None, None),
            (
                "name",
                UiNodeKind::TextInput,
                "asset.name.set",
                Some(UiSemanticPayloadValue::TextHandle {
                    value: neon_ui_schema::UiTextHandle {
                        id: 3,
                        generation: 1,
                    },
                }),
                Some(12),
            ),
        ] {
            let binding = renderer
                .hit_bindings
                .iter()
                .find_map(|(hit_id, binding)| {
                    (binding
                        .data_grid_cell
                        .as_ref()
                        .is_some_and(|cell| cell.column_key == column_key))
                    .then_some((*hit_id, binding))
                })
                .unwrap();
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == binding.1.node_path)
                .unwrap();
            let visual = renderer.visual_at(index);
            let pixel = [
                (visual.bounds.x + visual.bounds.width * 0.5) as usize,
                (visual.bounds.y + visual.bounds.height * 0.5) as usize,
            ];
            let hit_id = pixels[pixel[1] * size[0] as usize + pixel[0]];
            assert_eq!(hit_id, binding.0, "GPU hit ID must match {column_key}");
            let binding = renderer.hit_binding(hit_id).unwrap();
            assert_eq!(renderer.visual_at(index).kind, kind);
            assert_eq!(
                binding.intent,
                Some(UiIntent::Invoke {
                    action: intent.into(),
                    params: Value::Object(Default::default()),
                })
            );
            assert_eq!(binding.control_value, control_value);
            assert_eq!(binding.max_text_length, max_text_length);
            assert_eq!(
                binding.data_grid_cell,
                Some(UiDataGridCellTarget {
                    source_key: "assets_window".into(),
                    stable_row_key: "asset-42".into(),
                    column_key: column_key.into(),
                })
            );
        }
    }

    #[test]
    fn data_grid_controls_handle_first_press_after_normal_draw_only() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-data-grid-first-press");
        let root = UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 8.0,
                y: 8.0,
                width: 320.0,
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
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        };
        let declaration = neon_ui_schema::UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 1,
            row_height: 24,
            overscan: 0,
            columns: vec![
                neon_ui_schema::UiDataGridColumn {
                    key: "selected".into(),
                    label: "Selected".into(),
                    width: 80,
                    presentation: neon_ui_schema::UiDataGridPresentation::Select {
                        intent: "asset.selected.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "state".into(),
                    label: "State".into(),
                    width: 80,
                    presentation: neon_ui_schema::UiDataGridPresentation::Dropdown {
                        options: vec!["ready".into(), "review".into()],
                        intent: "asset.state.set".into(),
                    },
                },
                neon_ui_schema::UiDataGridColumn {
                    key: "name".into(),
                    label: "Name".into(),
                    width: 160,
                    presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                        max_chars: 12,
                        intent: "asset.name.set".into(),
                    },
                },
            ],
        };
        let frame = neon_ui_schema::UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 1,
            first_row: 0,
            window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: "asset-42".into(),
                cells: std::collections::BTreeMap::from([
                    (
                        "selected".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Bool { value: false },
                            display: neon_ui_schema::UiTextHandle {
                                id: 1,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "state".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::Enum {
                                value: "ready".into(),
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 2,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                    (
                        "name".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: neon_ui_schema::UiInputValue::TextHandle {
                                value: neon_ui_schema::UiTextHandle {
                                    id: 3,
                                    generation: 1,
                                },
                            },
                            display: neon_ui_schema::UiTextHandle {
                                id: 30,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    ),
                ]),
            }],
            expected_program_revision: neon_ui_schema::UiProgramRevision {
                program_id: "grid-test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: Vec::new(),
            },
        };
        let fragments = HashMap::from([(
            UiFragmentId("grid".into()),
            UiFragment {
                fragment_id: UiFragmentId("grid".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::DataGridFrame { declaration, frame }],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);

        // This invokes only `draw`; no hit pass or readback has populated state.
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [352, 104],
            0.0,
        );
        let cell_pointer = |renderer: &UiWgpuRenderer, column_key: &str| {
            let binding = renderer
                .hit_bindings
                .values()
                .find(|binding| {
                    binding
                        .data_grid_cell
                        .as_ref()
                        .is_some_and(|cell| cell.column_key == column_key)
                })
                .expect("normal draw must populate the generated cell binding");
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == binding.node_path)
                .unwrap();
            let bounds = renderer.visual_at(index).bounds;
            [
                bounds.x + bounds.width * 0.5,
                bounds.y + bounds.height * 0.5,
            ]
        };

        renderer.set_pointer_position(cell_pointer(&renderer, "name"));
        let input = renderer
            .text_input_at_pointer()
            .expect("edit must focus on first press");
        renderer.focus_text_input(input);
        assert!(renderer.data_grid_text_input_active());
        renderer.clear_text_focus();

        renderer.set_pointer_position(cell_pointer(&renderer, "state"));
        assert!(renderer.toggle_dropdown_at_pointer());
        assert!(renderer.open_dropdown.is_some());
        assert!(renderer.dropdown_debug_snapshot()["open_dropdown"].is_null());
        renderer.close_dropdown();

        renderer.set_pointer_position(cell_pointer(&renderer, "selected"));
        let hit_id = renderer
            .hit_id_at_pointer()
            .expect("select must hit on first press");
        let binding = renderer.hit_binding(hit_id).unwrap();
        assert_eq!(
            binding.control_value,
            Some(UiSemanticPayloadValue::Bool { value: true })
        );
        assert!(renderer.focus_control_at_pointer());
        assert!(
            renderer.semantic_hit_nodes().is_empty(),
            "generated paths must remain renderer-local"
        );
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
    fn two_resident_images_render_from_one_atlas_with_distinct_uvs() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-image-atlas");
        let image = |node_id: &str, bounds: UiBounds, asset_id: u64| {
            let mut value = node();
            value.node_id = UiNodeId(node_id.into());
            value.kind = UiNodeKind::Image;
            value.bounds = bounds;
            value.enter_transition = None;
            value.style.background_color = [1.0, 1.0, 1.0, 1.0];
            value.image = Some(AssetRef {
                project_id: "atlas-test".into(),
                asset_id,
                revision: Revision(1),
                kind: "image".into(),
            });
            value
        };
        let mut root = node();
        root.enter_transition = None;
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 8.0,
        };
        root.children = vec![
            image(
                "red",
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 8.0,
                    height: 8.0,
                },
                1,
            ),
            image(
                "green",
                UiBounds {
                    x: 8.0,
                    y: 0.0,
                    width: 8.0,
                    height: 8.0,
                },
                2,
            ),
        ];
        let fragment_id = UiFragmentId("image-atlas".into());
        let fragments = HashMap::from([(
            fragment_id.clone(),
            UiFragment {
                fragment_id,
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let red = AssetBytes {
            asset: AssetRef {
                project_id: "atlas-test".into(),
                asset_id: 1,
                revision: Revision(1),
                kind: "image".into(),
            },
            media_type: "application/x-neon-rgba8".into(),
            width: Some(2),
            height: Some(2),
            bytes: [255, 0, 0, 255].repeat(4),
        };
        let green = AssetBytes {
            asset: AssetRef {
                project_id: "atlas-test".into(),
                asset_id: 2,
                revision: Revision(1),
                kind: "image".into(),
            },
            media_type: "application/x-neon-rgba8".into(),
            width: Some(2),
            height: Some(2),
            bytes: [0, 255, 0, 128].repeat(4),
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.preload_image(&device, &queue, &red).unwrap();
        renderer.preload_image(&device, &queue, &green).unwrap();
        assert_eq!(renderer.resident_images.len(), 2);
        assert!(renderer.image_atlas.is_some());
        assert_ne!(
            renderer.resident_images[&("atlas-test".into(), 1, 1)].uv,
            renderer.resident_images[&("atlas-test".into(), 2, 1)].uv,
        );
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [16, 8],
            1.0,
        );
        let pixel = |x: usize, y: usize| {
            let offset = (y * 16 + x) * 4;
            &pixels[offset..offset + 4]
        };
        assert!(
            pixel(3, 3)[0] > 200 && pixel(3, 3)[1] < 40 && pixel(3, 3)[3] > 240,
            "red pixel: {:?}",
            pixel(3, 3)
        );
        assert!(
            pixel(11, 3)[1] > 100 && pixel(11, 3)[0] < 40 && pixel(11, 3)[3] > 150,
            "green pixel: {:?}",
            pixel(11, 3)
        );
    }

    #[test]
    fn prepare_interaction_binds_the_first_press_after_fragment_application() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-first-press-preparation");
        let mut root = node();
        root.kind = UiNodeKind::Button;
        root.enter_transition = None;
        let intent = UiIntent::Invoke {
            action: "gallery.first_press".into(),
            params: json!({}),
        };
        let fragments = HashMap::from([(
            UiFragmentId("first-press".into()),
            UiFragment {
                fragment_id: UiFragmentId("first-press".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::BoundSemanticIntent {
                    node_id: UiNodeId("root".into()),
                    intent: intent.clone(),
                }],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.set_pointer_position([20.0, 30.0]);

        renderer.prepare_interaction(&fragments, [160, 120], [160.0, 120.0], 0.0);

        let hit_id = renderer
            .hit_id_at_pointer()
            .expect("first press must resolve a hit");
        assert_eq!(renderer.hit_binding(hit_id).unwrap().intent, Some(intent));
    }

    #[test]
    fn value_gesture_uses_the_current_pointer_value_after_interaction_preparation() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-value-gesture-preparation");
        let mut root = node();
        root.kind = UiNodeKind::DragValue;
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 40.0,
        };
        root.enter_transition = None;
        let fragments = HashMap::from([(
            UiFragmentId("item-count".into()),
            UiFragment {
                fragment_id: UiFragmentId("item-count".into()),
                revision: Revision(1),
                root,
                effects: vec![UiEffect::ControlPresentation {
                    node_id: UiNodeId("root".into()),
                    state: UiControlPresentation::Numeric {
                        value: 12.0,
                        min: 0.0,
                        max: 20.0,
                    },
                }],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.set_pointer_position([278.4, 20.0]);

        renderer.prepare_interaction(&fragments, [320, 80], [320.0, 80.0], 0.0);
        let hit_id = renderer
            .hit_id_at_pointer()
            .expect("drag control must be bound");
        let binding = renderer.hit_binding(hit_id).unwrap();
        assert!(renderer.begin_value_gesture(&binding));
        assert_eq!(
            renderer.finish_value_gesture().map(|(value, _)| value),
            Some(UiSemanticPayloadValue::I32 { value: 15 })
        );
    }

    #[test]
    fn numeric_commit_frames_hold_preview_then_reconcile_or_rollback() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-pending-numeric-commit-frames");
        let fragment = |revision, value| {
            let mut root = node();
            root.kind = UiNodeKind::DragValue;
            root.bounds = UiBounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 40.0,
            };
            root.enter_transition = None;
            let fragment_id = UiFragmentId("numeric".into());
            HashMap::from([(
                fragment_id.clone(),
                UiFragment {
                    fragment_id,
                    revision: Revision(revision),
                    root,
                    effects: vec![
                        UiEffect::ControlPresentation {
                            node_id: UiNodeId("root".into()),
                            state: UiControlPresentation::Numeric {
                                value,
                                min: 0.0,
                                max: 20.0,
                            },
                        },
                        UiEffect::BoundSemanticIntent {
                            node_id: UiNodeId("root".into()),
                            intent: UiIntent::Invoke {
                                action: "numeric.commit".into(),
                                params: json!({}),
                            },
                        },
                    ],
                },
            )])
        };
        let presented_value = |renderer: &UiWgpuRenderer| {
            if let Some(UiSemanticPayloadValue::I32 { value }) =
                renderer.value_previews.get("numeric/root")
            {
                return *value as f32;
            }
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == "numeric/root")
                .unwrap();
            match renderer.sampled[index].presentation {
                Some(UiControlPresentation::Numeric { value, .. }) => value,
                _ => panic!("numeric presentation is required"),
            }
        };
        let start_commit =
            |renderer: &mut UiWgpuRenderer, fragments: &HashMap<UiFragmentId, UiFragment>| {
                renderer.prepare_interaction(fragments, [320, 80], [320.0, 80.0], 0.0);
                let (start, end) = renderer
                    .debug_value_gesture_points("numeric/root", 0.75)
                    .unwrap();
                renderer.set_pointer_position(start);
                let binding = renderer
                    .hit_id_at_pointer()
                    .and_then(|hit_id| renderer.hit_binding(hit_id))
                    .unwrap();
                assert!(renderer.begin_value_gesture(&binding));
                renderer.set_pointer_position(end);
                assert!(renderer.update_value_gesture());
                let (value, presentation) = renderer.finish_value_gesture().unwrap();
                (binding.fragment, value, presentation)
            };

        let initial = fragment(1, 4.0);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let (source_revision, value, presentation) = start_commit(&mut renderer, &initial);
        assert_eq!(value, UiSemanticPayloadValue::I32 { value: 15 });
        let rejected_key = renderer.retain_local_presentation(1, &source_revision, presentation);

        renderer.compose_sampled_visuals(0.0);
        assert_eq!(presented_value(&renderer), 15.0, "release pending frame");
        assert!(renderer.complete_local_presentation(&rejected_key, false, &initial));
        renderer.compose_sampled_visuals(0.0);
        assert_eq!(presented_value(&renderer), 4.0, "rejected rollback frame");

        let (source_revision, _, presentation) = start_commit(&mut renderer, &initial);
        let accepted_key = renderer.retain_local_presentation(2, &source_revision, presentation);
        assert_eq!(presented_value(&renderer), 15.0, "second pending frame");
        let authoritative = fragment(2, 13.0);
        renderer.prepare_interaction(&authoritative, [320, 80], [320.0, 80.0], 0.0);
        assert_eq!(
            presented_value(&renderer),
            13.0,
            "replacement composition must not render the stale preview"
        );
        renderer.complete_local_presentation(&accepted_key, true, &authoritative);
        assert!(renderer.pending_local_presentations.is_empty());
        assert_eq!(
            presented_value(&renderer),
            13.0,
            "accepted authoritative frame"
        );
    }

    #[test]
    fn drag_commit_frames_hold_drop_offset_then_reconcile_or_rollback() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-pending-drag-commit-frames");
        let fragment = |revision, reparented| {
            let mut root = node();
            root.bounds = UiBounds {
                x: 0.0,
                y: 0.0,
                width: 220.0,
                height: 100.0,
            };
            root.enter_transition = None;
            let mut source = node();
            source.node_id = UiNodeId("source".into());
            source.bounds = UiBounds {
                x: if reparented { 15.0 } else { 10.0 },
                y: if reparented { 10.0 } else { 20.0 },
                width: 30.0,
                height: 20.0,
            };
            source.enter_transition = None;
            let mut target = node();
            target.node_id = UiNodeId("target".into());
            target.bounds = UiBounds {
                x: 130.0,
                y: 10.0,
                width: 60.0,
                height: 50.0,
            };
            target.enter_transition = None;
            if reparented {
                target.children.push(source);
                root.children.push(target);
            } else {
                root.children.extend([source, target]);
            }
            let fragment_id = UiFragmentId("drag".into());
            HashMap::from([(
                fragment_id.clone(),
                UiFragment {
                    fragment_id,
                    revision: Revision(revision),
                    root,
                    effects: vec![
                        UiEffect::DragBinding {
                            binding: UiDragBinding {
                                key: "source-drag".into(),
                                source_node_id: UiNodeId("source".into()),
                                axis: UiDragAxis::Both,
                                snap: 0.0,
                                threshold: 1.0,
                                boundary: UiDragBoundary::Surface,
                            },
                        },
                        UiEffect::DropBinding {
                            binding: neon_ui_schema::UiDropBinding {
                                key: "target-drop".into(),
                                target_node_id: UiNodeId("target".into()),
                                accepts_drag_key: "source-drag".into(),
                                placement: UiDropPlacement::Into,
                                presentation_template_key: None,
                                intent: UiIntent::Invoke {
                                    action: "drag.commit".into(),
                                    params: json!({}),
                                },
                            },
                        },
                    ],
                },
            )])
        };
        let source_x = |renderer: &UiWgpuRenderer| {
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == "drag/source")
                .unwrap();
            renderer.sampled[index].bounds.x
        };
        let finish_drag =
            |renderer: &mut UiWgpuRenderer, fragments: &HashMap<UiFragmentId, UiFragment>| {
                renderer.prepare_interaction(fragments, [220, 100], [220.0, 100.0], 0.0);
                renderer.set_pointer_position([25.0, 30.0]);
                assert!(renderer.begin_drag_at_pointer(fragments));
                renderer.set_pointer_position([160.0, 30.0]);
                assert!(renderer.update_drag_preview());
                renderer.finish_drag_at_pointer(fragments).unwrap()
            };

        let initial = fragment(1, false);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let resolved = finish_drag(&mut renderer, &initial);
        let rejected_key =
            renderer.retain_local_presentation(1, &resolved.fragment, resolved.local_presentation);
        renderer.compose_sampled_visuals(0.0);
        assert_eq!(source_x(&renderer), 145.0, "release pending drag frame");
        assert!(renderer.complete_local_presentation(&rejected_key, false, &initial));
        renderer.compose_sampled_visuals(0.0);
        assert_eq!(source_x(&renderer), 10.0, "rejected drag rollback frame");

        let resolved = finish_drag(&mut renderer, &initial);
        let accepted_key =
            renderer.retain_local_presentation(2, &resolved.fragment, resolved.local_presentation);
        renderer.compose_sampled_visuals(0.0);
        assert_eq!(source_x(&renderer), 145.0, "second pending drag frame");
        let authoritative = fragment(2, true);
        renderer.prepare_interaction(&authoritative, [220, 100], [220.0, 100.0], 0.0);
        assert_eq!(source_x(&renderer), 145.0, "authoritative reparent frame");
        let source = renderer
            .plan
            .iter()
            .find(|node| node.id == "drag/source")
            .unwrap();
        assert_eq!(source.parent_id.as_deref(), Some("drag/target"));
        renderer.complete_local_presentation(&accepted_key, true, &authoritative);
        assert!(renderer.pending_local_presentations.is_empty());
        assert_eq!(
            source_x(&renderer),
            145.0,
            "accepted authoritative drag frame"
        );
    }

    #[test]
    fn declared_modal_subtree_escapes_parent_clip() {
        let mut root = node();
        root.layout = Some(UiLayout {
            clip: UiClipPolicy::Bounds,
            ..UiLayout::default()
        });
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        let mut modal = node();
        modal.node_id = UiNodeId("modal".into());
        modal.kind = UiNodeKind::Modal;
        modal.bounds = UiBounds {
            x: 30.0,
            y: 30.0,
            width: 40.0,
            height: 30.0,
        };
        root.children.push(modal);
        let fragment_id = neon_ui_schema::UiFragmentId("fixture".into());
        let flattened = flatten_fragments(
            &HashMap::from([(
                fragment_id.clone(),
                UiFragment {
                    fragment_id,
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [20.0, 20.0],
            None,
        );
        let modal = flattened
            .iter()
            .find(|(id, _, _, _)| id == "fixture/modal")
            .unwrap();
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
            world_depth: None,
            world_depth: None,
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
        let nodes = flatten_fragments(&fragments, [100.0, 80.0], None);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes[1].2.bounds,
            UiBounds {
                x: 8.0,
                y: 6.0,
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
        let nodes = flatten_fragments(&fragments, [200.0, 40.0], None);
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
    fn flex_redistributes_after_tracks_reach_minimum_and_maximum_widths() {
        let make_child = |id: &str, basis: f32, minimum: f32, maximum: Option<f32>| {
            let mut child = node();
            child.node_id = UiNodeId(id.into());
            child.bounds = UiBounds {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            };
            child.layout = Some(UiLayout {
                min_size: Some([minimum, 0.0]),
                max_size: maximum.map(|maximum| [maximum, f32::INFINITY]),
                flex_basis: Some(basis),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..UiLayout::default()
            });
            child.enter_transition = None;
            child.children.clear();
            child
        };
        let bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 40.0,
        };
        let layout = UiLayout {
            mode: UiLayoutMode::Row,
            align_items: UiAlignItems::Stretch,
            ..UiLayout::default()
        };

        let mut growing = node();
        growing.bounds = bounds;
        growing.layout = Some(layout);
        growing.children = vec![
            make_child("left", 100.0, 0.0, Some(120.0)),
            make_child("middle", 100.0, 0.0, Some(160.0)),
            make_child("right", 100.0, 0.0, None),
        ];
        let grown = resolve_children(&growing, bounds, layout, bounds, None);
        assert_eq!(grown[0].width, 120.0);
        assert_eq!(grown[1].width, 160.0);
        assert!((grown[2].width - 220.0).abs() < 0.001);

        let mut shrinking = node();
        shrinking.bounds = bounds;
        shrinking.layout = Some(layout);
        shrinking.children = vec![
            make_child("left", 300.0, 100.0, None),
            make_child("middle", 300.0, 250.0, None),
            make_child("right", 100.0, 100.0, None),
        ];
        let shrunk = resolve_children(&shrinking, bounds, layout, bounds, None);
        assert!((shrunk[0].width - 150.0).abs() < 0.001);
        assert_eq!(shrunk[1].width, 250.0);
        assert_eq!(shrunk[2].width, 100.0);
        assert!(
            shrunk
                .iter()
                .all(|pane| pane.x + pane.width <= bounds.width + 0.001)
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
            world_depth: None,
            world_depth: None,
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
            world_depth: None,
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
            flatten_fragments(&fragments, [1680.0, 900.0], None)
                .iter()
                .find(|node| node.0 == "component-gallery/feature-toggle")
                .and_then(|node| node.2.presentation.as_ref()),
            Some(UiControlPresentation::Toggle { selected: true })
        ));
        let enabled_controls = [
            "feature-toggle",
            "mode-radio",
            "exposure-slider",
            "count-drag",
            "mode-combo",
            "mode-dropdown",
            "item-selectable",
            "item-list",
            "gallery-scroll",
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
        assert_eq!(
            runtime.handle_service_request(submit).status,
            RpcStatus::Accepted
        );
        for (sequence, key) in [
            "feature-toggle",
            "mode-radio",
            "exposure-slider",
            "mode-combo",
            "item-selectable",
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
                data_grid_cell: None,
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
            assert_eq!(
                response.status,
                RpcStatus::Accepted,
                "{key}: {:?}",
                response.error
            );
        }
    }

    #[test]
    fn accepted_into_drop_materializes_visible_target_pixels_from_hidden_template() {
        const SIZE: [u32; 2] = [1680, 900];
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-accepted-drop-materialization");
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        ))
        .expect("component gallery must parse");
        let initial = UiFragment {
            fragment_id: UiFragmentId("component-gallery-drop".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        let intent = initial
            .effects
            .iter()
            .find_map(|effect| match effect {
                UiEffect::DropBinding { binding } if binding.key == "equipment-compass-drop" => {
                    Some(binding.intent.clone())
                }
                _ => None,
            })
            .unwrap();
        let event = UiSemanticEvent {
            event: UiSemanticEventType::DragDrop,
            event_id: "accepted-equipment-drop".into(),
            renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: initial.fragment_id.clone(),
                revision: initial.revision,
            },
            intent,
            pointer: None,
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: Some(neon_ui_schema::UiDragDropPayload {
                source_key: "backpack-compass".into(),
                target_key: "equipment-zone".into(),
                placement: UiDropPlacement::Into,
                presentation_template_key: Some("equipment-item-template".into()),
            }),
        };
        let response = DemoDragDropDomain::new().handle(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("accepted-equipment-drop".into()),
            client: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "renderer-test".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName("demo-domain".into()),
            method: "ui.drag_drop.apply".into(),
            params: json!({"event": event, "fragment": initial.clone()}),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("accepted-equipment-drop".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        let accepted: UiFragment =
            serde_json::from_value(response.result.unwrap()["fragment"].clone()).unwrap();
        accepted.validate().unwrap();

        let instance_key = "equipment-item-template-backpack-compass-r2-equipment-item-template";
        let label_key = "equipment-item-template-backpack-compass-r2-equipment-item-template-label";
        fn find_test_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
            (node.node_id.0 == key).then_some(node).or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_test_node(child, key))
            })
        }
        assert!(find_test_node(&accepted.root, "backpack-compass").is_none());
        assert!(
            !find_test_node(&accepted.root, "equipment-item-template")
                .unwrap()
                .visible
        );
        assert!(
            find_test_node(&accepted.root, instance_key)
                .unwrap()
                .visible
        );
        assert!(matches!(
            find_test_node(&accepted.root, label_key)
                .unwrap()
                .text
                .as_ref(),
            Some(TextRef::Literal { value }) if value == "Brass compass"
        ));

        let initial_fragments = HashMap::from([(initial.fragment_id.clone(), initial.clone())]);
        let accepted_fragments = HashMap::from([(accepted.fragment_id.clone(), accepted.clone())]);
        let font = fixture_font();
        let before = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &initial_fragments,
            SIZE,
            1.0,
            std::slice::from_ref(&font),
            Vec::new(),
        );
        let after = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &accepted_fragments,
            SIZE,
            1.0,
            &[font],
            Vec::new(),
        );
        let flattened = flatten_fragments(&accepted_fragments, [1680.0, 900.0], None);
        let visual = |key: &str| {
            &flattened
                .iter()
                .find(|(path, _, _, _)| path == &format!("component-gallery-drop/{key}"))
                .unwrap()
                .2
        };
        assert!(
            flattened
                .iter()
                .all(|(path, _, _, _)| path != "component-gallery-drop/equipment-item-template")
        );
        let target = visual("equipment-zone").bounds;
        let instance = visual(instance_key).bounds;
        let label = visual(label_key);
        assert!(instance.x >= target.x && instance.y >= target.y);
        assert!(instance.x + instance.width <= target.x + target.width);
        assert!(instance.y + instance.height <= target.y + target.height);
        assert!(matches!(
            label.text.as_ref(),
            Some(TextRef::Literal { value }) if value == "Brass compass"
        ));
        assert!(label.bounds.x >= target.x && label.bounds.y >= target.y);
        assert!(label.bounds.x + label.bounds.width <= target.x + target.width);
        assert!(label.bounds.y + label.bounds.height <= target.y + target.height);

        let left = instance.x.floor().max(0.0) as usize;
        let top = instance.y.floor().max(0.0) as usize;
        let right = (instance.x + instance.width).ceil().min(SIZE[0] as f32) as usize;
        let bottom = (instance.y + instance.height).ceil().min(SIZE[1] as f32) as usize;
        let changed_target_pixels = (top..bottom)
            .flat_map(|y| (left..right).map(move |x| (y * SIZE[0] as usize + x) * 4))
            .filter(|offset| before[*offset..*offset + 4] != after[*offset..*offset + 4])
            .count();
        let center_offset = (((top + bottom) / 2) * SIZE[0] as usize + (left + right) / 2) * 4;
        assert!(
            changed_target_pixels > 0,
            "accepted instance must paint inside target: target={target:?} instance={instance:?} before={:?} after={:?}",
            &before[center_offset..center_offset + 4],
            &after[center_offset..center_offset + 4],
        );
    }

    #[test]
    fn component_gallery_fixture_offscreen_captures_remain_responsive() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-component-gallery-viewport-resize");
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        ))
        .expect("component gallery must parse");
        let declaration = document.ir.data_grids[0].clone();
        let handle = |id| neon_ui_schema::UiTextHandle { id, generation: 1 };
        let cells = std::collections::BTreeMap::from([
            (
                "name".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::TextHandle { value: handle(1) },
                    display: handle(101),
                    presentation_override: None,
                },
            ),
            (
                "status".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::Enum {
                        value: "ready".into(),
                    },
                    display: handle(102),
                    presentation_override: None,
                },
            ),
            (
                "owner".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::Bool { value: true },
                    display: handle(103),
                    presentation_override: None,
                },
            ),
            (
                "notes".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::TextHandle { value: handle(4) },
                    display: handle(104),
                    presentation_override: None,
                },
            ),
        ]);
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        fragment.effects.push(UiEffect::DataGridFrame {
            declaration,
            frame: neon_ui_schema::UiDataGridFrame {
                list_revision: Revision(1),
                total_rows: 100,
                first_row: 0,
                window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                    stable_row_key: "asset-1".into(),
                    cells,
                }],
                expected_program_revision: neon_ui_schema::UiProgramRevision {
                    program_id: "component-gallery-responsive-test".into(),
                    revision: Revision(1),
                    schema_version: 1,
                    capabilities: Vec::new(),
                },
            },
        });
        let fragments = HashMap::from([(fragment.fragment_id.clone(), fragment)]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let mut prior_revision = 0;

        for size in [[1920, 1080], [1820, 634], [1668, 900], [1280, 720]] {
            let logical_size = [size[0] as f32, size[1] as f32];
            for path in [
                "component-gallery/component-gallery",
                "component-gallery/gallery-controls",
                "component-gallery/asset-grid",
                "component-gallery/field-pack",
            ] {
                renderer.scroll_offsets.insert(path.into(), [0.0; 2]);
            }
            let pixels = render_renderer_offscreen_for_test(
                &mut renderer,
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                size,
                1.0,
            );
            assert!(renderer.viewport_revision > prior_revision);
            assert_eq!(renderer.plan_viewport_revision, renderer.viewport_revision);
            prior_revision = renderer.viewport_revision;

            let visual = |path: &str| {
                let index = renderer
                    .plan
                    .iter()
                    .position(|node| node.id == path)
                    .unwrap();
                renderer.visual_at(index)
            };
            let responsive_width = logical_size[0].max(2048.0);
            let free = responsive_width - 1856.0;
            assert_eq!(
                visual("component-gallery/component-gallery").bounds,
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: logical_size[0],
                    height: logical_size[1]
                },
            );
            let gallery = visual("component-gallery/gallery-layout").bounds;
            let controls = visual("component-gallery/gallery-controls").bounds;
            let grid = visual("component-gallery/asset-grid").bounds;
            let field_pack = visual("component-gallery/field-pack").bounds;
            assert_eq!(
                gallery,
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: responsive_width,
                    height: logical_size[1]
                }
            );
            assert_eq!(controls.width, 360.0 + free * 0.25);
            assert_eq!(grid.width, 720.0 + free * 0.5);
            assert_eq!(field_pack.width, 720.0 + free * 0.25);
            assert!(controls.width <= 430.0);
            assert!(field_pack.width <= 860.0);
            assert_eq!(grid.x, controls.x + controls.width + 12.0);
            assert_eq!(field_pack.x, grid.x + grid.width + 12.0);
            assert_eq!(field_pack.x + field_pack.width + 16.0, responsive_width);
            assert_eq!(controls.height, (logical_size[1] - 32.0).max(0.0));
            assert_eq!(grid.height, controls.height);
            assert_eq!(field_pack.height, controls.height);
            if logical_size[0] >= 2048.0 {
                for pane in [controls, grid, field_pack] {
                    assert!(pane.x >= 0.0 && pane.x + pane.width <= logical_size[0]);
                }
            }
            assert_eq!(
                renderer.scroll_metrics["component-gallery/component-gallery"].max_offset[0],
                (2048.0 - logical_size[0]).max(0.0)
            );
            let controls_overflow =
                renderer.scroll_metrics["component-gallery/gallery-controls"].max_offset;
            let field_pack_overflow =
                renderer.scroll_metrics["component-gallery/field-pack"].max_offset;
            assert!(controls_overflow[1] > 0.0);
            assert_eq!(field_pack_overflow[0], 0.0);

            let extent = visual("component-gallery/asset-grid/data-grid-content-extent");
            let header = visual("component-gallery/asset-grid/data-grid-header");
            let notes = visual("component-gallery/asset-grid/data-grid-row-asset-1/cell-notes");
            let status =
                visual("component-gallery/asset-grid/data-grid-row-asset-1/cell-status").bounds;
            let header_y = header.bounds.y;
            let expected_content_width = (grid.width - DATA_GRID_SCROLLBAR_GUTTER).max(640.0);
            assert!((extent.bounds.width - expected_content_width).abs() < 0.001);
            assert_eq!(header.bounds.width, extent.bounds.width);
            assert!(
                (notes.bounds.x + notes.bounds.width + 5.0 - (grid.x + extent.bounds.width)).abs()
                    < 0.001
            );

            let feature = visual("component-gallery/feature-toggle").bounds;
            let logical_point = [feature.x + 12.0, feature.y + feature.height * 0.5];
            let physical_point = [logical_point[0] as usize, logical_point[1] as usize];
            let rgba_offset = (physical_point[1] * size[0] as usize + physical_point[0]) * 4;
            assert!(
                pixels[rgba_offset + 3] > 0,
                "feature control must paint after resize"
            );

            let hits = render_hit_ids_with_renderer_for_test(
                &mut renderer,
                &device,
                &queue,
                &fragments,
                size,
            );
            assert_eq!(renderer.plan_viewport_revision, renderer.viewport_revision);
            let hit_id = hits[physical_point[1] * size[0] as usize + physical_point[0]];
            assert_eq!(
                renderer.hit_binding(hit_id).unwrap().node_path,
                "component-gallery/feature-toggle",
            );
            let status_point = [
                (status.x + status.width * 0.5).floor() as usize,
                (status.y + status.height * 0.5).floor() as usize,
            ];
            let status_hit = hits[status_point[1] * size[0] as usize + status_point[0]];
            let status_binding = renderer.hit_binding(status_hit).unwrap();
            assert_eq!(
                status_binding.node_path,
                "component-gallery/asset-grid/data-grid-row-asset-1/cell-status"
            );
            assert_eq!(
                status_binding.data_grid_cell.as_ref().unwrap().column_key,
                "status"
            );

            let vertical_max =
                renderer.scroll_metrics["component-gallery/asset-grid"].max_offset[1];
            assert!(vertical_max > 0.0);
            renderer.scroll_offsets.insert(
                "component-gallery/asset-grid".into(),
                [0.0, vertical_max.min(48.0)],
            );
            let scrolled_pixels = render_renderer_offscreen_for_test(
                &mut renderer,
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                size,
                2.0,
            );
            let header = renderer
                .plan
                .iter()
                .position(|node| node.id == "component-gallery/asset-grid/data-grid-header")
                .map(|index| renderer.visual_at(index))
                .unwrap();
            assert_eq!(header.bounds.y, header_y);
            let header_point = [
                (header.bounds.x + 2.0).floor() as usize,
                (header.bounds.y + 2.0).floor() as usize,
            ];
            assert!(
                scrolled_pixels[(header_point[1] * size[0] as usize + header_point[0]) * 4 + 3] > 0
            );
        }

        renderer.prepare_interaction(&fragments, [1920, 1080], [1280.0, 720.0], 2.0);
        assert_eq!(
            renderer.plan[0].target.bounds,
            UiBounds {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 720.0
            },
        );
        assert_eq!(renderer.viewport_physical_size, [1920, 1080]);
        assert_eq!(renderer.viewport_logical_size, [1280.0, 720.0]);
        assert_eq!(renderer.plan_viewport_revision, renderer.viewport_revision);
    }

    #[test]
    fn component_gallery_fixture_keeps_scrollports_and_interactive_hits_separate() {
        const TARGET_SIZE: [u32; 2] = [1680, 900];
        const TARGET_WIDTH: usize = TARGET_SIZE[0] as usize;
        const TOTAL_ROWS: u64 = 10_000;
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-component-gallery-scroll-hit-map");
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        ))
        .expect("component gallery must parse");
        let mut declaration = document.ir.data_grids[0].clone();
        declaration.columns.last_mut().unwrap().width += 100;
        let max_window_rows = declaration.max_window_rows;
        let row_height = declaration.row_height as f32;
        let overscan = u64::from(declaration.overscan);
        let handle = |id| neon_ui_schema::UiTextHandle { id, generation: 1 };
        let cells = std::collections::BTreeMap::from([
            (
                "name".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::TextHandle { value: handle(1) },
                    display: handle(101),
                    presentation_override: None,
                },
            ),
            (
                "status".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::Enum {
                        value: "ready".into(),
                    },
                    display: handle(102),
                    presentation_override: None,
                },
            ),
            (
                "owner".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::Bool { value: true },
                    display: handle(103),
                    presentation_override: None,
                },
            ),
            (
                "notes".into(),
                neon_ui_schema::UiDataGridCell {
                    value: neon_ui_schema::UiInputValue::TextHandle { value: handle(4) },
                    display: handle(104),
                    presentation_override: None,
                },
            ),
        ]);
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        fragment.effects.push(UiEffect::DataGridFrame {
            declaration,
            frame: neon_ui_schema::UiDataGridFrame {
                list_revision: Revision(1),
                total_rows: TOTAL_ROWS,
                first_row: 0,
                window_rows: (0..max_window_rows)
                    .map(|row| neon_ui_schema::UiDataGridWindowRow {
                        stable_row_key: format!("asset-{}", row + 1),
                        cells: cells.clone(),
                    })
                    .collect(),
                expected_program_revision: neon_ui_schema::UiProgramRevision {
                    program_id: "component-gallery-test".into(),
                    revision: Revision(1),
                    schema_version: 1,
                    capabilities: Vec::new(),
                },
            },
        });
        let fragments = HashMap::from([(fragment.fragment_id.clone(), fragment)]);
        let flattened = flatten_fragments(&fragments, [1680.0, 900.0], None);
        let visual = |path: &str| &flattened.iter().find(|(id, _, _, _)| id == path).unwrap().2;
        let controls = visual("component-gallery/gallery-controls");
        let grid = visual("component-gallery/asset-grid");
        let field_pack = visual("component-gallery/field-pack");
        assert_eq!(
            visual("component-gallery/component-gallery").bounds,
            UiBounds {
                x: 0.0,
                y: 0.0,
                width: 1680.0,
                height: 900.0
            }
        );
        assert_eq!(
            controls.bounds,
            UiBounds {
                x: 16.0,
                y: 16.0,
                width: 408.0,
                height: 868.0
            }
        );
        assert_eq!(
            grid.bounds,
            UiBounds {
                x: 436.0,
                y: 16.0,
                width: 816.0,
                height: 868.0
            }
        );
        assert_eq!(
            field_pack.bounds,
            UiBounds {
                x: 1264.0,
                y: 16.0,
                width: 768.0,
                height: 868.0
            }
        );
        assert_eq!(controls.clip, controls.bounds);
        assert_eq!(grid.clip, grid.bounds);
        assert!(
            controls.scroll,
            "gallery-controls must be a scroll viewport"
        );
        assert!(field_pack.scroll, "field-pack must be a scroll viewport");
        for path in [
            "component-gallery/backpack-compass",
            "component-gallery/backpack-potion",
            "component-gallery/backpack-gem",
            "component-gallery/equipment-zone",
            "component-gallery/crafting-zone",
            "component-gallery/discard-zone",
        ] {
            let item = visual(path);
            assert!(
                item.bounds.width > 0.0 && item.bounds.height > 0.0,
                "{path} must have a composed visual"
            );
        }
        assert_eq!(visual("component-gallery/feature-toggle").bounds.y, 344.0);
        assert_eq!(
            visual("component-gallery/asset-grid/data-grid-row-asset-1/cell-status").bounds,
            UiBounds {
                x: 661.0,
                y: 40.0,
                width: 140.0,
                height: 24.0
            }
        );

        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let start_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            TARGET_SIZE,
            1.0,
        );
        let sampled_bounds = |renderer: &UiWgpuRenderer, path: &str| {
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == path)
                .unwrap();
            renderer.visual_at(index).bounds
        };
        let visible_sample_point = |renderer: &UiWgpuRenderer, path: &str| {
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == path)
                .unwrap();
            let visual = renderer.visual_at(index);
            let visible = intersect_clip(Some(visual.clip), visual.bounds);
            assert!(
                visible.width > 0.0 && visible.height > 0.0,
                "{path} must be visible"
            );
            [
                (visible.x + visible.width * 0.5).floor() as usize,
                (visible.y + visible.height * 0.5).floor() as usize,
            ]
        };
        let visible_grid_cell_path = |renderer: &UiWgpuRenderer, column_key: &str| {
            let suffix = format!("/cell-{column_key}");
            renderer
                .plan
                .iter()
                .enumerate()
                .filter_map(|(index, node)| {
                    if !node
                        .id
                        .starts_with("component-gallery/asset-grid/data-grid-row-")
                        || !node.id.ends_with(&suffix)
                    {
                        return None;
                    }
                    let visual = renderer.visual_at(index);
                    let visible = intersect_clip(Some(visual.clip), visual.bounds);
                    (visible.width > 0.0 && visible.height > 0.0)
                        .then_some((visible.y, node.id.clone()))
                })
                .min_by(|left, right| left.0.total_cmp(&right.0))
                .unwrap()
                .1
        };
        let rgba_at = |pixels: &[u8], point: [usize; 2]| -> [u8; 4] {
            pixels[(point[1] * TARGET_WIDTH + point[0]) * 4..][..4]
                .try_into()
                .unwrap()
        };
        let hit_at = |pixels: &[u32], point: [usize; 2]| pixels[point[1] * TARGET_WIDTH + point[0]];
        let header_path = "component-gallery/asset-grid/data-grid-header";
        let first_header_label_path = "component-gallery/asset-grid/data-grid-header-0";
        let header_at_start = sampled_bounds(&renderer, header_path);
        let first_header_label_at_start = sampled_bounds(&renderer, first_header_label_path);
        let header_pixel_point = |renderer: &UiWgpuRenderer| {
            let index = renderer
                .plan
                .iter()
                .position(|node| node.id == header_path)
                .unwrap();
            let visual = renderer.visual_at(index);
            let visible = intersect_clip(Some(visual.clip), visual.bounds);
            [
                (visible.x + 4.0).floor() as usize,
                (visible.y + visible.height - 2.0).floor() as usize,
            ]
        };
        let feature_point = visible_sample_point(&renderer, "component-gallery/feature-toggle");
        renderer.set_pointer_position([feature_point[0] as f32, feature_point[1] as f32]);
        assert!(
            renderer.hit_id_at_pointer().is_some(),
            "the default-hidden dialog must not block gallery controls"
        );
        let probe = renderer.pointer_probe_snapshot();
        assert_eq!(probe["fallback_hit"]["status"], "hit");
        assert_eq!(
            probe["fallback_hit"]["semantic_node_path"],
            "component-gallery/feature-toggle"
        );
        assert!(probe["fallback_hit"].get("render_id").is_none());
        assert_eq!(
            renderer.scroll_metrics["component-gallery/gallery-controls"].max_offset,
            [0.0, 284.0]
        );
        assert_eq!(
            renderer.scroll_metrics["component-gallery/field-pack"].max_offset,
            [0.0, 0.0]
        );
        assert_eq!(
            renderer.scroll_metrics["component-gallery/asset-grid"].max_offset,
            [56.0, 239_168.0]
        );

        let (item_point, drop_point) = renderer
            .debug_drag_gesture_points("backpack-compass", "equipment-zone")
            .expect("field-pack drag source and drop zone must be composed and visible");
        renderer.set_pointer_position(item_point);
        let item_hit = renderer
            .hit_id_at_pointer()
            .and_then(|hit_id| renderer.hit_binding(hit_id))
            .unwrap();
        assert_eq!(item_hit.node_path, "component-gallery/backpack-compass");
        assert!(renderer.begin_drag_at_pointer(&fragments));
        renderer.set_pointer_position(drop_point);
        assert!(renderer.update_drag_preview());
        let resolved_drop = renderer
            .finish_drag_at_pointer(&fragments)
            .expect("equipment drop zone must resolve the compass drag");
        assert_eq!(resolved_drop.source_key, "backpack-compass");
        assert_eq!(resolved_drop.target_key, "equipment-zone");
        renderer.rollback_local_presentation(&resolved_drop.local_presentation);

        let pixels = render_hit_ids_with_renderer_for_test(
            &mut renderer,
            &device,
            &queue,
            &fragments,
            TARGET_SIZE,
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(&pixels, feature_point))
                .unwrap()
                .node_path,
            "component-gallery/feature-toggle"
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(
                    &pixels,
                    [item_point[0] as usize, item_point[1] as usize]
                ))
                .unwrap()
                .node_path,
            "component-gallery/backpack-compass"
        );
        let status_path = visible_grid_cell_path(&renderer, "status");
        let row_prefix = status_path.strip_suffix("/cell-status").unwrap();
        let owner_path = format!("{row_prefix}/cell-owner");
        let status_point = visible_sample_point(&renderer, &status_path);
        let owner_point = visible_sample_point(&renderer, &owner_path);
        let header_hit_point = [
            status_point[0],
            (header_at_start.y + header_at_start.height - 2.0) as usize,
        ];
        assert_eq!(
            hit_at(&pixels, header_hit_point),
            u32::MAX,
            "row zero must not enter the header hit band at offset zero"
        );
        let status_binding = renderer.hit_binding(hit_at(&pixels, status_point)).unwrap();
        assert_eq!(
            status_binding
                .data_grid_cell
                .as_ref()
                .unwrap()
                .stable_row_key,
            "asset-1"
        );
        assert_eq!(
            status_binding.data_grid_cell.as_ref().unwrap().column_key,
            "status"
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(&pixels, owner_point))
                .unwrap()
                .data_grid_cell
                .as_ref()
                .unwrap()
                .column_key,
            "owner"
        );
        let header_pixel = rgba_at(&start_pixels, header_pixel_point(&renderer));

        renderer
            .scroll_offsets
            .insert("component-gallery/asset-grid".into(), [0.0, 312.0]);
        let mid_pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            TARGET_SIZE,
            2.0,
        );
        let pixels = render_hit_ids_with_renderer_for_test(
            &mut renderer,
            &device,
            &queue,
            &fragments,
            TARGET_SIZE,
        );
        assert_eq!(sampled_bounds(&renderer, header_path).y, header_at_start.y);
        assert_eq!(
            sampled_bounds(&renderer, first_header_label_path).y,
            first_header_label_at_start.y
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(&pixels, feature_point))
                .unwrap()
                .node_path,
            "component-gallery/feature-toggle"
        );
        let status_path = visible_grid_cell_path(&renderer, "status");
        let row_prefix = status_path.strip_suffix("/cell-status").unwrap();
        let owner_path = format!("{row_prefix}/cell-owner");
        let status_point = visible_sample_point(&renderer, &status_path);
        let owner_point = visible_sample_point(&renderer, &owner_path);
        let header_hit_point = [
            status_point[0],
            (header_at_start.y + header_at_start.height - 2.0) as usize,
        ];
        assert_eq!(
            hit_at(&pixels, header_hit_point),
            u32::MAX,
            "mid-scroll body hits must not enter the sticky header"
        );
        assert_eq!(
            rgba_at(&mid_pixels, header_pixel_point(&renderer)),
            header_pixel,
            "mid-scroll body pixels must not replace the sticky header"
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(&pixels, status_point))
                .unwrap()
                .data_grid_cell
                .as_ref()
                .unwrap()
                .column_key,
            "status"
        );
        assert_eq!(
            renderer
                .hit_binding(hit_at(&pixels, owner_point))
                .unwrap()
                .data_grid_cell
                .as_ref()
                .unwrap()
                .column_key,
            "owner"
        );

        let horizontal_offset =
            renderer.scroll_metrics["component-gallery/asset-grid"].max_offset[0];
        renderer.scroll_offsets.insert(
            "component-gallery/asset-grid".into(),
            [horizontal_offset, 312.0],
        );
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            TARGET_SIZE,
            2.5,
        );
        let header_at_horizontal_offset = sampled_bounds(&renderer, header_path);
        let first_header_label_at_horizontal_offset =
            sampled_bounds(&renderer, first_header_label_path);
        assert_eq!(header_at_horizontal_offset.y, header_at_start.y);
        assert_eq!(
            header_at_horizontal_offset.x,
            header_at_start.x - horizontal_offset
        );
        assert_eq!(
            first_header_label_at_horizontal_offset.y,
            first_header_label_at_start.y
        );
        assert_eq!(
            first_header_label_at_horizontal_offset.x,
            first_header_label_at_start.x - horizontal_offset
        );

        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            TARGET_SIZE,
            3.0,
        );
        let controls_background = [29, 42, 40, 255];
        for path in [
            "component-gallery/feature-toggle",
            "component-gallery/mode-radio",
            "component-gallery/exposure-slider",
            "component-gallery/count-drag",
            "component-gallery/mode-combo",
            "component-gallery/mode-dropdown",
            "component-gallery/item-selectable",
            "component-gallery/item-list",
            "component-gallery/gallery-scroll",
            "component-gallery/dialog-toggle",
        ] {
            let indices = renderer
                .plan
                .iter()
                .enumerate()
                .filter_map(|(index, node)| (node.id == path).then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(
                indices.len(),
                1,
                "{path} must have one composed visual without duplication"
            );
            let index = indices[0];
            let visual = renderer.visual_at(index);
            let bounds = visual.bounds;
            let actual_visible = intersect_clip(Some(visual.clip), bounds);
            assert!(actual_visible.width >= 0.0 && actual_visible.height >= 0.0);
            if actual_visible.height > 0.0
                && actual_visible.x >= 0.0
                && actual_visible.y >= 0.0
                && actual_visible.x + actual_visible.width <= TARGET_SIZE[0] as f32
                && actual_visible.y + actual_visible.height <= TARGET_SIZE[1] as f32
            {
                let x = (actual_visible.x + 12.0).floor() as usize;
                let center = [
                    x,
                    (actual_visible.y + actual_visible.height * 0.5).floor() as usize,
                ];
                let bottom = [
                    x,
                    (actual_visible.y + actual_visible.height - 2.0).floor() as usize,
                ];
                assert_ne!(rgba_at(&pixels, center), controls_background);
                assert_ne!(rgba_at(&pixels, bottom), controls_background);
            }
        }

        let grid_offset = renderer.scroll_metrics["component-gallery/asset-grid"].max_offset;
        renderer
            .scroll_offsets
            .insert("component-gallery/asset-grid".into(), grid_offset);
        let mut sequence = 0;
        let requests = renderer.data_grid_window_requests(
            &fragments,
            1,
            Revision(1),
            &mut sequence,
            None,
            false,
        );
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        let viewport_rows = (grid.bounds.height / row_height).ceil() as u64;
        let requested_rows = (viewport_rows + overscan * 2).min(u64::from(request.max_window_rows));
        assert_eq!(request.requested_first_row, TOTAL_ROWS - requested_rows);
        assert_eq!(request.max_window_rows, max_window_rows);
        let tail_row_count = u64::from(request.max_window_rows).min(TOTAL_ROWS);
        let tail_first_row = request.requested_first_row.min(TOTAL_ROWS - tail_row_count);
        let tail_end_row = (tail_first_row + tail_row_count).min(TOTAL_ROWS);

        let mut end_fragment = fragments[&UiFragmentId("component-gallery".into())].clone();
        let UiEffect::DataGridFrame { frame, .. } = end_fragment
            .effects
            .iter_mut()
            .find(|effect| matches!(effect, UiEffect::DataGridFrame { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        frame.first_row = tail_first_row;
        frame.window_rows = (tail_first_row..tail_end_row)
            .map(|row| neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: format!("asset-{row}"),
                cells: cells
                    .iter()
                    .filter(|(key, _)| matches!(key.as_str(), "status" | "owner"))
                    .map(|(key, cell)| (key.clone(), cell.clone()))
                    .collect(),
            })
            .collect();
        end_fragment.revision = Revision(2);
        let end_fragments = HashMap::from([(end_fragment.fragment_id.clone(), end_fragment)]);
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &end_fragments,
            TARGET_SIZE,
            4.0,
        );
        let header_at_tail = sampled_bounds(&renderer, header_path);
        let first_header_label_at_tail = sampled_bounds(&renderer, first_header_label_path);
        assert_eq!(header_at_tail.y, header_at_start.y);
        assert_eq!(first_header_label_at_tail.y, first_header_label_at_start.y);
        assert_eq!(header_at_tail.x, header_at_start.x - grid_offset[0]);
        assert_eq!(
            first_header_label_at_tail.x,
            first_header_label_at_start.x - grid_offset[0]
        );
        let tail_hits = render_hit_ids_with_renderer_for_test(
            &mut renderer,
            &device,
            &queue,
            &end_fragments,
            TARGET_SIZE,
        );
        let tail_status_path = visible_grid_cell_path(&renderer, "status");
        let tail_status_point = visible_sample_point(&renderer, &tail_status_path);
        let tail_header_hit_point = [
            tail_status_point[0],
            (header_at_tail.y + header_at_tail.height - 2.0) as usize,
        ];
        assert_eq!(
            hit_at(&tail_hits, tail_header_hit_point),
            u32::MAX,
            "tail body hits must not enter the sticky header"
        );
        assert_eq!(
            rgba_at(&pixels, header_pixel_point(&renderer)),
            header_pixel,
            "tail body pixels must not replace the sticky header"
        );
        let visible_tail_rows = (tail_first_row..tail_end_row)
            .filter(|row| {
                let path = format!("component-gallery/asset-grid/data-grid-row-asset-{row}");
                let index = renderer
                    .plan
                    .iter()
                    .position(|node| node.id == path)
                    .unwrap();
                let visual = renderer.visual_at(index);
                let visible = intersect_clip(Some(visual.clip), visual.bounds);
                visible.width > 0.0 && visible.height > 0.0
            })
            .collect::<Vec<_>>();
        assert!(
            visible_tail_rows.len() >= 3,
            "tail frame must compose multiple visible rows"
        );
        for row in [
            visible_tail_rows[0],
            visible_tail_rows[visible_tail_rows.len() / 2],
            *visible_tail_rows.last().unwrap(),
        ] {
            let path = format!("component-gallery/asset-grid/data-grid-row-asset-{row}");
            let point = visible_sample_point(&renderer, &path);
            assert!(
                rgba_at(&pixels, point)[3] > 0,
                "tail row {row} must render at its translated viewport position"
            );
        }

        let paths = [
            format!(
                "component-gallery/asset-grid/data-grid-row-asset-{}/cell-status",
                TOTAL_ROWS - 1
            ),
            format!(
                "component-gallery/asset-grid/data-grid-row-asset-{}/cell-owner",
                TOTAL_ROWS - 1
            ),
        ];
        let sampled_snapshot = |renderer: &UiWgpuRenderer| {
            paths.each_ref().map(|path| {
                let index = renderer
                    .plan
                    .iter()
                    .position(|node| node.id == *path)
                    .unwrap();
                let visual = renderer.visual_at(index);
                (path.clone(), visual.bounds, visual.clip)
            })
        };
        let semantic_hits = |renderer: &mut UiWgpuRenderer| {
            paths.each_ref().map(|path| {
                let index = renderer
                    .plan
                    .iter()
                    .position(|node| node.id == *path)
                    .unwrap();
                let visual = renderer.visual_at(index);
                let visible = intersect_clip(Some(visual.clip), visual.bounds);
                renderer.set_pointer_position([
                    visible.x + visible.width * 0.5,
                    visible.y + visible.height * 0.5,
                ]);
                let binding = renderer
                    .hit_id_at_pointer()
                    .and_then(|hit_id| renderer.hit_binding(hit_id))
                    .unwrap_or_else(|| panic!("{path} must remain semantically hittable"));
                (binding.node_path, binding.data_grid_cell)
            })
        };
        let drawn_visuals = sampled_snapshot(&renderer);
        let drawn_hits = semantic_hits(&mut renderer);

        renderer.prepare_interaction(
            &end_fragments,
            TARGET_SIZE,
            [TARGET_SIZE[0] as f32, TARGET_SIZE[1] as f32],
            4.0,
        );

        assert_eq!(sampled_snapshot(&renderer), drawn_visuals);
        assert_eq!(semantic_hits(&mut renderer), drawn_hits);
        let tail_snapshot = sampled_snapshot(&renderer);
        let tail = &tail_snapshot[1];
        let visible = intersect_clip(Some(tail.2), tail.1);
        assert!(
            visible.width > 0.0 && visible.height > 0.0,
            "DataGrid tail cell must remain visible after interaction preparation"
        );
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
            world_depth: None,
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
        let nodes = flatten_fragments(&fragments, [100.0, 100.0], None);
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
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
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
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
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
            scroll: false,
            declared_scroll_offset: [0.0; 2],
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
            scroll: false,
            declared_scroll_offset: [0.0; 2],
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
