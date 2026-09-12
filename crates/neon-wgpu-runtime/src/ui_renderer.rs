//! Minimal GPU UI composition pass adapted from Neon2's instanced panel renderer.
//! It deliberately consumes only Neon3's public UI schema, not old ECS state.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use neon_protocol::{AssetBytes, AssetRef, UiImageSource, UiImageTextureRef, UiImageTextureRegion};
use neon_ui_schema::{
    RenderSurfaceRef, TextRef, UiAlignItems, UiBounds, UiClipPolicy, UiControlPresentation,
    UiDataGridCellTarget, UiDataGridWindowRequest, UiDragAxis, UiDragBinding, UiDragBoundary,
    UiDropPlacement, UiEasing, UiFragment, UiFragmentRevision, UiImageFit, UiIntent, UiJustifyContent,
    UiLayout, UiLayoutMode, UiNode, UiNodeKind, UiSemanticPayloadValue, UiStyle,
    UiStylePatch as SchemaStylePatch, UiTransition, UiTransitionState, UiControlSkin,
    UiSkinSlot, UiSkinSlotKind, UiVisualState, UiSkinPresentation, UiMaterialRef, UiShaderPackage,
};
use serde_json::{Value, json};

const SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
struct ShaderEvent { event_id: u32, payload: vec4<f32> }
struct ShaderEventBuffer { counter: atomic<u32>, events: array<ShaderEvent> }
@group(0) @binding(1) var<storage, read_write> shader_events: ShaderEventBuffer;
fn emit_shader_event(event_id: u32, payload: vec4<f32>) { let slot = atomicAdd(&shader_events.counter, 1u); if (slot < 256u) { shader_events.events[slot].event_id = event_id; shader_events.events[slot].payload = payload; } }

fn animation_progress(animation: vec4<f32>) -> f32 {
    if (animation.w == 0.0 || animation.y <= 0.0) { return 1.0; }
    let t = clamp((view.time_seconds - animation.x) / animation.y, 0.0, 1.0);
    if (animation.z == 1.0) { return t * t; }
    if (animation.z == 2.0) { return 1.0 - (1.0 - t) * (1.0 - t); }
    if (animation.z == 3.0) {
        return select(2.0 * t * t, 1.0 - pow(-2.0 * t + 2.0, 2.0) / 2.0, t >= 0.5);
    }
    return t;
}

fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
    let low = value / 12.92;
    let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(low, high, value > vec3<f32>(0.04045));
}
fn hash2(p: vec2<f32>) -> f32 {
    var q = vec3<f32>(p, 17.31);
    q = fract(q * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i);
    let b = hash2(i + vec2<f32>(1.0, 0.0));
    let c = hash2(i + vec2<f32>(0.0, 1.0));
    let d = hash2(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn liquid_glass(color: vec4<f32>, pixel: vec2<f32>, local: vec2<f32>) -> vec4<f32> {
    // Windows Composition supplies the real backdrop blur and tint. The panel
    // color passes through unchanged; the previous white rim highlight and
    // transmission tint produced an unwanted light border around cut-corner
    // shells.
    return color;
}
fn outside_clip(pixel: vec2<f32>, clip: vec4<f32>, radius: f32) -> bool {
    if (pixel.x < clip.x || pixel.y < clip.y || pixel.x > clip.z || pixel.y > clip.w) { return true; }
    if (radius <= 0.0) { return false; }
    let size = clip.zw - clip.xy; let r = min(radius, min(size.x, size.y) * 0.5);
    let point = pixel - (clip.xy + size * 0.5); let extent = max(size * 0.5 - vec2<f32>(r), vec2<f32>(0.0));
    return length(max(abs(point) - extent, vec2<f32>(0.0))) > r;
}

fn outside_cut(local: vec2<f32>, size: vec2<f32>, cut: vec4<f32>) -> bool {
    // cut = [bl, br, tr, tl] logical pixels removed from each corner.
    let p = local * size;
    let bl = min(cut.x, min(size.x, size.y));
    let br = min(cut.y, min(size.x, size.y));
    let tr = min(cut.z, min(size.x, size.y));
    let tl = min(cut.w, min(size.x, size.y));
    if (bl > 0.0 && p.x < bl && p.y < bl && p.x + p.y < bl) { return true; }
    let rx = size.x - p.x;
    if (br > 0.0 && rx < br && p.y < br && rx + p.y < br) { return true; }
    let ty = size.y - p.y;
    if (tr > 0.0 && rx < tr && ty < tr && rx + ty < tr) { return true; }
    if (tl > 0.0 && p.x < tl && p.y < tl && p.x + p.y < tl) { return true; }
    return false;
}

struct VsIn {
    @location(0) rect: vec4<f32>,
    @location(1) fill: vec4<f32>,
    @location(2) border: vec4<f32>,
    @location(3) params: vec4<f32>,
    @location(4) clip: vec4<f32>,
    @location(5) depth: f32,
    @location(6) from_rect: vec4<f32>,
    @location(7) from_fill: vec4<f32>,
    @location(8) from_border: vec4<f32>,
    @location(9) from_params: vec4<f32>,
    @location(10) animation: vec4<f32>,
    @location(11) cut: vec4<f32>,
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
    @location(7) cut: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let local = corners[vertex_index];
    let t = animation_progress(input.animation);
    let rect = mix(input.from_rect, input.rect, t);
    let fill = mix(input.from_fill, input.fill, t);
    let border = mix(input.from_border, input.border, t);
    let params = mix(input.from_params, input.params, t);
    let pixel = rect.xy + local * rect.zw;
    var output: VsOut;
    output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, input.depth, 1.0);
    output.local = local;
    output.size = rect.zw;
    output.fill = fill;
    output.border = border;
    output.params = params;
    output.clip = input.clip;
    output.pixel = pixel;
    output.cut = input.cut;
    return output;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
    if (outside_cut(input.local, input.size, input.cut)) { discard; }
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
        let alpha = color.a * input.params.z * shape_alpha;
        // A transparent structural container must not populate the color depth
        // attachment. Otherwise its near depth rejects all visible World UI
        // children while contributing no color itself.
        if (alpha <= 0.001) { discard; }
        let glass = liquid_glass(color, input.pixel, input.local);
        return vec4<f32>(select(srgb_to_linear(glass.rgb), glass.rgb, view.color_mode == 1u) * alpha, alpha);
    }
    let radius = min(input.params.y, min(input.size.x, input.size.y) * 0.5);
    let point = input.local * input.size - input.size * 0.5;
    let extent = max(input.size * 0.5 - vec2<f32>(radius), vec2<f32>(0.0));
    let corner_distance = length(max(abs(point) - extent, vec2<f32>(0.0))) - radius;
    let shape_alpha = 1.0 - smoothstep(0.0, 1.0, corner_distance);
    let edge_distance = select(min(extent.x - abs(point.x), extent.y - abs(point.y)), -corner_distance, radius > 0.0);
    let border_alpha = 1.0 - smoothstep(input.params.x - 1.0, input.params.x + 1.0, edge_distance);
    let color = mix(input.fill, input.border, border_alpha);
    let alpha = color.a * input.params.z * shape_alpha;
    if (alpha <= 0.001) { discard; }
    let glass = liquid_glass(color, input.pixel, input.local);
    return vec4<f32>(select(srgb_to_linear(glass.rgb), glass.rgb, view.color_mode == 1u) * alpha, alpha);
}
"#;

const HIT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
fn animation_progress(animation: vec4<f32>) -> f32 { if (animation.w == 0.0 || animation.y <= 0.0) { return 1.0; } let t=clamp((view.time_seconds-animation.x)/animation.y,0.0,1.0); if(animation.z==1.0){return t*t;} if(animation.z==2.0){return 1.0-(1.0-t)*(1.0-t);} if(animation.z==3.0){return select(2.0*t*t,1.0-pow(-2.0*t+2.0,2.0)/2.0,t>=0.5);} return t; }
fn outside_clip(pixel: vec2<f32>, clip: vec4<f32>, radius: f32) -> bool { if (pixel.x < clip.x || pixel.y < clip.y || pixel.x > clip.z || pixel.y > clip.w) { return true; } if (radius <= 0.0) { return false; } let size=clip.zw-clip.xy; let r=min(radius,min(size.x,size.y)*0.5); let point=pixel-(clip.xy+size*0.5); let extent=max(size*0.5-vec2<f32>(r),vec2<f32>(0.0)); return length(max(abs(point)-extent,vec2<f32>(0.0)))>r; }
fn outside_cut(local: vec2<f32>, size: vec2<f32>, cut: vec4<f32>) -> bool { let p = local * size; let bl = min(cut.x, min(size.x, size.y)); let br = min(cut.y, min(size.x, size.y)); let tr = min(cut.z, min(size.x, size.y)); let tl = min(cut.w, min(size.x, size.y)); if (bl > 0.0 && p.x < bl && p.y < bl && p.x + p.y < bl) { return true; } let rx = size.x - p.x; if (br > 0.0 && rx < br && p.y < br && rx + p.y < br) { return true; } let ty = size.y - p.y; if (tr > 0.0 && rx < tr && ty < tr && rx + ty < tr) { return true; } if (tl > 0.0 && p.x < tl && p.y < tl && p.x + p.y < tl) { return true; } return false; }
struct VsIn { @location(0) rect: vec4<f32>, @location(1) params: vec4<f32>, @location(2) hit_id: u32, @location(3) clip: vec4<f32>, @location(4) cut: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) size: vec2<f32>, @location(2) params: vec4<f32>, @location(3) @interpolate(flat) hit_id: u32, @location(4) clip: vec4<f32>, @location(5) pixel: vec2<f32>, @location(6) cut: vec4<f32> }
@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[vertex_index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
 output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0); output.local = local; output.size = input.rect.zw; output.params = input.params; output.hit_id = input.hit_id; output.clip = input.clip; output.pixel = pixel; output.cut = input.cut; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) u32 {
   if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
  if (outside_cut(input.local, input.size, input.cut)) { discard; }
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

/// GPU→CPU shader event ring buffer. Layout: 4-byte atomic counter +
/// 12-byte padding + 256 × 32-byte `ShaderEvent` slots (u32 id + vec4 payload,
/// aligned to 16). Rounded up to 256-byte MAP_READ alignment.
const SHADER_EVENT_BUFFER_SIZE: u64 = 16384;
const SHADER_EVENT_CAPACITY: usize = 256;

// Package sources provide only `fn material(input: MaterialInput) -> vec4<f32>`
// plus private helper functions. The renderer owns this wrapper, its vertex
// ABI, clipping, blend contract, and all GPU bindings.
const MATERIAL_SHADER_PREFIX: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
struct ShaderEvent { event_id: u32, payload: vec4<f32> }
struct ShaderEventBuffer { counter: atomic<u32>, events: array<ShaderEvent> }
@group(0) @binding(1) var<storage, read_write> shader_events: ShaderEventBuffer;
fn emit_shader_event(event_id: u32, payload: vec4<f32>) { let slot = atomicAdd(&shader_events.counter, 1u); if (slot < 256u) { shader_events.events[slot].event_id = event_id; shader_events.events[slot].payload = payload; } }
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> { let low=value/12.92; let high=pow((value+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)); return select(low,high,value>vec3<f32>(0.04045)); }
fn outside_clip(pixel: vec2<f32>, clip: vec4<f32>, radius: f32) -> bool { if (pixel.x < clip.x || pixel.y < clip.y || pixel.x > clip.z || pixel.y > clip.w) { return true; } if (radius <= 0.0) { return false; } let size=clip.zw-clip.xy; let r=min(radius,min(size.x,size.y)*0.5); let point=pixel-(clip.xy+size*0.5); let extent=max(size*0.5-vec2<f32>(r),vec2<f32>(0.0)); return length(max(abs(point)-extent,vec2<f32>(0.0)))>r; }
fn outside_cut(local: vec2<f32>, size: vec2<f32>, cut: vec4<f32>) -> bool { let p=local*size; let bl=min(cut.x,min(size.x,size.y)); let br=min(cut.y,min(size.x,size.y)); let tr=min(cut.z,min(size.x,size.y)); let tl=min(cut.w,min(size.x,size.y)); if(bl>0.0&&p.x<bl&&p.y<bl&&p.x+p.y<bl){return true;} let rx=size.x-p.x; if(br>0.0&&rx<br&&p.y<br&&rx+p.y<br){return true;} let ty=size.y-p.y; if(tr>0.0&&rx<tr&&ty<tr&&rx+ty<tr){return true;} if(tl>0.0&&p.x<tl&&p.y<tl&&p.x+p.y<tl){return true;} return false; }
struct VsIn { @location(0) rect: vec4<f32>, @location(1) fill: vec4<f32>, @location(2) border: vec4<f32>, @location(3) params: vec4<f32>, @location(4) clip: vec4<f32>, @location(5) depth: f32, @location(6) from_rect: vec4<f32>, @location(7) from_fill: vec4<f32>, @location(8) from_border: vec4<f32>, @location(9) from_params: vec4<f32>, @location(10) animation: vec4<f32>, @location(11) cut: vec4<f32> }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) size: vec2<f32>, @location(2) fill: vec4<f32>, @location(3) border: vec4<f32>, @location(4) params: vec4<f32>, @location(5) clip: vec4<f32>, @location(6) pixel: vec2<f32>, @location(7) cut: vec4<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut { var corners=array<vec2<f32>,6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0)); let local=corners[index]; let pixel=input.rect.xy+local*input.rect.zw; var output:VsOut; output.position=vec4<f32>(pixel.x/view.viewport.x*2.0-1.0,1.0-pixel.y/view.viewport.y*2.0,input.depth,1.0); output.local=local; output.size=input.rect.zw; output.fill=input.fill; output.border=input.border; output.params=input.params; output.clip=input.clip; output.pixel=pixel; output.cut=input.cut; return output; }
struct MaterialInput { local_position: vec2<f32>, bounds: vec4<f32>, base_color: vec4<f32>, border_color: vec4<f32>, time_seconds: f32, opacity: f32, geometry_edge: f32, state_flags: u32 }
"#;
const MATERIAL_SHADER_SUFFIX: &str = r#"
@fragment fn fs_material(input: VsOut) -> @location(0) vec4<f32> { if(outside_clip(input.pixel,input.clip,input.params.w)||outside_cut(input.local,input.size,input.cut)){discard;} let edge=min(min(input.local.x,1.0-input.local.x),min(input.local.y,1.0-input.local.y)); let color=material(MaterialInput(input.local,vec4<f32>(input.pixel,input.size),input.fill,input.border,view.time_seconds,input.params.z,edge,0u)); let alpha=clamp(color.a,0.0,1.0); if(alpha<=0.001){discard;} let rgb=select(srgb_to_linear(color.rgb),color.rgb,view.color_mode==1u); return vec4<f32>(rgb*alpha,alpha); }
"#;

const DEPTH_SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
fn animation_progress(animation: vec4<f32>) -> f32 { if (animation.w == 0.0 || animation.y <= 0.0) { return 1.0; } let t=clamp((view.time_seconds-animation.x)/animation.y,0.0,1.0); if(animation.z==1.0){return t*t;} if(animation.z==2.0){return 1.0-(1.0-t)*(1.0-t);} if(animation.z==3.0){return select(2.0*t*t,1.0-pow(-2.0*t+2.0,2.0)/2.0,t>=0.5);} return t; }
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
    @location(6) from_rect: vec4<f32>,
    @location(7) from_fill: vec4<f32>,
    @location(8) from_border: vec4<f32>,
    @location(9) from_params: vec4<f32>,
    @location(10) animation: vec4<f32>,
}
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) clip: vec4<f32>,
    @location(1) params: vec4<f32>,
    @location(2) pixel: vec2<f32>,
    @location(3) depth: f32,
    @location(4) local: vec2<f32>,
    @location(5) size: vec2<f32>,
}
@vertex fn vs_main(@builtin(vertex_index) vertex_index: u32, input: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0)
    );
    let local = corners[vertex_index]; let t = animation_progress(input.animation); let rect=mix(input.from_rect,input.rect,t); let params=mix(input.from_params,input.params,t); let pixel = rect.xy + local * rect.zw;
    var output: VsOut;
    output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, 0.0, 1.0);
    output.clip = input.clip;
    output.params = params;
    output.pixel = pixel;
    output.depth = input.depth;
    output.local = local;
    output.size = rect.zw;
    return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) f32 {
    if (outside_clip(input.pixel, input.clip, input.params.w)) { discard; }
    // Zero is a valid topmost depth for screen UI. The external target is a
    // color target, so screen groups can overwrite world depth at overlap.
    if (input.depth < 0.0) { discard; }
    // Match the color pass's visible-region test so depth coverage aligns with
    // the antialiased color edge. The color pass fades the shape over a 1px
    // smoothstep; a hard clip edge here would sit 1px inside that fade and
    // leave a ring of depth=0 (always-visible) pixels that the host never
    // occludes, which shows up as white fringes along every panel edge.
    var shape_alpha: f32;
    if (input.params.y < 0.0) {
        let cut = min(-input.params.y, input.size.x * 0.25);
        let point = input.local * input.size;
        let left = cut * (1.0 - input.local.y);
        let right = input.size.x - cut * input.local.y;
        let edge_distance = min(
            min(point.x - left, right - point.x),
            min(point.y, input.size.y - point.y)
        );
        shape_alpha = smoothstep(-1.0, 1.0, edge_distance);
    } else {
        let radius = min(input.params.y, min(input.size.x, input.size.y) * 0.5);
        let point = input.local * input.size - input.size * 0.5;
        let extent = max(input.size * 0.5 - vec2<f32>(radius), vec2<f32>(0.0));
        let corner_distance = length(max(abs(point) - extent, vec2<f32>(0.0))) - radius;
        shape_alpha = 1.0 - smoothstep(0.0, 1.0, corner_distance);
    }
    if (shape_alpha <= 0.001) { discard; }
    return input.depth;
}
"#;

const IMAGE_SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
 let low = value / 12.92; let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)); return select(low, high, value > vec3<f32>(0.04045));
}
struct VsIn { @location(0) rect: vec4<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) uv: vec4<f32>, @location(4) depth: f32, @location(5) source_insets: vec4<f32>, @location(6) target_insets: vec4<f32>, @location(7) mode: u32, @location(8) fill_center: u32 }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) tint: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32>, @location(4) uv: vec4<f32>, @location(5) source_insets: vec4<f32>, @location(6) target_insets: vec4<f32>, @location(7) @interpolate(flat) mode: u32, @location(8) @interpolate(flat) fill_center: u32, @location(9) rect_size: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
    output.position = vec4<f32>(pixel.x / view.viewport.x * 2.0 - 1.0, 1.0 - pixel.y / view.viewport.y * 2.0, input.depth, 1.0); output.local = local; output.tint = input.tint; output.clip = input.clip; output.pixel = pixel; output.uv = input.uv; output.source_insets = input.source_insets; output.target_insets = input.target_insets; output.mode = input.mode; output.fill_center = input.fill_center; output.rect_size = input.rect.zw; return output;
 }
fn compressed_insets(size: f32, left: f32, right: f32) -> vec2<f32> {
  let total = left + right;
  if total > size && total > 0.0 {
    return vec2<f32>(left, right) * (size / total);
  }
  return vec2<f32>(left, right);
}
fn map_axis(distance: f32, size: f32, source_size: f32, target_edges: vec2<f32>, source_edges: vec2<f32>, mode: u32) -> f32 {
  let target_span = compressed_insets(size, target_edges.x, target_edges.y);
  let source = vec2<f32>(source_edges.x, source_edges.y);
  let middle_source = max(source_size - source.x - source.y, 0.0);
  let middle_target = max(size - target_span.x - target_span.y, 0.0);
  if distance <= target_span.x || middle_target <= 0.0 {
    return select(0.0, (distance / max(target_span.x, 0.0001)) * source.x, target_span.x > 0.0);
  }
  if distance >= size - target_span.y {
    return source_size - source.y + ((distance - (size - target_span.y)) / max(target_span.y, 0.0001)) * source.y;
  }
  if middle_source <= 0.0 {
    return source.x;
  }
  let middle_distance = distance - target_span.x;
  if mode == 0u {
    return source.x + middle_distance / middle_target * middle_source;
  }
  let tile_index = floor(middle_distance / middle_source);
  let tile_offset = middle_distance - tile_index * middle_source;
  if mode == 2u && (u32(tile_index) & 1u) == 1u {
    return source.x + middle_source - tile_offset;
  }
  return source.x + tile_offset;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
    // External images are atlas entries with integer regions. Sample the
    // resolved texel directly instead of relying on normalized filtering at
    // the atlas boundary; this keeps the first/last texel visible for both
    // tiny probes and cropped engine images.
    let atlas_dims = vec2<i32>(textureDimensions(image_texture));
     let source_size = input.uv.zw * vec2<f32>(atlas_dims) + vec2<f32>(1.0);
     let distance = input.local * input.rect_size;
     let x_edges = compressed_insets(input.rect_size.x, input.target_insets.x, input.target_insets.z);
     let y_edges = compressed_insets(input.rect_size.y, input.target_insets.y, input.target_insets.w);
     let source_x = map_axis(distance.x, input.rect_size.x, source_size.x, vec2<f32>(x_edges.x, x_edges.y), vec2<f32>(input.source_insets.x, input.source_insets.z), input.mode);
     let source_y = map_axis(distance.y, input.rect_size.y, source_size.y, vec2<f32>(y_edges.x, y_edges.y), vec2<f32>(input.source_insets.y, input.source_insets.w), input.mode);
     let in_center = distance.x >= x_edges.x && distance.x <= input.rect_size.x - x_edges.y && distance.y >= y_edges.x && distance.y <= input.rect_size.y - y_edges.y;
     if (!in_center || input.fill_center == 1u) {
       // Continue with the sampled texel. The condition is intentionally
       // branch-shaped so transparent center fill remains a cheap discard.
     } else {
       discard;
     }
     let atlas_position = input.uv.xy * vec2<f32>(atlas_dims) - vec2<f32>(0.5) + vec2<f32>(source_x, source_y);
     let texel = clamp(
         vec2<i32>(atlas_position),
         vec2<i32>(0),
         atlas_dims - vec2<i32>(1),
     );
    let sample = textureLoad(image_texture, texel, 0);
  let alpha = sample.a * input.tint.a;
  if (alpha <= 0.001) { discard; }
  let tint = select(srgb_to_linear(input.tint.rgb), input.tint.rgb, view.color_mode == 1u);
     return vec4<f32>(sample.rgb * tint * alpha, alpha);
}
"#;

const TEXT_SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(1) @binding(1) var glyph_sampler: sampler;
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> {
 let low = value / 12.92; let high = pow((value + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4)); return select(low, high, value > vec3<f32>(0.04045));
}
struct VsIn { @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) uv: vec4<f32>, @location(4) depth: f32 }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) local: vec2<f32>, @location(1) color: vec4<f32>, @location(2) clip: vec4<f32>, @location(3) pixel: vec2<f32>, @location(4) uv: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners = array<vec2<f32>, 6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local = corners[index]; let pixel = input.rect.xy + local * input.rect.zw; var output: VsOut;
  output.position=vec4<f32>(pixel.x/view.viewport.x*2.0-1.0,1.0-pixel.y/view.viewport.y*2.0,input.depth,1.0); output.local=local; output.color=input.color; output.clip=input.clip; output.pixel=pixel; output.uv=input.uv.xy + local * input.uv.zw; return output;
}
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
 if (input.pixel.x < input.clip.x || input.pixel.y < input.clip.y || input.pixel.x > input.clip.z || input.pixel.y > input.clip.w) { discard; }
 let coverage = textureSample(glyph_atlas, glyph_sampler, input.uv).a;
 if (coverage <= 0.001) { discard; }
  let color = select(srgb_to_linear(input.color.rgb), input.color.rgb, view.color_mode == 1u);
   let alpha = input.color.a * coverage;
   return vec4<f32>(color * alpha, alpha);
}
"#;

const CANVAS_SHADER: &str = r#"
struct View { viewport: vec2<f32>, color_mode: u32, time_seconds: f32, extras: array<vec4<f32>, 10> }
@group(0) @binding(0) var<uniform> view: View;
struct VsIn { @location(0) start: vec2<f32>, @location(1) end: vec2<f32>, @location(2) color: vec4<f32>, @location(3) width: f32, @location(4) kind: u32, @location(5) clip: vec4<f32>, @location(6) depth: f32 }
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) pixel: vec2<f32>, @location(1) start: vec2<f32>, @location(2) end: vec2<f32>, @location(3) color: vec4<f32>, @location(4) width: f32, @location(5) @interpolate(flat) kind: u32, @location(6) clip: vec4<f32> }
fn srgb_to_linear(value: vec3<f32>) -> vec3<f32> { let low=value/12.92; let high=pow((value+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)); return select(low,high,value>vec3<f32>(0.04045)); }
@vertex fn vs_main(@builtin(vertex_index) index: u32, input: VsIn) -> VsOut {
 var corners=array<vec2<f32>,6>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(1.0,1.0));
 let local=corners[index]; let delta=input.end-input.start; let length=max(length(delta),0.0001); let direction=delta/length; let normal=vec2<f32>(-direction.y,direction.x); let half=input.width*0.5;
 let point=select(input.start + direction*(local.x*length) + normal*((local.y*2.0-1.0)*half), input.start + (local-vec2<f32>(0.5))*input.width, input.kind==0u);
 var output:VsOut; output.position=vec4<f32>(point.x/view.viewport.x*2.0-1.0,1.0-point.y/view.viewport.y*2.0,input.depth,1.0); output.pixel=point; output.start=input.start; output.end=input.end; output.color=input.color; output.width=input.width; output.kind=input.kind; output.clip=input.clip; return output;
}
@fragment fn fs_main(input:VsOut)->@location(0) vec4<f32> {
 if(input.pixel.x<input.clip.x||input.pixel.y<input.clip.y||input.pixel.x>input.clip.z||input.pixel.y>input.clip.w){discard;}
 if(input.kind==0u && length(input.pixel-input.start)>input.width*0.5){discard;}
 if(input.color.a<=0.001){discard;}
  let alpha = input.color.a;
  return vec4<f32>(select(srgb_to_linear(input.color.rgb),input.color.rgb,view.color_mode==1u) * alpha, alpha);
}
"#;

const BUILTIN_UI_FONT: &[u8] = include_bytes!("../assets/fonts/SarasaUiSC-Light.ttf");

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct UiInstance {
    rect: [f32; 4],
    fill: [f32; 4],
    border: [f32; 4],
    params: [f32; 4],
    clip: [f32; 4],
    /// Normalized color-pass depth (0.0 = near/always-on-top, 1.0 = far).
    depth: f32,
    paint_group_id: u32,
    from_rect: [f32; 4],
    from_fill: [f32; 4],
    from_border: [f32; 4],
    from_params: [f32; 4],
    /// start seconds, duration seconds, easing code, enabled.
    animation: [f32; 4],
    /// Cut-corner panel style in logical pixels: [bl, br, tr, tl]. Zero when
    /// the node has no cut geometry. The fragment shader clips to the same
    /// polygon used by the hit pass.
    cut: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiView {
    viewport: [f32; 2],
    color_mode: u32,
    time_seconds: f32,
    extras: [[f32; 4]; 10],
}

/// Generic per-view extra uniform data (10 x vec4 = 160 bytes).
/// Written by the `wgpu.ui.set_view_extras` RPC handler and read by the
/// render loop. The runtime never interprets the content — shaders are
/// free to use the 40 f32 slots for audio spectrum, sensor data, IRC
/// counters, or any other per-frame data.
static GLOBAL_VIEW_EXTRAS: std::sync::Mutex<[[f32; 4]; 10]> =
    std::sync::Mutex::new([[0.0; 4]; 10]);

pub(crate) fn set_global_view_extras(extras: [[f32; 4]; 10]) {
    if let Ok(mut guard) = GLOBAL_VIEW_EXTRAS.lock() {
        *guard = extras;
    }
}

pub(crate) fn get_global_view_extras() -> [[f32; 4]; 10] {
    GLOBAL_VIEW_EXTRAS.lock().map(|g| *g).unwrap_or([[0.0; 4]; 10])
}


#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiHitInstance {
    rect: [f32; 4],
    params: [f32; 4],
    hit_id: u32,
    _pad: [u32; 3],
    clip: [f32; 4],
    /// Cut-corner panel style in logical pixels: [bl, br, tr, tl].
    cut: [f32; 4],
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
    paint_group_id: u32,
    source_insets: [f32; 4],
    target_insets: [f32; 4],
    mode: u32,
    fill_center: u32,
    _padding: [u32; 2],
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
    paint_group_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct UiCanvasInstance {
    start: [f32; 2],
    end: [f32; 2],
    color: [f32; 4],
    width: f32,
    kind: u32,
    clip: [f32; 4],
    depth: f32,
    paint_group_id: u32,
}

struct ResidentImage {
    slot: u32,
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
    size: [u32; 2],
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

/// Logical layout geometry for a flattened node, independent of camera
/// distance and final projection. This is the single source of truth for
/// text measurement, wrapping, and layout decisions.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LogicalLayoutBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    content_x: f32,
    content_y: f32,
    content_width: f32,
    content_height: f32,
    clip: Option<UiBounds>,
}

/// Final visual projection applied after logical layout completes. Screen UI
/// uses a fixed identity transform; WorldUi applies its root uniform scale
/// around the projected bottom-center origin.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FinalVisualTransform {
    origin: [f32; 2],
    uniform_scale: f32,
    world_depth: Option<f32>,
}

impl FinalVisualTransform {
    fn identity() -> Self {
        Self {
            origin: [0.0, 0.0],
            uniform_scale: 1.0,
            world_depth: None,
        }
    }

    fn is_identity(&self) -> bool {
        self.origin == [0.0, 0.0] && self.uniform_scale == 1.0 && self.world_depth.is_none()
    }

    fn project_point(&self, point: [f32; 2]) -> [f32; 2] {
        [
            self.origin[0] + (point[0] - self.origin[0]) * self.uniform_scale,
            self.origin[1] + (point[1] - self.origin[1]) * self.uniform_scale,
        ]
    }

    fn project_bounds(&self, bounds: UiBounds) -> UiBounds {
        let top_left = self.project_point([bounds.x, bounds.y]);
        UiBounds {
            x: top_left[0],
            y: top_left[1],
            width: bounds.width * self.uniform_scale,
            height: bounds.height * self.uniform_scale,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct UiVisual {
    /// Final visual bounds after the complete logical layout and any WorldUi
    /// root projection. Screen UI and WorldUi both render from this.
    bounds: UiBounds,
    /// Logical pre-projection geometry. Text measurement and wrapping always
    /// use `logical_bounds`, never the projected `bounds`, so camera distance
    /// cannot feed back into re-layout.
    logical_bounds: LogicalLayoutBox,
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
    /// Normalized occlusion depth inherited from a projected world panel.
    /// Screen UI has `None` and is omitted from the external depth pass.
    world_depth: Option<f32>,
    /// Uniform distance-based scale inherited from a projected world panel.
    /// `None` for screen UI; used to scale glyphs with their panel subtree.
    world_scale: Option<f32>,
    paint_group_id: u32,
}

#[derive(Clone, Debug)]
struct ActiveTransition {
    from: UiVisual,
    target: UiVisual,
    started_at_seconds: f32,
    transition: UiTransition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAnimationStatus {
    Pending,
    Running,
    Completed,
    Cancelled,
    Superseded,
    Rejected,
}

#[derive(Clone, Debug, PartialEq)]
struct UiAnimationSpec {
    motion_key: Option<String>,
    delay_ms: u32,
    duration_ms: u32,
    easing: UiEasing,
    from: UiTransitionState,
}

#[derive(Clone, Debug, PartialEq)]
struct UiAnimationInstance {
    node_path: String,
    fragment_revision: neon_protocol::Revision,
    started_at_seconds: f32,
    spec: UiAnimationSpec,
    status: UiAnimationStatus,
}

fn animation_instance_from_active(
    node_path: &str,
    active: &ActiveTransition,
    status: UiAnimationStatus,
) -> UiAnimationInstance {
    UiAnimationInstance {
        node_path: node_path.to_owned(),
        fragment_revision: neon_protocol::Revision(0),
        started_at_seconds: active.started_at_seconds,
        spec: UiAnimationSpec {
            motion_key: active.transition.motion_key.clone(),
            delay_ms: active.transition.delay_ms,
            duration_ms: active.transition.duration_ms,
            easing: active.transition.easing,
            from: active.transition.from,
        },
        status,
    }
}

impl ActiveTransition {
    fn animation_instance(
        &self,
        node_path: &str,
        fragment_revision: neon_protocol::Revision,
        time_seconds: f32,
    ) -> UiAnimationInstance {
        let finished = transition_finished(self, time_seconds);
        UiAnimationInstance {
            node_path: node_path.to_owned(),
            fragment_revision,
            started_at_seconds: self.started_at_seconds,
            spec: UiAnimationSpec {
                motion_key: self.transition.motion_key.clone(),
                delay_ms: self.transition.delay_ms,
                duration_ms: self.transition.duration_ms,
                easing: self.transition.easing,
                from: self.transition.from,
            },
            status: if finished {
                UiAnimationStatus::Completed
            } else {
                UiAnimationStatus::Running
            },
        }
    }
}

struct PlannedNode {
    id: String,
    parent_id: Option<String>,
    target: UiVisual,
    transition: Option<UiTransition>,
    instance_index: Option<usize>,
    paint_group_id: u32,
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

/// Cache entry for a single text-bearing node's laid-out glyph instances.
/// Re-layout is skipped when the composite cache key (node path + text +
/// text scale + atlas generation) matches (plan §7.3).
#[derive(Clone)]
struct CachedTextLayout {
    text_instances: Vec<UiTextInstance>,
    /// Final visual origin used when the glyph instances were built. The
    /// logical line layout is reusable, but sampled scroll/parent translation
    /// must be applied to the cached visual instances on cache hits.
    visual_origin: [f32; 2],
}

/// Per-draw stage timings collected on the last color pass. Diagnostics only;
/// collecting these never alters rendering behavior or draw order.
#[derive(Default, Clone, Copy)]
pub(crate) struct UiDrawStageTimings {
    pub refresh_plan_ms: f32,
    pub compose_visuals_ms: f32,
    pub text_layout_ms: f32,
    pub group_sort_ms: f32,
    pub buffer_upload_ms: f32,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct UiLayoutCounters {
    layout_count: u64,
    text_layout_count: u64,
    world_transform_update_count: u64,
}

/// Which subset of the combined plan a pass should emit. The plan itself is
/// always the full combined Flow (screen UI + projected world panels); only the
/// instance emission is filtered so a single renderer can feed both the world
/// target (color + occlusion depth) and the screen target (color only) from the
/// same buffer index / frame sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UiDrawMode {
    /// Every node (used by the unified ID pass).
    All,
    /// Only projected world panels and their descendants.
    World,
    /// Only ordinary screen UI (nodes without a world depth).
    Screen,
    /// Nodes declared `composition_layer behind_glass` and descendants.
    BehindGlass,
}

fn composition_layer_is(layer: neon_ui_schema::UiCompositionLayer, mode: UiDrawMode) -> bool {
    match mode {
        UiDrawMode::BehindGlass => layer == neon_ui_schema::UiCompositionLayer::BehindGlass,
        UiDrawMode::All => true,
        UiDrawMode::Screen | UiDrawMode::World => layer != neon_ui_schema::UiCompositionLayer::BehindGlass,
    }
}

fn color_pass_depth(world_depth: Option<f32>) -> f32 {
    // The exported world depth is reversed-Z, but the producer color pass uses
    // the ordinary LessEqual depth test.
    world_depth.map_or(0.0, |depth| 1.0 - depth)
}

fn compare_paint_group_order(
    left_group: u32,
    right_group: u32,
    group_depths: &HashMap<u32, Option<f32>>,
) -> std::cmp::Ordering {
    let left_depth = group_depths.get(&left_group).copied().flatten();
    let right_depth = group_depths.get(&right_group).copied().flatten();
    match (left_depth, right_depth) {
        // The external color target is emitted far-to-near. Under the exported
        // reversed-Z convention larger source depth is nearer, so ascending
        // source depth emits far groups first. Equal-depth groups still need a
        // total order.
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left_group.cmp(&right_group)),
        // Screen groups have no GPU depth and are always above projected world
        // groups in the combined color pass.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        // Group IDs are assigned by flattened declaration order for Screen UI.
        (None, None) => left_group.cmp(&right_group),
    }
}

fn paint_group_root_ids(plan: &[PlannedNode]) -> HashSet<String> {
    let indices = plan
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    plan.iter()
        .filter_map(|node| {
            let parent = node
                .parent_id
                .as_deref()
                .and_then(|parent_id| indices.get(parent_id).copied());
            let is_root = if node.target.world_depth.is_some() {
                // A projected panel starts a depth group when its parent is not
                // projected. Descendants inherit the same world depth/group.
                parent.map_or(true, |parent| plan[parent].target.world_depth.is_none())
            } else {
                // Screen UI has no depth buffer. Keep the surface root and each
                // direct child as atomic painter groups so panel backgrounds,
                // images and text cannot be split across sibling panels.
                parent.map_or(true, |parent| plan[parent].parent_id.is_none())
            };
            is_root.then(|| node.id.clone())
        })
        .collect()
}

fn assign_paint_group_ids(plan: &mut [PlannedNode]) {
    let roots = paint_group_root_ids(plan);
    let mut group_ids = HashMap::<String, u32>::new();
    let mut next_group_id = 1_u32;
    for index in 0..plan.len() {
        let mut root = index;
        while !roots.contains(&plan[root].id) {
            let Some(parent_id) = plan[root].parent_id.as_deref() else {
                break;
            };
            let Some(parent) = plan.iter().position(|node| node.id == parent_id) else {
                break;
            };
            root = parent;
        }
        let root_id = plan[root].id.clone();
        let group_id = *group_ids.entry(root_id).or_insert_with(|| {
            let id = next_group_id;
            next_group_id = next_group_id.saturating_add(1);
            id
        });
        plan[index].paint_group_id = group_id;
    }
}

fn gpu_easing(easing: UiEasing) -> f32 {
    match easing {
        UiEasing::Linear => 0.0,
        UiEasing::EaseIn => 1.0,
        UiEasing::EaseOut => 2.0,
        UiEasing::EaseInOut => 3.0,
    }
}

fn is_world_panel_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|key| key.strip_prefix('p'))
        .is_some_and(|index| !index.is_empty() && index.chars().all(|c| c.is_ascii_digit()))
}

fn transition_finished(active: &ActiveTransition, time_seconds: f32) -> bool {
    time_seconds
        >= active.started_at_seconds
            + (active.transition.delay_ms + active.transition.duration_ms) as f32 / 1000.0
}

/// Resolves the final shell polygon from authored corner cuts and final panel
/// bounds. Adjacent cuts are proportionally compressed when a responsive shell
/// becomes too narrow; every visual pass and hit pass must consume this result.
fn resolve_shell_cut(size: [f32; 2], declared: [f32; 4]) -> [f32; 4] {
    let width = size[0].max(0.0);
    let height = size[1].max(0.0);
    let max_corner = (width.min(height) * 0.5).max(0.0);
    let mut cut = declared.map(|value| value.max(0.0).min(max_corner));
    for (a, b, limit) in [(0usize, 3usize, width), (3, 2, width), (2, 1, width), (1, 0, width), (0, 1, height), (1, 2, height), (2, 3, height), (3, 0, height)] {
        let total = cut[a] + cut[b];
        if total > limit && total > 0.0 {
            let scale = limit / total;
            cut[a] *= scale;
            cut[b] *= scale;
        }
    }
    cut
}

/// Active splitter drag state. Records the left/right panel indices and
/// the current drag ratio so sampling can resize panels in real time.
struct SplitterDrag {
    left_index: usize,
    right_index: usize,
    splitter_path: String,
    /// True when the splitter divides a horizontal row (drag along X axis).
    horizontal: bool,
    /// The container's left edge (or top edge for vertical).
    container_start: f32,
    /// Total width of left + splitter + right.
    container_size: f32,
    /// Width of the splitter itself.
    splitter_size: f32,
    /// Offset from pointer to splitter's leading edge at drag start.
    /// Keeps the grab point stable so the splitter doesn't jump.
    pointer_offset: f32,
    /// Current leading edge position of the splitter.
    splitter_pos: f32,
    /// Original bounds for all affected nodes, captured at drag start.
    original_bounds: Vec<(usize, UiBounds)>,
}

fn splitter_debug(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("D:\\Neon3\\splitter_debug.log")
    {
        let _ = writeln!(f, "{}", msg);
    }
}

pub struct UiWgpuRenderer {
    trace_role: &'static str,
    color_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    depth_format: Option<wgpu::TextureFormat>,
    depth_pipeline: Option<wgpu::RenderPipeline>,
    view_layout: wgpu::BindGroupLayout,
    view_buffer: wgpu::Buffer,
    view_bind_group: wgpu::BindGroup,
    /// GPU→CPU shader event ring (storage buffer, written by material shaders).
    event_buffer: wgpu::Buffer,
    /// Staging buffer for `event_buffer` readback.
    event_staging_buffer: wgpu::Buffer,
    /// Events read back from the previous frame; drained by `take_shader_events`.
    pending_shader_events: Vec<(u32, [f32; 4])>,
    /// Generic per-view extra uniform data (10 x vec4 = 160 bytes).
    /// Host writes via wgpu.ui.set_view_extras; runtime does not interpret content.
    view_extras: [[f32; 4]; 10],
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    depth_instance_buffer: wgpu::Buffer,
    depth_instance_capacity: usize,
    material_instance_buffer: wgpu::Buffer,
    material_instance_capacity: usize,
    popup_instance_buffer: wgpu::Buffer,
    popup_instance_capacity: usize,
    plan_revisions: HashMap<neon_ui_schema::UiFragmentId, neon_protocol::Revision>,
    plan: Vec<PlannedNode>,
    plan_index: HashMap<String, usize>,
    composition_layers: HashMap<String, neon_ui_schema::UiCompositionLayer>,
    debug_semantic_nodes: Vec<DebugSemanticNode>,
    sampled: Vec<UiVisual>,
    instances: Vec<UiInstance>,
    uploaded_instances: Vec<UiInstance>,
    uploaded_depth_instances: Vec<UiInstance>,
    viewport_physical_size: [u32; 2],
    viewport_logical_size: [f32; 2],
    viewport_revision: u64,
    plan_viewport_revision: u64,
    view_buffer_viewport_revision: u64,
    current: HashMap<String, UiVisual>,
    active: HashMap<String, ActiveTransition>,
    animation_history: VecDeque<UiAnimationInstance>,
    pointer_position: Option<[f32; 2]>,
    pressed_until_seconds: f32,
    hit_pipeline: wgpu::RenderPipeline,
    hit_clear_pipeline: wgpu::RenderPipeline,
    hit_buffer: wgpu::Buffer,
    hit_capacity: usize,
    hit_readbacks: HitReadbackRing,
    hit_bindings: HashMap<u32, UiHitBinding>,
    hit_id_by_node: HashMap<String, u32>,
    image_pipeline: wgpu::RenderPipeline,
    image_buffer: wgpu::Buffer,
    popup_image_buffer: wgpu::Buffer,
    image_capacity: usize,
    resident_images: HashMap<(String, u64, u64), ResidentImage>,
    external_images: HashMap<String, ResidentImage>,
    nine_slices: HashMap<String, neon_ui_schema::UiNineSlice>,
    /// Cut-corner panel styles keyed by the short node id (matches the
    /// `nine_slices` pattern). Zero cut means no corner removal.
    node_cuts: HashMap<String, [f32; 4]>,
    /// Declarative material overlays keyed by the node's stable short id.
    /// These are emitted after their host rect and before its children; they do
    /// not enter the hit-id pass.
    node_materials: HashMap<String, UiMaterialRef>,
    material_pipelines: BTreeMap<String, wgpu::RenderPipeline>,
    image_fits: HashMap<String, UiImageFit>,
    skins: HashMap<String, UiControlSkin>,
    skin_references: HashMap<String, String>,
    skin_image_ids: HashMap<String, String>,
    skin_assets: HashMap<String, AssetRef>,
    image_atlas: Option<ResidentImageAtlas>,
    image_atlas_generation: u64,
    resident_render_surfaces: HashMap<String, ResidentRenderSurface>,
    image_texture_layout: wgpu::BindGroupLayout,
    canvas_pipeline: wgpu::RenderPipeline,
    canvas_buffer: wgpu::Buffer,
    canvas_capacity: usize,
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
    /// Persistent built-in component interaction state (checkbox toggle, etc.).
    /// These values survive fragment re-submissions and override fragment
    /// ControlPresentation during rendering.
    builtin_toggles: HashMap<String, bool>,
    /// Global context-menu visibility flag. Per-node builtin_toggles filter
    /// runs during plan building when hidden menus aren't in the plan yet.
    context_menus_visible: bool,
    /// Right-click anchor position for context menu placement.
    context_menu_anchor: Option<[f32; 2]>,
    /// Maps host node path -> bound ContextMenu node key.
    context_menu_bindings: HashMap<String, String>,
    /// The currently visible context menu ID (only one shown at a time).
    active_context_menu_id: Option<String>,
    /// Active splitter drag state, if any.
    splitter_drag: Option<SplitterDrag>,
    /// Persistent splitter positions (x coordinate) keyed by splitter node path.
    /// Survives fragment re-submissions; applied each frame so drag results stick.
    splitter_positions: HashMap<String, f32>,
    /// Persistent built-in numeric state (slider value, scroll position).
    builtin_numerics: HashMap<String, (f32, f32, f32)>,
    /// Persistent built-in choice selection (ListBox/Tabs/Combo/Dropdown).
    builtin_choices: HashMap<String, String>,
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
    /// Text layout cache: keyed by `{fragment_id}:{node_path}`, stores the
    /// computed glyph instances so camera/anchor movement does not re-layout
    /// static monster name/level/title text (plan §7.3).
    text_layout_cache: HashMap<String, CachedTextLayout>,
    /// Monotonic font atlas generation counter. Incremented when a new glyph is
    /// rasterized so the text layout cache can detect atlas changes.
    atlas_generation: u64,
    available_cameras: HashSet<(neon_world_bridge::CameraId, neon_world_bridge::CameraKind)>,
    last_stage_timings: UiDrawStageTimings,
    layout_counters: UiLayoutCounters,
}

impl UiWgpuRenderer {
    /// Compiles package-local fragment functions into renderer-owned material
    /// pipelines. Sources never receive a device, texture, sampler, or bind
    /// group: the fixed wrapper supplies the only ABI and GPU bindings.
    pub(crate) fn sync_material_packages(
        &mut self,
        device: &wgpu::Device,
        packages: &[UiShaderPackage],
    ) {
        for package in packages {
            if self.material_pipelines.contains_key(&package.package_id) {
                continue;
            }
            let Ok(source) = std::str::from_utf8(&package.source_bytes) else {
                continue;
            };
            let source = format!("{MATERIAL_SHADER_PREFIX}\n{source}\n{MATERIAL_SHADER_SUFFIX}");
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&format!("neon3-ui-material-{}", package.package_id)),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("neon3-ui-material-layout"),
                bind_group_layouts: &[Some(&self.view_layout)],
                immediate_size: 0,
            });
            let attributes = [
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 64, shader_location: 4 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 80, shader_location: 5 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 88, shader_location: 6 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 104, shader_location: 7 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 120, shader_location: 8 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 136, shader_location: 9 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 152, shader_location: 10 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 168, shader_location: 11 },
            ];
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("neon3-ui-material-pipeline-{}", package.package_id)),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<UiInstance>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &attributes })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_material"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.color_format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            self.material_pipelines.insert(package.package_id.clone(), pipeline);
        }
    }
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_internal(device, format, None, "screen")
    }

    pub(crate) fn new_unified(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_internal(device, format, None, "unified")
    }

    /// Renderer that also emits a per-pixel occlusion depth target (R32Float).
    /// `draw` writes color as usual; `draw_depth` re-emits the same instances
    /// with their normalized depth into a separate depth pass.
    pub(crate) fn new_with_depth(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        Self::new_internal(device, format, Some(depth_format), "world")
    }

    fn new_internal(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        trace_role: &'static str,
    ) -> Self {
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon3-ui-view-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let view_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-ui-view"),
            size: std::mem::size_of::<UiView>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let event_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-ui-shader-events"),
            size: SHADER_EVENT_BUFFER_SIZE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let event_staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-ui-shader-events-staging"),
            size: SHADER_EVENT_BUFFER_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-ui-view-bind-group"),
            layout: &view_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: event_buffer.as_entire_binding(),
                },
            ],
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
        let premultiplied_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
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
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 80,
                            shader_location: 5,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 88,
                            shader_location: 6,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 104,
                            shader_location: 7,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 120,
                            shader_location: 8,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 136,
                            shader_location: 9,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 152,
                            shader_location: 10,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 168,
                            shader_location: 11,
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
                    blend: Some(premultiplied_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth_format.map(|_| wgpu::DepthStencilState {
                // This is the GPU depth attachment for the color pass. The
                // exported occlusion target is a separate R32Float color
                // target and must never be used as a depth-stencil format.
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
        let canvas_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neon3-ui-canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(CANVAS_SHADER.into()),
        });
        let canvas_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neon3-ui-canvas-layout"),
            bind_group_layouts: &[Some(&view_layout)],
            immediate_size: 0,
        });
        let canvas_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neon3-ui-canvas-pipeline"),
            layout: Some(&canvas_layout),
            vertex: wgpu::VertexState {
                module: &canvas_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiCanvasInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 32,
                            shader_location: 3,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 36,
                            shader_location: 4,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 40,
                            shader_location: 5,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 56,
                            shader_location: 6,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &canvas_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(premultiplied_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth_format.map(|_| wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
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
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 64,
                            shader_location: 4,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 72,
                            shader_location: 5,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 88,
                            shader_location: 6,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 104,
                            shader_location: 7,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: 108,
                            shader_location: 8,
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
                    blend: Some(premultiplied_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth_format.map(|_| wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                // Keep the attachment format compatible with the world color
                // pass, but let the explicit far-to-near painter order decide
                // which panel covers another. The exported R32 depth ring is
                // the separate scene-occlusion path.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 64,
                            shader_location: 4,
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
                    blend: Some(premultiplied_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth_format.map(|_| wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
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
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 88,
                                shader_location: 6,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 104,
                                shader_location: 7,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 120,
                                shader_location: 8,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 136,
                                shader_location: 9,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 152,
                                shader_location: 10,
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
            trace_role,
            color_format: format,
            pipeline,
            depth_format,
            depth_pipeline,
            view_layout,
            view_buffer,
            view_bind_group,
            event_buffer,
            event_staging_buffer,
            pending_shader_events: Vec::new(),
            view_extras: [[0.0; 4]; 10],
            // Pre-allocate GPU buffers to the plan's known budget (512 nodes,
            // 512 bindings) so the render loop never re-creates buffers on the
            // hot path. The growth path still exists as a safety net.
            instance_buffer: create_instance_buffer(device, 512),
            instance_capacity: 512,
            depth_instance_buffer: create_instance_buffer(device, 512),
            depth_instance_capacity: 512,
            material_instance_buffer: create_instance_buffer(device, 512),
            material_instance_capacity: 512,
            popup_instance_buffer: create_instance_buffer(device, 512),
            popup_instance_capacity: 512,
            plan_revisions: HashMap::new(),
            plan: Vec::new(),
            plan_index: HashMap::new(),
            composition_layers: HashMap::new(),
            debug_semantic_nodes: Vec::new(),
            sampled: Vec::new(),
            instances: Vec::new(),
            uploaded_instances: Vec::new(),
            uploaded_depth_instances: Vec::new(),
            viewport_physical_size: [0, 0],
            viewport_logical_size: [0.0, 0.0],
            viewport_revision: 0,
            plan_viewport_revision: 0,
            view_buffer_viewport_revision: 0,
            current: HashMap::new(),
            active: HashMap::new(),
            animation_history: VecDeque::with_capacity(64),
            pointer_position: None,
            pressed_until_seconds: 0.0,
            hit_pipeline,
            hit_clear_pipeline,
            hit_buffer: create_hit_buffer(device, 512),
            hit_capacity: 512,
            hit_readbacks: HitReadbackRing::new(device, 3),
            hit_bindings: HashMap::new(),
            hit_id_by_node: HashMap::new(),
            image_pipeline,
            image_buffer: create_image_buffer(device, 512),
            popup_image_buffer: create_image_buffer(device, 512),
            image_capacity: 512,
            resident_images: HashMap::new(),
            external_images: HashMap::new(),
            nine_slices: HashMap::new(),
            node_cuts: HashMap::new(),
            node_materials: HashMap::new(),
            material_pipelines: BTreeMap::new(),
            image_fits: HashMap::new(),
            skins: HashMap::new(),
            skin_references: HashMap::new(),
            skin_image_ids: HashMap::new(),
            skin_assets: HashMap::new(),
            image_atlas: None,
            image_atlas_generation: 0,
            resident_render_surfaces: HashMap::new(),
            image_texture_layout,
            canvas_pipeline,
            canvas_buffer: create_canvas_buffer(device, 512),
            canvas_capacity: 512,
            text_pipeline,
            text_buffer: create_text_buffer(device, 512),
            text_capacity: 512,
            popup_text_buffer: create_text_buffer(device, 512),
            popup_text_capacity: 512,
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
            builtin_toggles: HashMap::new(),
            context_menus_visible: false,
            context_menu_anchor: None,
            context_menu_bindings: HashMap::new(),
            active_context_menu_id: None,
            splitter_drag: None,
            splitter_positions: HashMap::new(),
            builtin_numerics: HashMap::new(),
            builtin_choices: HashMap::new(),
            pending_local_presentations: HashMap::new(),
            open_dropdown: None,
            scroll_offsets: HashMap::new(),
            scroll_metrics: HashMap::new(),
            scroll_drag: None,
            scroll_pan: None,
            data_grid_frames: HashMap::new(),
            data_grid_scroll_holds: HashMap::new(),
            data_grid_text_display_cache: HashMap::new(),
            text_layout_cache: HashMap::new(),
            atlas_generation: 0,
            available_cameras: HashSet::new(),
            last_stage_timings: UiDrawStageTimings::default(),
            layout_counters: UiLayoutCounters::default(),
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
            let short_key = self.plan[index]
                .id
                .rsplit('/')
                .next()
                .unwrap_or(&self.plan[index].id);
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
                cut: self.node_cuts.get(short_key).copied().unwrap_or([0.0; 4]),
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
                color_mode: 0,
                time_seconds,
                extras: get_global_view_extras(),
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

    fn plan_index_of(&self, node_path: &str) -> Option<usize> {
        self.plan_index.get(node_path).copied()
    }

    /// Number of hit bindings in the current ID frame. Used by the headless
    /// render loop's perf counters to report `unified_id_instances`.
    pub(crate) fn hit_binding_count(&self) -> usize {
        self.hit_bindings.len()
    }

    /// Returns whether a local interaction changed the visual control state
    /// without changing the authoritative fragment revision.
    pub(crate) fn pointer_visual_dirty(&self) -> bool {
        self.pointer_visual_dirty
    }

    /// Snapshot of the binding map for the last ID pass. Used to pair a
    /// completed unified ID frame with its numeric-ID -> semantic binding map
    /// so a pointer readback and its lookup come from the same frame.
    pub(crate) fn hit_bindings_snapshot(&self) -> std::collections::HashMap<u32, UiHitBinding> {
        self.hit_bindings.clone()
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
        // Apply persisted splitter positions (from finished drags) first.
        self.apply_persisted_splitter_positions();
        // Apply active splitter drag to target bounds BEFORE sampling.
        self.apply_splitter_drag_to_targets();
        self.update_scroll_metrics();
        for index in 0..self.plan.len() {
            let node_id = self.plan[index].id.clone();
            let target = self.plan[index].target.clone();
            let transition = self.plan[index].transition.clone();
            let was_active = self.active.contains_key(&node_id);
            // Always begin with a canonical transition sample. Inherited composition
            // below must never accumulate in `sampled` across renderer entry points.
            self.sampled[index] =
                self.sample_with_history(&node_id, &target, transition.as_ref(), time_seconds);
            if target.world_scale.is_some() && self.sampled[index].bounds != target.bounds {
                self.layout_counters.world_transform_update_count = self
                    .layout_counters
                    .world_transform_update_count
                    .saturating_add(1);
            }
            if !was_active
                && self.trace_role != "screen"
                && is_world_panel_path(&node_id)
                && self
                    .active
                    .get(&node_id)
                    .is_some_and(|active| active.transition.motion_key.is_some())
            {
                if let Some(active) = self.active.get(&node_id) {
                    eprintln!(
                        "{}",
                        json!({
                            "event": "world_ui_transition_begin",
                            "node_path": node_id,
                            "motion_key": active.transition.motion_key,
                            "start_seconds": active.started_at_seconds,
                            "duration_ms": active.transition.duration_ms,
                        })
                    );
                }
            }
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
        // Apply persistent built-in choice selection (ListBox/Tabs/Combo/Dropdown).
        for (index, node) in self.plan.iter().enumerate() {
            let Some(selected) = self.builtin_choices.get(&node.id) else {
                continue;
            };
            if !matches!(node.target.kind, UiNodeKind::ListBox | UiNodeKind::Tabs | UiNodeKind::Combo | UiNodeKind::Dropdown) {
                continue;
            }
            if let Some(UiControlPresentation::Choice { options, .. }) = &self.sampled[index].presentation {
                self.sampled[index].presentation = Some(UiControlPresentation::Choice {
                    token: selected.clone(),
                    options: options.clone(),
                    selected: true,
                });
            }
        }
        // Apply persistent built-in numeric state (slider value, scroll position).
        // These survive fragment re-submissions. Transient value_previews (drag
        // in progress) override these in the loop below.
        for (index, node) in self.plan.iter().enumerate() {
            let Some(&(value, min, max)) = self.builtin_numerics.get(&node.id) else {
                continue;
            };
            if !matches!(node.target.kind, UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar | UiNodeKind::ProgressBar) {
                continue;
            }
            if let Some(UiControlPresentation::Numeric { .. }) = &self.sampled[index].presentation {
                self.sampled[index].presentation = Some(UiControlPresentation::Numeric {
                    value,
                    min,
                    max,
                });
            }
            if let Some(UiControlPresentation::Scroll { .. }) = &self.sampled[index].presentation {
                self.sampled[index].presentation = Some(UiControlPresentation::Scroll {
                    position: value,
                });
            }
        }
        // Local numeric gestures update the renderer presentation before an
        // authoritative fragment revision arrives. Apply that preview to the
        // sampled visual text as well as the chrome; otherwise the thumb moves
        // internally while the label keeps displaying the old `0.00` value.
        for (index, node) in self.plan.iter().enumerate() {
            let Some(UiSemanticPayloadValue::F32 { value }) = self.value_previews.get(&node.id)
            else {
                continue;
            };
            if !matches!(node.target.kind, UiNodeKind::Slider | UiNodeKind::DragValue) {
                continue;
            }
            if let Some(UiControlPresentation::Numeric { min, max, .. }) =
                &self.sampled[index].presentation
            {
                self.sampled[index].presentation = Some(UiControlPresentation::Numeric {
                    value: *value,
                    min: *min,
                    max: *max,
                });
            }
            if node.target.kind == UiNodeKind::Slider {
                if let Some(TextRef::Literal { value: label }) = &node.target.text {
                    self.sampled[index].text = Some(TextRef::Literal {
                        value: format!("{label}: {value:.2}"),
                    });
                }
            }
        }
        top_layer
    }

    fn has_scroll_ancestor_in_plan(plan: &[PlannedNode], index: usize) -> bool {
        let mut parent = plan[index].parent_id.as_deref();
        while let Some(parent_id) = parent {
            let Some(parent_index) = plan.iter().position(|node| node.id == parent_id) else {
                break;
            };
            if plan[parent_index].target.scroll {
                return true;
            }
            parent = plan[parent_index].parent_id.as_deref();
        }
        false
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

    /// Resolve a window pointer against the same composed visual and semantic
    /// binding snapshot in one operation. This avoids using a numeric GPU ID
    /// from a previous asynchronous frame to choose the current control.
    pub(crate) fn hit_binding_at_pointer(&self) -> Option<(u32, UiHitBinding)> {
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
                self.hit_bindings.iter().find_map(|(hit_id, binding)| {
                    (binding.node_path == node.id).then_some((*hit_id, binding.clone()))
                })
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
        Value::Null
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
            .plan_index_of(node_path)
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
        let hit_id = self
            .hit_id_by_node
            .get(node_path)
            .copied()
            .ok_or("semantic_target_not_hittable")?;
        self.hit_bindings
            .get(&hit_id)
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
            self.plan_index_of(&node.plan_path)
                .filter(|_| node.plan_path == expected_plan_path)
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
            .plan_index_of(node_path)
            .ok_or("unknown_semantic_target")?;
        let visual = self.visual_at(index);
        let (bounds, current_fraction) = match (&visual.kind, &visual.presentation) {
            (UiNodeKind::Slider, Some(UiControlPresentation::Numeric { value, min, max })) => (
                UiBounds {
                    // Keep debug producer points on the same track geometry
                    // consumed by begin_value_gesture/update_value_gesture.
                    x: visual.bounds.x + 12.0,
                    y: visual.bounds.y,
                    width: (visual.bounds.width - 24.0).max(1.0),
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
            (UiNodeKind::Splitter, _) => (0.0, 1.0), // split ratio 0-1
            _ => return false,
        };
        let hit_bounds = visual.bounds;
        let bounds = match &kind {
            // Use the same full-width track geometry as the renderer chrome.
            // Numeric sliders expose min/max values, so 0%, 50%, and 100%
            // must resolve to the left, midpoint, and right of one predictable
            // track rather than an arbitrary label-reserved subregion.
            UiNodeKind::Slider => UiBounds {
                x: visual.bounds.x + 12.0,
                y: visual.bounds.y,
                width: (visual.bounds.width - 24.0).max(1.0),
                height: visual.bounds.height,
            },
            UiNodeKind::Scrollbar => UiBounds {
                x: visual.bounds.x + 10.0,
                y: visual.bounds.y,
                width: (visual.bounds.width - 20.0).max(1.0),
                height: visual.bounds.height,
            },
            UiNodeKind::DragValue => drag_value_bounds(visual.bounds),
            UiNodeKind::Splitter => {
                // Use parent container bounds as the drag range
                if let Some(parent_id) = self.plan.iter().find(|n| n.id == binding.node_path).and_then(|n| n.parent_id.clone()) {
                    if let Some(parent) = self.plan.iter().find(|n| n.id == parent_id) {
                        parent.target.bounds
                    } else {
                        visual.bounds
                    }
                } else {
                    visual.bounds
                }
            }
            _ => visual.bounds,
        };
        if !self
            .pointer_position
            .is_some_and(|pointer| contains(hit_bounds, pointer))
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
                    UiNodeKind::Slider | UiNodeKind::DragValue | UiNodeKind::Scrollbar | UiNodeKind::Splitter
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
        // Persist numeric value to built-in state (survives fragment re-submission)
        if let UiSemanticPayloadValue::F32 { value: v } = &value {
            self.builtin_numerics
                .insert(gesture.node_path.clone(), (*v, gesture.min, gesture.max));
        }
        if let UiSemanticPayloadValue::I32 { value: v } = &value {
            self.builtin_numerics
                .insert(gesture.node_path.clone(), (*v as f32, gesture.min, gesture.max));
        }
        Some((
            value.clone(),
            LocalPresentationCommit::Value {
                node_path: gesture.node_path,
                value,
            },
        ))
    }

    /// Produces an immediate renderer-local toggle prediction. The domain
    /// remains authoritative; the pending value is cleared on an accepted
    /// fragment publication or rolled back on rejection.
    pub(crate) fn finish_toggle_control(
        &mut self,
        node_path: &str,
    ) -> Option<(UiSemanticPayloadValue, LocalPresentationCommit)> {
        let index = self.plan_index_of(node_path)?;
        let visual = self.visual_at(index);
        if !matches!(
            visual.kind,
            UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
        ) {
            return None;
        }
        let selected = self
            .value_previews
            .get(node_path)
            .and_then(|value| match value {
                UiSemanticPayloadValue::Bool { value } => Some(*value),
                _ => None,
            })
            .or_else(|| match &visual.presentation {
                Some(UiControlPresentation::Toggle { selected }) => Some(*selected),
                _ => None,
            })?;
        let new_selected = !selected;
        let value = UiSemanticPayloadValue::Bool { value: new_selected };
        // Persist to built-in state (survives fragment re-submission)
        self.builtin_toggles.insert(node_path.to_owned(), new_selected);
        // Also set transient preview for immediate visual feedback
        self.value_previews
            .insert(node_path.to_owned(), value.clone());
        self.pointer_visual_dirty = true;
        Some((
            value.clone(),
            LocalPresentationCommit::Value {
                node_path: node_path.to_owned(),
                value,
            },
        ))
    }

    /// Set persistent built-in choice selection for ListBox/Tabs/Combo/Dropdown.
    pub(crate) fn set_choice(&mut self, node_path: &str, value: &str) {
        self.builtin_choices.insert(node_path.to_owned(), value.to_owned());
        self.pointer_visual_dirty = true;
    }

    /// Toggle TreeView node expand/collapse state.
    /// Returns true if expanded, false if collapsed.
    pub(crate) fn toggle_treeview_node(&mut self, node_path: &str) -> bool {
        let current = self.builtin_toggles.get(node_path).copied().unwrap_or(true);
        let new = !current;
        self.builtin_toggles.insert(node_path.to_owned(), new);
        self.pointer_visual_dirty = true;
        new
    }

    /// Check if a node is a TreeView child (parent is TreeView).
    pub(crate) fn is_treeview_child(&self, node_path: &str) -> bool {
        let Some(index) = self.plan.iter().position(|node| node.id == node_path) else {
            return false;
        };
        let Some(parent_id) = self.plan[index].parent_id.as_ref() else {
            return false;
        };
        self.plan.iter().any(|node| {
            node.id == *parent_id && matches!(node.target.kind, UiNodeKind::TreeView)
        })
    }

    /// Whether a splitter drag is currently active.
    pub(crate) fn splitter_drag_active(&self) -> bool {
        self.splitter_drag.is_some()
    }

    /// Check if the node at the given path is a Splitter.
    pub(crate) fn is_splitter_binding(&self, node_path: &str) -> bool {
        self.plan.iter().any(|node| {
            node.id == node_path && matches!(node.target.kind, UiNodeKind::Splitter)
        })
    }

    /// Begin a splitter drag. Finds the left and right sibling panels around
    /// the splitter and records the initial ratio.
    pub(crate) fn begin_splitter_drag(&mut self, splitter_path: &str) -> bool {
        let Some(split_idx) = self.plan.iter().position(|n| n.id == splitter_path) else {
            return false;
        };
        let splitter = &self.plan[split_idx];
        let parent_id = match splitter.parent_id.clone() {
            Some(pid) => pid,
            None => return false,
        };
        // Find siblings: children of the same parent, in plan order.
        let siblings: Vec<usize> = self.plan.iter().enumerate()
            .filter(|(_, n)| n.parent_id.as_deref() == Some(parent_id.as_str()))
            .map(|(i, _)| i)
            .collect();
        let pos = match siblings.iter().position(|&i| i == split_idx) {
            Some(p) => p,
            None => return false,
        };
        if pos == 0 || pos >= siblings.len() - 1 {
            return false;
        }
        let left_idx = siblings[pos - 1];
        let right_idx = siblings[pos + 1];
        let left = &self.plan[left_idx].target.bounds;
        let right = &self.plan[right_idx].target.bounds;
        let total = left.width + splitter.target.bounds.width + right.width;
        if total <= 0.0 {
            return false;
        }
        let horizontal = splitter.target.bounds.width < splitter.target.bounds.height;
        let container_start = if horizontal { left.x } else { left.y };
        let splitter_size = if horizontal { splitter.target.bounds.width } else { splitter.target.bounds.height };
        let splitter_lead = if horizontal { splitter.target.bounds.x } else { splitter.target.bounds.y };
        // Record pointer offset so the grab point stays stable during drag.
        let pointer_pos = if horizontal {
            self.pointer_position.map(|p| p[0]).unwrap_or(splitter_lead)
        } else {
            self.pointer_position.map(|p| p[1]).unwrap_or(splitter_lead)
        };
        let pointer_offset = pointer_pos - splitter_lead;
        // Capture original bounds for panels, splitter, and all descendants.
        let mut original_bounds = Vec::new();
        for idx in 0..self.plan.len() {
            if idx == left_idx || idx == right_idx || idx == split_idx
                || is_descendant(&self.plan, idx, left_idx)
                || is_descendant(&self.plan, idx, right_idx)
            {
                original_bounds.push((idx, self.plan[idx].target.bounds));
            }
        }
        self.splitter_drag = Some(SplitterDrag {
            left_index: left_idx,
            right_index: right_idx,
            splitter_path: splitter_path.to_string(),
            horizontal,
            container_start,
            container_size: total,
            splitter_size,
            pointer_offset,
            splitter_pos: splitter_lead,
            original_bounds,
        });
        splitter_debug(&format!(
            "[BEGIN] path={} horizontal={} container_start={} container_size={} splitter_size={} pointer_offset={} splitter_pos={} left_w={} right_w={}",
            splitter_path, horizontal, container_start, total, splitter_size, pointer_offset, splitter_lead, left.width, right.width
        ));
        self.pointer_visual_dirty = true;
        true
    }

    /// Update the splitter drag based on current pointer position.
    pub(crate) fn update_splitter_drag(&mut self) {
        let Some(drag) = self.splitter_drag.as_mut() else { return };
        let Some(pointer) = self.pointer_position else { return };
        let pos = if drag.horizontal { pointer[0] } else { pointer[1] };
        // Splitter leading edge follows pointer with the original grab offset.
        let min_pos = drag.container_start + 8.0; // minimum left panel width
        let max_pos = drag.container_start + drag.container_size - drag.splitter_size - 8.0;
        let new_pos = (pos - drag.pointer_offset).clamp(min_pos, max_pos);
        splitter_debug(&format!(
            "[UPDATE] pointer=({:.1},{:.1}) pos={:.1} offset={:.1} old_pos={:.1} new_pos={:.1} min={:.1} max={:.1}",
            pointer[0], pointer[1], pos, drag.pointer_offset, drag.splitter_pos, new_pos, min_pos, max_pos
        ));
        drag.splitter_pos = new_pos;
        self.pointer_visual_dirty = true;
    }

    /// Finish the splitter drag and persist the final position.
    pub(crate) fn finish_splitter_drag(&mut self) {
        if let Some(drag) = self.splitter_drag.take() {
            self.splitter_positions.insert(drag.splitter_path.clone(), drag.splitter_pos);
            splitter_debug(&format!("[FINISH] path={} saved_pos={:.1}", drag.splitter_path, drag.splitter_pos));
            self.pointer_visual_dirty = true;
        }
    }

    /// Apply active splitter drag to target bounds BEFORE sampling.
    /// This modifies plan[index].target.bounds so sampling, inherited transforms,
    /// hit testing, and vertex submission all see the drag geometry natively.
    fn apply_splitter_drag_to_targets(&mut self) {
        let Some(drag) = self.splitter_drag.as_ref() else { return };
        let left_idx = drag.left_index;
        let right_idx = drag.right_index;
        let split_idx = match self.plan.iter().position(|n| n.id == drag.splitter_path) {
            Some(i) => i,
            None => return,
        };
        // Step 1: RESET all affected nodes to original bounds.
        for (idx, orig) in &drag.original_bounds {
            self.plan[*idx].target.bounds = *orig;
        }
        // Step 2: Compute geometry directly from splitter position (no ratio).
        let split_x = drag.splitter_pos;
        let split_w = drag.splitter_size;
        let left_x = drag.container_start;
        let new_left_w = split_x - left_x;
        let right_x = split_x + split_w;
        let new_right_w = (left_x + drag.container_size) - right_x;
        // Step 3: Apply new bounds.
        self.plan[left_idx].target.bounds.width = new_left_w;
        self.plan[split_idx].target.bounds.x = split_x;
        self.plan[right_idx].target.bounds.x = right_x;
        self.plan[right_idx].target.bounds.width = new_right_w;
        splitter_debug(&format!(
            "[APPLY] split_x={:.1} new_left_w={:.1} right_x={:.1} new_right_w={:.1} left_idx={} right_idx={} split_idx={}",
            split_x, new_left_w, right_x, new_right_w, left_idx, right_idx, split_idx
        ));
        // Step 4: Get original bounds for delta calculation.
        let orig_left_w = drag.original_bounds.iter()
            .find(|(i, _)| *i == left_idx).map(|(_, b)| b.width).unwrap_or(new_left_w);
        let orig_right_x = drag.original_bounds.iter()
            .find(|(i, _)| *i == right_idx).map(|(_, b)| b.x).unwrap_or(right_x);
        let orig_right_w = drag.original_bounds.iter()
            .find(|(i, _)| *i == right_idx).map(|(_, b)| b.width).unwrap_or(new_right_w);
        let left_dw = new_left_w - orig_left_w;
        let right_dx = right_x - orig_right_x;
        let right_dw = new_right_w - orig_right_w;
        // Step 5: Propagate to descendants.  Only right-panel descendants move
        // in x; absolute-positioned children keep their own width.  Clip is
        // the intersection of child bounds with the resized panel, clamped to
        // zero so a fully-overflowing child produces no glyphs instead of a
        // negative-width clip that disables scissoring.
        for (idx, _) in &drag.original_bounds {
            if *idx == left_idx || *idx == right_idx || *idx == split_idx {
                continue;
            }
            let is_left = is_descendant(&self.plan, *idx, left_idx);
            let is_right = is_descendant(&self.plan, *idx, right_idx);
            if is_right {
                self.plan[*idx].target.bounds.x += right_dx;
            }
            let panel_bounds = if is_left {
                self.plan[left_idx].target.bounds
            } else {
                self.plan[right_idx].target.bounds
            };
            let cb = self.plan[*idx].target.bounds;
            let cx = panel_bounds.x.max(cb.x);
            let cy = panel_bounds.y.max(cb.y);
            let cw = (panel_bounds.x + panel_bounds.width).min(cb.x + cb.width) - cx;
            let ch = (panel_bounds.y + panel_bounds.height).min(cb.y + cb.height) - cy;
            self.plan[*idx].target.clip = UiBounds { x: cx, y: cy, width: cw.max(0.0), height: ch.max(0.0) };
        }
        self.plan[left_idx].target.clip = self.plan[left_idx].target.bounds;
        self.plan[right_idx].target.clip = self.plan[right_idx].target.bounds;
        self.plan[split_idx].target.clip = self.plan[split_idx].target.bounds;
        self.active.remove(&self.plan[left_idx].id);
        self.active.remove(&self.plan[right_idx].id);
        self.pointer_visual_dirty = true;
    }

    /// Apply persisted splitter positions (from finished drags) to plan bounds.
    /// Runs every frame so drag results survive fragment re-submissions.
    fn apply_persisted_splitter_positions(&mut self) {
        // Collect splitter nodes with persisted positions first.
        let splitters: Vec<(usize, f32)> = self.plan.iter().enumerate()
            .filter(|(_, n)| n.target.kind == UiNodeKind::Splitter)
            .filter_map(|(i, n)| self.splitter_positions.get(&n.id).map(|&pos| (i, pos)))
            .collect();
        for (split_idx, splitter_x) in splitters {
            let splitter = &self.plan[split_idx];
            let Some(parent_id) = splitter.parent_id.clone() else { continue };
            // Find siblings in plan order.
            let siblings: Vec<usize> = self.plan.iter().enumerate()
                .filter(|(_, n)| n.parent_id.as_deref() == Some(parent_id.as_str()))
                .map(|(i, _)| i)
                .collect();
            let Some(pos) = siblings.iter().position(|&i| i == split_idx) else { continue };
            if pos == 0 || pos >= siblings.len() - 1 { continue; }
            let left_idx = siblings[pos - 1];
            let right_idx = siblings[pos + 1];
            let split_w = splitter.target.bounds.width;
            let container_start = self.plan[left_idx].target.bounds.x;
            let container_end = self.plan[right_idx].target.bounds.x + self.plan[right_idx].target.bounds.width;
            let container_size = container_end - container_start;
            // Clamp position.
            let min_pos = container_start + 8.0;
            let max_pos = container_end - split_w - 8.0;
            let split_x = splitter_x.clamp(min_pos, max_pos);
            // Compute deltas from current bounds.
            let orig_left_w = self.plan[left_idx].target.bounds.width;
            let orig_right_x = self.plan[right_idx].target.bounds.x;
            let orig_right_w = self.plan[right_idx].target.bounds.width;
            let new_left_w = split_x - container_start;
            let new_right_x = split_x + split_w;
            let new_right_w = container_end - new_right_x;
            let left_dw = new_left_w - orig_left_w;
            let right_dx = new_right_x - orig_right_x;
            let right_dw = new_right_w - orig_right_w;
            // Apply to immediate nodes.
            self.plan[left_idx].target.bounds.width = new_left_w;
            self.plan[split_idx].target.bounds.x = split_x;
            self.plan[right_idx].target.bounds.x = new_right_x;
            self.plan[right_idx].target.bounds.width = new_right_w;
            // Propagate to descendants.  Only right-panel children move in x;
            // absolute children keep their own width.  Clip intersects the
            // resized panel and is clamped to zero.
            for idx in 0..self.plan.len() {
                if idx == left_idx || idx == right_idx || idx == split_idx { continue; }
                let is_left = is_descendant(&self.plan, idx, left_idx);
                let is_right = is_descendant(&self.plan, idx, right_idx);
                if !is_left && !is_right { continue; }
                if is_right {
                    self.plan[idx].target.bounds.x += right_dx;
                }
                let panel_bounds = if is_left {
                    self.plan[left_idx].target.bounds
                } else {
                    self.plan[right_idx].target.bounds
                };
                let cb = self.plan[idx].target.bounds;
                let cx = panel_bounds.x.max(cb.x);
                let cy = panel_bounds.y.max(cb.y);
                let cw = (panel_bounds.x + panel_bounds.width).min(cb.x + cb.width) - cx;
                let ch = (panel_bounds.y + panel_bounds.height).min(cb.y + cb.height) - cy;
                self.plan[idx].target.clip = UiBounds { x: cx, y: cy, width: cw.max(0.0), height: ch.max(0.0) };
            }
            self.plan[left_idx].target.clip = self.plan[left_idx].target.bounds;
            self.plan[right_idx].target.clip = self.plan[right_idx].target.bounds;
            self.plan[split_idx].target.clip = self.plan[split_idx].target.bounds;
        }
    }

    /// Show all context menus (built-in right-click). Uses a global flag
    /// because hidden menus aren't in the plan during filtering.
    /// Show a specific context menu at the pointer position. Only the menu
    /// with the matching ID is rendered; all other ContextMenu nodes stay hidden.
    pub(crate) fn show_context_menu(&mut self, menu_id: String) {
        self.active_context_menu_id = Some(menu_id);
        self.context_menus_visible = true;
        self.context_menu_anchor = self.pointer_position;
        self.pointer_visual_dirty = true;
    }

    /// Find the context menu bound to the node under the pointer (or ancestor).
    pub(crate) fn context_menu_at_pointer(&self) -> Option<String> {
        let pointer = self.pointer_position?;
        println!("[ctx-menu] pointer=({:.1},{:.1}), bindings={:?}", pointer[0], pointer[1], self.context_menu_bindings.keys().collect::<Vec<_>>());
        let hit_index = self.plan.iter().enumerate().rev().find_map(|(index, node)| {
            let b = node.target.bounds;
            if pointer[0] >= b.x && pointer[0] <= b.x + b.width
                && pointer[1] >= b.y && pointer[1] <= b.y + b.height
            {
                Some(index)
            } else {
                None
            }
        })?;
        println!("[ctx-menu] hit_index={}, hit_id={}", hit_index, self.plan[hit_index].id);
        let mut current = Some(hit_index);
        while let Some(idx) = current {
            let node = &self.plan[idx];
            println!("[ctx-menu] walk: id={}, parent={:?}", node.id, node.parent_id);
            if let Some(menu_id) = self.context_menu_bindings.get(&node.id) {
                println!("[ctx-menu] FOUND binding: {} -> {}", node.id, menu_id);
                return Some(menu_id.clone());
            }
            current = node.parent_id.as_deref()
                .and_then(|pid| self.plan.iter().position(|n| n.id == pid));
        }
        println!("[ctx-menu] NO binding found in ancestor chain");
        None
    }

    /// Hide all context menus (built-in, called on click-outside).
    pub(crate) fn hide_context_menus(&mut self) {
        if self.context_menus_visible {
            self.context_menus_visible = false;
            self.active_context_menu_id = None;
            self.pointer_visual_dirty = true;
        }
    }

    /// Check if a context menu is currently visible.
    pub(crate) fn context_menu_visible(&self, node_path: &str) -> bool {
        self.builtin_toggles.get(node_path).copied().unwrap_or(false)
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
                matches!(node.target.kind, UiNodeKind::Combo | UiNodeKind::Dropdown)
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
                .find(|binding| binding.node_path == node_path)
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
        let mut current = self.plan_index_of(node_id);
        while let Some(index) = current {
            if index == root {
                return true;
            }
            current = self.plan[index]
                .parent_id
                .as_deref()
                .and_then(|parent| self.plan_index_of(parent));
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
        let Some((_, binding)) = self.hit_binding_at_pointer() else {
            return false;
        };
        self.focused_control = Some(binding.node_path);
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

    pub(crate) fn text_input_debug_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "active": self.editing.node_path.is_some(),
            "node_path": self.editing.node_path.as_deref(),
            "cursor": self.editing.cursor,
            "selection_anchor": self.editing.selection_anchor,
            "committed_chars": self.editing.committed.chars().count(),
            "preedit_chars": self.editing.preedit.chars().count(),
            "horizontal_scroll": self.editing.horizontal_scroll,
            "ime_rect": self.text_input_ime_rect(),
        })
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

    pub(crate) fn finish_text_input(&mut self) -> Option<(UiHitBinding, String)> {
        let node_path = self.editing.node_path.clone()?;
        let binding = self.text_input_binding(&node_path)?;
        let value = self.editing.committed.clone();
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
            .enumerate()
            .filter_map(|(index, visual)| visual.image.as_ref().map(|asset| (index, visual, asset)))
            .map(|(index, visual, asset)| {
                let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
                let node_key = self.plan[index]
                    .id
                    .rsplit('/')
                    .next()
                    .unwrap_or(self.plan[index].id.as_str());
                let nine_slice = self.nine_slices.get(node_key).copied();
                serde_json::json!({
                    "asset": asset,
                    "bounds": visual.bounds,
                    "clip": visual.clip,
                    "resident": self.resident_images.contains_key(&key)
                        || asset
                            .project_id
                            .strip_prefix("external:")
                            .is_some_and(|image_id| self.external_images.contains_key(image_id)),
                    "uv": self
                        .resident_images
                        .get(&key)
                        .map(|image| image.uv)
                        .or_else(|| {
                            asset
                                .project_id
                                .strip_prefix("external:")
                                .and_then(|image_id| self.external_images.get(image_id))
                                .map(|image| image.uv)
                        }),
                    "nine_slice": nine_slice,
                    "nine_slice_valid_for_resident": nine_slice.is_none_or(|layout| {
                        self.resident_images
                            .get(&key)
                            .is_none_or(|image| layout.validate_for_image(image.width, image.height))
                            && asset.project_id.strip_prefix("external:").is_none_or(|image_id| {
                                self.external_images
                                    .get(image_id)
                                    .is_none_or(|image| layout.validate_for_image(image.width, image.height))
                            })
                    }),
                })
            })
            .collect::<Vec<_>>();
        let external_images = self
            .external_images
            .iter()
            .map(|(image_id, image)| {
                let atlas_size = self.image_atlas_size();
                let region_x = atlas_size
                    .map(|size| (image.uv[0] * size[0] as f32 - 0.5).max(0.0) as u32)
                    .unwrap_or(0);
                let region_y = atlas_size
                    .map(|size| (image.uv[1] * size[1] as f32 - 0.5).max(0.0) as u32)
                    .unwrap_or(0);
                serde_json::json!({
                    "image_id": image_id,
                    "resident": true,
                    "texture_index": image.slot,
                    "generation": self.image_atlas_generation,
                    "atlas_size": atlas_size,
                    "region": {
                        "x": region_x,
                        "y": region_y,
                        "width": image.width,
                        "height": image.height,
                    },
                    "uv": image.uv,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "atlas_ready": self.image_atlas.is_some(),
            "resident_count": self.resident_images.len(),
            "sampled_images": sampled_images,
            "external_images": external_images,
            "atlas_generation": self.image_atlas_generation,
        })
    }

    pub(crate) fn has_active_animation(&mut self, time_seconds: f32) -> bool {
        let mut completed = Vec::new();
        self.active.retain(|id, active| {
            let end = active.started_at_seconds
                + (active.transition.delay_ms + active.transition.duration_ms) as f32 / 1000.0;
            if time_seconds < end {
                true
            } else {
                self.animation_history
                    .push_back(animation_instance_from_active(
                        id,
                        active,
                        UiAnimationStatus::Completed,
                    ));
                completed.push((
                    id.clone(),
                    active.target.clone(),
                    active.transition.motion_key.clone(),
                    active.started_at_seconds,
                    active.transition.duration_ms,
                ));
                false
            }
        });
        for (node, target, motion_key, start_seconds, duration_ms) in completed {
            if self.trace_role != "screen" && is_world_panel_path(&node) {
                eprintln!(
                    "{}",
                    json!({
                        "event": "world_ui_transition_end",
                        "node_path": node,
                        "motion_key": motion_key,
                        "start_seconds": start_seconds,
                        "end_seconds": time_seconds,
                        "duration_ms": duration_ms,
                    })
                );
            }
            // Pin the rendered value to the exact target. Without this the
            // last in-flight interpolation (progress < 1.0) survives in
            // `current`, so the next `sample` treats it as a fresh source and
            // re-starts a no-op motion against the still-present
            // `enter_transition` — the endless start/complete churn.
            self.current.insert(node.clone(), target);
        }
        !self.active.is_empty() || time_seconds < self.pressed_until_seconds
    }

    pub(crate) fn cancel_animation(&mut self, node_path: &str) -> bool {
        let Some(active) = self.active.remove(node_path) else {
            return false;
        };
        self.animation_history
            .push_back(animation_instance_from_active(
                node_path,
                &active,
                UiAnimationStatus::Cancelled,
            ));
        while self.animation_history.len() > 64 {
            self.animation_history.pop_front();
        }
        self.current.insert(node_path.to_owned(), active.target);
        true
    }

    /// Renderer-side snapshot of every in-flight transition. `node_key` is the
    /// stable plan path (fragment_id/node_id), `motion_key` is the Flow motion
    /// identity attached by the UI runtime (renderer never interprets it),
    /// `elapsed_ms`/`duration_ms` drive progress, and `target` describes the
    /// destination visual. Purely diagnostic; never read by frame production.
    pub(crate) fn active_transition_debug_snapshot(&self, time_seconds: f32) -> Value {
        let mut transitions = self
            .active
            .iter()
            .map(|(node_key, active)| {
                let elapsed_ms = ((time_seconds - active.started_at_seconds) * 1000.0).max(0.0);
                let status = if transition_finished(active, time_seconds) {
                    UiAnimationStatus::Completed
                } else {
                    UiAnimationStatus::Running
                };
                json!({
                    "node_key": node_key,
                    "motion_key": active.transition.motion_key,
                    "elapsed_ms": elapsed_ms,
                    "delay_ms": active.transition.delay_ms,
                    "duration_ms": active.transition.duration_ms,
                    "easing": format_easing(active.transition.easing),
                    "progress": ((elapsed_ms - active.transition.delay_ms as f32)
                        / active.transition.duration_ms as f32)
                        .clamp(0.0, 1.0),
                    "status": format!("{status:?}"),
                    "target": {
                        "bounds": {
                            "x": active.target.bounds.x,
                            "y": active.target.bounds.y,
                            "width": active.target.bounds.width,
                            "height": active.target.bounds.height,
                        },
                        "opacity": active.target.style.opacity,
                        "numeric_value": match &active.target.presentation {
                            Some(UiControlPresentation::Numeric { value, min, max }) => {
                                json!({"value": value, "min": min, "max": max})
                            }
                            _ => Value::Null,
                        },
                    },
                })
            })
            .collect::<Vec<_>>();
        transitions
            .sort_by(|left, right| left["node_key"].as_str().cmp(&right["node_key"].as_str()));
        let history = self
            .animation_history
            .iter()
            .map(|animation| {
                json!({
                    "node_key": animation.node_path,
                    "status": format!("{:?}", animation.status),
                    "motion_key": animation.spec.motion_key,
                    "started_at_seconds": animation.started_at_seconds,
                })
            })
            .collect::<Vec<_>>();
        json!({ "count": transitions.len(), "transitions": transitions, "history": history })
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
                slot: 0,
                width,
                height,
                bytes: content.bytes.clone(),
                uv: [0.0; 4],
            },
        );
        self.rebuild_image_atlas(device, queue);
        Ok(())
    }

    /// Uploads an external-engine image into the same renderer-owned atlas as
    /// project images. The source is not interpreted as an AssetRef.
    pub(crate) fn preload_external_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &UiImageSource,
    ) -> Result<UiImageTextureRef, &'static str> {
        let Some(byte_len) = (source.width as usize)
            .checked_mul(source.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return Err("invalid_image_bytes");
        };
        if source.image_id.trim().is_empty()
            || source.media_type != "application/x-neon-rgba8"
            || source.width == 0
            || source.height == 0
            || source.width > IMAGE_ATLAS_WIDTH - IMAGE_ATLAS_PADDING * 2
            || source.bytes.len() != byte_len
        {
            return Err("invalid_image_source");
        }
        self.external_images.insert(
            source.image_id.clone(),
            ResidentImage {
                slot: 0,
                width: source.width,
                height: source.height,
                bytes: source.bytes.clone(),
                uv: [0.0; 4],
            },
        );
        self.rebuild_image_atlas(device, queue);
        let image = self
            .external_images
            .get(&source.image_id)
            .expect("external image was inserted");
        let atlas_size = self.image_atlas_size().unwrap_or([IMAGE_ATLAS_WIDTH, 1]);
        let region = UiImageTextureRegion {
            x: (image.uv[0] * atlas_size[0] as f32 - 0.5).max(0.0) as u32,
            y: (image.uv[1] * atlas_size[1] as f32 - 0.5).max(0.0) as u32,
            width: image.width,
            height: image.height,
        };
        Ok(UiImageTextureRef {
            image_id: source.image_id.clone(),
            texture_index: image.slot,
            generation: self.image_atlas_generation,
            atlas_size,
            region,
            uv: image.uv,
        })
    }

    fn rebuild_image_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut keys = self.resident_images.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut external_keys = self.external_images.keys().cloned().collect::<Vec<_>>();
        external_keys.sort();
        enum ImagePlacement {
            Project((String, u64, u64)),
            External(String),
        }
        let mut placements = Vec::with_capacity(keys.len() + external_keys.len());
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
            placements.push((ImagePlacement::Project(key.clone()), x, y));
            x += image.width + IMAGE_ATLAS_PADDING;
            row_height = row_height.max(image.height);
        }
        for key in &external_keys {
            let image = &self.external_images[key];
            if x + image.width + IMAGE_ATLAS_PADDING > IMAGE_ATLAS_WIDTH {
                x = IMAGE_ATLAS_PADDING;
                y += row_height + IMAGE_ATLAS_PADDING;
                row_height = 0;
            }
            placements.push((ImagePlacement::External(key.clone()), x, y));
            x += image.width + IMAGE_ATLAS_PADDING;
            row_height = row_height.max(image.height);
        }
        let atlas_height = (y + row_height + IMAGE_ATLAS_PADDING).max(1);
        self.image_atlas_generation = self.image_atlas_generation.saturating_add(1).max(1);
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
        for (placement, x, y) in placements {
            let image = match &placement {
                ImagePlacement::Project(key) => self
                    .resident_images
                    .get_mut(key)
                    .expect("image placement has a resident image"),
                ImagePlacement::External(key) => self
                    .external_images
                    .get_mut(key)
                    .expect("external image placement has a resident image"),
            };
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
        for (slot, placement) in keys
            .into_iter()
            .map(ImagePlacement::Project)
            .chain(external_keys.into_iter().map(ImagePlacement::External))
            .enumerate()
        {
            match placement {
                ImagePlacement::Project(key) => {
                    if let Some(image) = self.resident_images.get_mut(&key) {
                        image.slot = slot as u32;
                    }
                }
                ImagePlacement::External(key) => {
                    if let Some(image) = self.external_images.get_mut(&key) {
                        image.slot = slot as u32;
                    }
                }
            }
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("neon3-ui-image-atlas-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
            size: [IMAGE_ATLAS_WIDTH, atlas_height],
        });
    }

    fn image_atlas_size(&self) -> Option<[u32; 2]> {
        self.image_atlas.as_ref().map(|atlas| atlas.size)
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
            // Keep glyph edges crisp at authored logical sizes. Text layout
            // remains unchanged; only atlas sampling becomes less blurry.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
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
        mode: UiDrawMode,
    ) {
        /// Whether a sampled visual belongs to the requested emission subset.
        fn sampled_in_mode(visual: &UiVisual, mode: UiDrawMode) -> bool {
            match mode {
                UiDrawMode::All => true,
                UiDrawMode::World => visual.world_depth.is_some(),
                UiDrawMode::Screen => visual.world_depth.is_none(),
                UiDrawMode::BehindGlass => visual.world_depth.is_none(),
            }
        }
        self.ensure_builtin_font(device, queue);
        self.update_viewport(viewport_physical_size, viewport_logical_size);
        // The only per-frame animation upload is this 16-byte view uniform.
        // Every transition record remains resident in the instance buffer and
        // WGSL samples it from the monotonic clock.
        queue.write_buffer(
            &self.view_buffer,
            0,
            bytemuck::bytes_of(&UiView {
                viewport: self.viewport_logical_size,
                // Bevy's HDR camera keeps the post-tonemap target in linear
                // display space until the final surface encode.
                color_mode: 0,
                time_seconds,
                extras: get_global_view_extras(),
            }),
        );
        self.view_buffer_viewport_revision = self.viewport_revision;
        let stage = Instant::now();
        self.refresh_plan(fragments, viewport_logical_size);
        let refresh_plan_ms = stage.elapsed().as_secs_f32() * 1000.0;
        self.instances.clear();
        let stage = Instant::now();
        let top_layer = self.compose_sampled_visuals(time_seconds);
        let visible_top_layer = top_layer
            .iter()
            .map(|root| {
                root.is_none_or(|root| {
                    self.plan[root].target.kind != UiNodeKind::Tooltip || self.tooltip_hovered(root)
                })
            })
            .collect::<Vec<_>>();
        let compose_visuals_ms = stage.elapsed().as_secs_f32() * 1000.0;
        let mut buffer_upload_ms = 0.0_f32;
        let plan_index = self
            .plan
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect::<HashMap<_, _>>();
        // Ordinary UI stays in document order. The captured drag subtree is held
        // for the final screen-space batch below so no later panel, popup, modal,
        // or component chrome can occlude the item under the pointer.
        let mut drag_preview_instances = Vec::new();
        let mut material_instances = BTreeMap::<u32, BTreeMap<String, Vec<UiInstance>>>::new();
        for index in 0..self.plan.len() {
            if self.plan[index].instance_index.is_none()
                || top_layer[index].is_some()
                || !sampled_in_mode(&self.sampled[index], mode)
                || !composition_layer_is(
                    self.composition_layers
                        .get(&self.plan[index].id)
                        .copied()
                        .unwrap_or_default(),
                    mode,
                )
            {
                continue;
            }
            let visual = &self.sampled[index];
            let instance = self.instance(visual, &self.plan[index].id, time_seconds);
            let chrome = self.component_chrome_instances(visual, &self.plan[index].id);
            let material_instance = self.node_materials.get(
                self.plan[index].id.rsplit('/').next().unwrap_or(self.plan[index].id.as_str()),
            ).and_then(|material| self.material_pipelines.contains_key(&material.package_id)
                .then(|| (material.package_id.clone(), self.material_instance(visual, &self.plan[index].id, material, time_seconds))));
            let destination = if self.drag_offset_for_node(index, &plan_index).is_some() {
                &mut drag_preview_instances
            } else {
                &mut self.instances
            };
            destination.push(instance);
            if let Some((package_id, material)) = material_instance {
                material_instances
                    .entry(visual.paint_group_id)
                    .or_default()
                    .entry(package_id)
                    .or_default()
                    .push(material);
            }
            destination.extend(chrome);
        }
        // Material draws are recorded into one command buffer, so each package
        // needs a stable range in a single upload. Writing different package
        // instances repeatedly into one buffer would leave earlier draws
        // observing the last write when the GPU executes the submitted frame.
        let mut material_batches = BTreeMap::<u32, Vec<(String, u32, u32)>>::new();
        let mut material_payload = Vec::<UiInstance>::new();
        for (group_id, packages) in &material_instances {
            for (package_id, instances) in packages {
                if instances.is_empty() {
                    continue;
                }
                let start = material_payload.len() as u32;
                material_payload.extend_from_slice(instances);
                material_batches.entry(*group_id).or_default().push((
                    package_id.clone(),
                    start,
                    instances.len() as u32,
                ));
            }
        }
        // CPU first-press handling must be ready as soon as the visible frame is
        // drawn; asynchronous GPU hit readback is only supplemental.
        self.refresh_hit_bindings(fragments);
        // Dropdown/modal/tooltip chrome is screen-UI presentation; the world
        // target never carries it. World panels are only emitted through the
        // ordinary instance loop above, so a World pass emits zero popups.
        let mut popup_instances = if mode == UiDrawMode::World {
            Vec::new()
        } else {
            self.dropdown_popup_instances()
        };
        for index in 0..self.plan.len() {
            if self.plan[index].target.scroll && sampled_in_mode(&self.sampled[index], mode) {
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
            if mode == UiDrawMode::World {
                // Modal/dialog backdrops and popup chrome belong to screen UI.
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
            if self.plan[index].instance_index.is_some()
                && sampled_in_mode(&self.sampled[index], mode)
            {
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
        // Drag preview is the final composited screen layer. It is intentionally
        // renderer-local and does not mutate the canonical UI tree.
        popup_instances.extend(drag_preview_instances);
        self.pointer_visual_dirty = false;
        if mode != UiDrawMode::World {
            // Text is drawn after ordinary rect instances. Keep the caret and
            // selection above glyphs so a focused virtual-list cell has a
            // visible insertion point.
            popup_instances.extend(self.text_input_overlay_instances());
        }
        self.last_panel_instance_count = self.instances.len();
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
            self.uploaded_instances.clear();
        }
        if self.instance_capacity > self.depth_instance_capacity {
            self.depth_instance_capacity = self.instance_capacity;
            self.depth_instance_buffer =
                create_instance_buffer(device, self.depth_instance_capacity);
            self.uploaded_depth_instances.clear();
        }
        if material_payload.len() > self.material_instance_capacity {
            self.material_instance_capacity = material_payload.len().next_power_of_two();
            self.material_instance_buffer =
                create_instance_buffer(device, self.material_instance_capacity);
        }
        if popup_instances.len() > self.popup_instance_capacity {
            self.popup_instance_capacity = popup_instances.len().next_power_of_two();
            self.popup_instance_buffer =
                create_instance_buffer(device, self.popup_instance_capacity);
        }
        // Transition records are immutable after begin/retarget. During the
        // animation WGSL samples `animation` from the time uniform, so avoid
        // re-uploading panel data every frame.
        if self.instances != self.uploaded_instances {
            let stage = Instant::now();
            queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.instances),
            );
            self.uploaded_instances.clone_from(&self.instances);
            buffer_upload_ms += stage.elapsed().as_secs_f32() * 1000.0;
        }
        if !material_payload.is_empty() {
            let stage = Instant::now();
            queue.write_buffer(
                &self.material_instance_buffer,
                0,
                bytemuck::cast_slice(&material_payload),
            );
            buffer_upload_ms += stage.elapsed().as_secs_f32() * 1000.0;
        }
        let mut images = Vec::new();
        let mut popup_images = Vec::new();
        for (index, visual) in self.sampled.iter().enumerate() {
            let image = (|| {
                if !sampled_in_mode(visual, mode) {
                    return None;
                }
                let asset = visual.image.as_ref()?;
                let image = if let Some(resource_key) = asset.project_id.strip_prefix("external:") {
                    self.external_images.get(resource_key)
                } else {
                    let key = (asset.project_id.clone(), asset.asset_id, asset.revision.0);
                    self.resident_images.get(&key)
                }?;
                let node_key = self.plan[index]
                    .id
                    .rsplit('/')
                    .next()
                    .unwrap_or(self.plan[index].id.as_str());
                let nine_slice = self.nine_slices.get(node_key).copied();
                if nine_slice
                    .is_some_and(|layout| !layout.validate_for_image(image.width, image.height))
                {
                    return None;
                }
                let (source_insets, mut target_insets, mode, fill_center) = nine_slice
                    .map(|layout| {
                        let scale = visual.world_scale.unwrap_or(1.0);
                        (
                            layout.source_insets_px.map(|value| value as f32),
                            layout.target_insets.map(|value| value * scale),
                            match layout.mode {
                                neon_ui_schema::UiNineSliceMode::Stretch => 0,
                                neon_ui_schema::UiNineSliceMode::Tile => 1,
                                neon_ui_schema::UiNineSliceMode::Mirror => 2,
                            },
                            u32::from(layout.fill_center),
                        )
                    })
                    .unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                for value in &mut target_insets {
                    *value = value.max(0.0);
                }
                let fit = self.image_fits.get(node_key).copied().unwrap_or(UiImageFit::Stretch);
                let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
                Some(UiImageInstance {
                    rect,
                    tint: [1.0, 1.0, 1.0, visual.style.opacity],
                    clip: [
                        visual.clip.x,
                        visual.clip.y,
                        visual.clip.x + visual.clip.width,
                        visual.clip.y + visual.clip.height,
                    ],
                    uv,
                    depth: color_pass_depth(visual.world_depth),
                    paint_group_id: self.plan[index].paint_group_id,
                    source_insets,
                    target_insets,
                    mode,
                    fill_center,
                    _padding: [0; 2],
                })
            })();
            let Some(image) = image else {
                continue;
            };
            // Modal, dialog, and tooltip panels are emitted in the popup pass
            // after the ordinary image batch. Their images must follow their
            // own opaque panel rectangles, or those rectangles cover them.
            if top_layer[index].is_some() {
                if !visible_top_layer[index] {
                    continue;
                }
                popup_images.push(image);
            } else {
                images.push(image);
            }
        }
        // Skin bodies paint after the standard control rectangle, replacing its
        // visual treatment without changing the logical button bounds or hits.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Button || !sampled_in_mode(visual, mode) {
                continue;
            }
            let node_path = &self.plan[index].id;
            let Some(skin_key) = self.skin_references.get(node_path) else {
                continue;
            };
            let Some(skin) = self.skins.get(skin_key) else {
                continue;
            };
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            let Some(slot) = select_button_skin_slot(skin, hovered, pressed, enabled) else {
                continue;
            };
            let (resource_key, fit, nine_slice) = match &slot.presentation {
                UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                UiSkinPresentation::NineSlice { resource_key, layout } => {
                    (resource_key, UiImageFit::Stretch, Some(*layout))
                }
                _ => continue,
            };
            let binding_key = format!("{skin_key}/{resource_key}");
            let image = self.skin_assets.get(&binding_key).and_then(|asset| {
                self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))
            }).or_else(|| {
                self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))
            });
            let Some(image) = image else {
                continue;
            };
            if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                continue;
            }
            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (
                layout.source_insets_px.map(|value| value as f32),
                layout.target_insets,
                match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 },
                u32::from(layout.fill_center),
            )).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
            let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
            images.push(UiImageInstance {
                rect,
                tint: [1.0, 1.0, 1.0, visual.style.opacity],
                clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height],
                uv,
                depth: color_pass_depth(visual.world_depth),
                paint_group_id: self.plan[index].paint_group_id,
                source_insets,
                target_insets,
                mode: slice_mode,
                fill_center,
                _padding: [0; 2],
            });
        }
        // Slider skins replace the standard track/fill/thumb chrome only. The
        // slider visual and its hit bounds remain the authored logical bounds.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Slider || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let normalized = match &visual.presentation {
                Some(UiControlPresentation::Numeric { value, min, max }) => ((value - min) / (max - min)).clamp(0.0, 1.0),
                _ => continue,
            };
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            // Track height and thumb size scale with component height for distinct skin variants
            let track_h = (visual.bounds.height * 0.18).clamp(2.0, 8.0);
            let thumb_s = (visual.bounds.height * 0.55).clamp(8.0, 24.0);
            let track = UiBounds { x: visual.bounds.x + 12.0, y: visual.bounds.y + visual.bounds.height * 0.5 - track_h * 0.5, width: (visual.bounds.width - 24.0).max(1.0), height: track_h };
            let fill = UiBounds { x: track.x, y: track.y, width: track.width * normalized, height: track.height };
            let thumb = UiBounds { x: track.x + track.width * normalized - thumb_s * 0.5, y: track.y + track_h * 0.5 - thumb_s * 0.5, width: thumb_s, height: thumb_s };
            for (slot_kind, bounds) in [(UiSkinSlotKind::Track, track), (UiSkinSlotKind::Fill, fill), (UiSkinSlotKind::Thumb, thumb)] {
                let Some(slot) = select_slider_skin_slot(skin, slot_kind, hovered, pressed, enabled) else { continue };
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
                let Some(image) = image else { continue };
                if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
                let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                let (rect, uv) = fit_image_rect_and_uv(bounds, image.uv, image.width, image.height, fit);
                images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
            }
        }
        // Panel / Dialog / ContextMenu / Splitter / ListBox / Modal / TreeView
        // / Toast / MenuBar / Accordion / Spinner / Divider skins replace the
        // standard fill with a skinned body image. These are non-interactive
        // body-only components; only the Normal state is consulted.
        for (index, visual) in self.sampled.iter().enumerate() {
            if !matches!(visual.kind, UiNodeKind::Panel | UiNodeKind::Dialog | UiNodeKind::ContextMenu | UiNodeKind::Splitter | UiNodeKind::ListBox | UiNodeKind::Modal | UiNodeKind::TreeView | UiNodeKind::Toast | UiNodeKind::MenuBar | UiNodeKind::Accordion | UiNodeKind::Spinner | UiNodeKind::Divider)
                || !sampled_in_mode(visual, mode)
            {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let Some(slot) = skin.slots.iter().find(|slot| slot.slot_kind == UiSkinSlotKind::Body && slot.state == UiVisualState::Normal) else { continue };
            let (resource_key, fit, nine_slice) = match &slot.presentation {
                UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                _ => continue,
            };
            let binding_key = format!("{skin_key}/{resource_key}");
            let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
            let Some(image) = image else { continue };
            if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
            let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
        }
        // Switch skins render track + sliding thumb. Thumb position is driven by
        // the Toggle presentation (selected = on). Track and thumb support
        // Normal / Hover / Pressed / Disabled via select_toggle_skin_slot.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Switch || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let selected = matches!(&visual.presentation, Some(UiControlPresentation::Toggle { selected }) if *selected);
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            let track = visual.bounds;
            let thumb_size = track.height.min(24.0).max(8.0);
            let thumb_x = if selected { track.x + track.width - thumb_size - 2.0 } else { track.x + 2.0 };
            let thumb = UiBounds { x: thumb_x, y: track.y + (track.height - thumb_size) * 0.5, width: thumb_size, height: thumb_size };
            for (slot_kind, bounds) in [(UiSkinSlotKind::Track, track), (UiSkinSlotKind::Thumb, thumb)] {
                let Some(slot) = select_toggle_skin_slot(skin, slot_kind, hovered, pressed, enabled) else { continue };
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
                let Some(image) = image else { continue };
                if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
                let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                let (rect, uv) = fit_image_rect_and_uv(bounds, image.uv, image.width, image.height, fit);
                images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
            }
        }
        // ProgressBar skins render track + fill using the normalized value.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::ProgressBar || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let normalized = match &visual.presentation {
                Some(UiControlPresentation::Numeric { value, min, max }) => ((value - min) / (max - min)).clamp(0.0, 1.0),
                _ => 0.0,
            };
            let track = visual.bounds;
            let fill = UiBounds { x: track.x, y: track.y, width: track.width * normalized, height: track.height };
            for (slot_kind, bounds) in [(UiSkinSlotKind::Track, track), (UiSkinSlotKind::Fill, fill)] {
                let target_state = if slot_kind == UiSkinSlotKind::Fill { UiVisualState::Active } else { UiVisualState::Normal };
                let Some(slot) = skin.slots.iter().find(|slot| slot.slot_kind == slot_kind && slot.state == target_state) else { continue };
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
                let Some(image) = image else { continue };
                if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
                let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                let (rect, uv) = fit_image_rect_and_uv(bounds, image.uv, image.width, image.height, fit);
                images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
            }
        }
        // Scrollbar skins render track + thumb using the normalized scroll value.
        // Track and thumb each support Normal / Hover / Pressed states.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Scrollbar || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let normalized = match &visual.presentation {
                Some(UiControlPresentation::Numeric { value, min, max }) => ((value - min) / (max - min)).clamp(0.0, 1.0),
                _ => 0.0,
            };
            let horizontal = visual.bounds.width > visual.bounds.height;
            let track = visual.bounds;
            let thumb_size = if horizontal { visual.bounds.height.min(24.0) } else { visual.bounds.width.min(24.0) };
            let thumb = if horizontal {
                UiBounds { x: track.x + (track.width - thumb_size) * normalized, y: track.y, width: thumb_size, height: track.height }
            } else {
                UiBounds { x: track.x, y: track.y + (track.height - thumb_size) * normalized, width: track.width, height: thumb_size }
            };
            let pointer = self.pointer_position;
            let track_hovered = pointer.is_some_and(|p| contains(track, p));
            let thumb_hovered = pointer.is_some_and(|p| contains(thumb, p));
            let thumb_pressed = thumb_hovered && time_seconds < self.pressed_until_seconds;
            for (slot_kind, bounds, hovered, pressed) in [
                (UiSkinSlotKind::Track, track, track_hovered, false),
                (UiSkinSlotKind::Thumb, thumb, thumb_hovered, thumb_pressed),
            ] {
                let Some(slot) = select_scrollbar_skin_slot(skin, slot_kind, hovered, pressed) else { continue };
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
                let Some(image) = image else { continue };
                if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
                let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                let (rect, uv) = fit_image_rect_and_uv(bounds, image.uv, image.width, image.height, fit);
                images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
            }
        }
        // Checkbox skins render body + check mark based on selected state.
        // Body supports Normal / Hover / Pressed / Disabled; Fill (check mark) shows when selected.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Checkbox || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let selected = matches!(&visual.presentation, Some(UiControlPresentation::Toggle { selected }) if *selected);
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            // Box area: square on the left, vertically centered (matches default checkbox layout)
            let box_size = (visual.bounds.height - 4.0).max(10.0).min(24.0);
            let box_bounds = UiBounds {
                x: visual.bounds.x + 8.0,
                y: visual.bounds.y + (visual.bounds.height - box_size) * 0.5,
                width: box_size,
                height: box_size,
            };
            // Body (supports Disabled → Pressed → Hover → Normal fallback)
            if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Body, hovered, pressed, enabled) {
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                    if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                        let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                        let (rect, uv) = fit_image_rect_and_uv(box_bounds, image.uv, image.width, image.height, fit);
                        images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                    }
                }
            }
            // Check mark (only when selected) - use Fill slot, centered in box
            if selected {
                if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Fill, hovered, pressed, enabled)
                    .or_else(|| skin.slots.iter().find(|s| s.slot_kind == UiSkinSlotKind::Fill && s.state == UiVisualState::Active))
                {
                    let (resource_key, fit, nine_slice) = match &slot.presentation {
                        UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                        UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                        _ => continue,
                    };
                    let binding_key = format!("{skin_key}/{resource_key}");
                    if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                        if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                            let mark_size = box_size * 0.6;
                            let mark_bounds = UiBounds { x: box_bounds.x + (box_size - mark_size) * 0.5, y: box_bounds.y + (box_size - mark_size) * 0.5, width: mark_size, height: mark_size };
                            let (rect, uv) = fit_image_rect_and_uv(mark_bounds, image.uv, image.width, image.height, fit);
                            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                        }
                    }
                }
            }
        }
        // RadioButton skins render body + dot based on selected state.
        // Body supports Normal / Hover / Pressed / Disabled; Fill (dot) shows when selected.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::RadioButton || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let selected = matches!(&visual.presentation, Some(UiControlPresentation::Toggle { selected }) if *selected);
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            // Box area: square on the left, vertically centered (matches default radio layout)
            let box_size = (visual.bounds.height - 4.0).max(10.0).min(24.0);
            let box_bounds = UiBounds {
                x: visual.bounds.x + 8.0,
                y: visual.bounds.y + (visual.bounds.height - box_size) * 0.5,
                width: box_size,
                height: box_size,
            };
            // Body (supports Disabled → Pressed → Hover → Normal fallback)
            if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Body, hovered, pressed, enabled) {
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                    if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                        let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                        let (rect, uv) = fit_image_rect_and_uv(box_bounds, image.uv, image.width, image.height, fit);
                        images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                    }
                }
            }
            // Dot (only when selected) - use Fill slot, centered in box
            if selected {
                if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Fill, hovered, pressed, enabled)
                    .or_else(|| skin.slots.iter().find(|s| s.slot_kind == UiSkinSlotKind::Fill && s.state == UiVisualState::Active))
                {
                    let (resource_key, fit, nine_slice) = match &slot.presentation {
                        UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                        UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                        _ => continue,
                    };
                    let binding_key = format!("{skin_key}/{resource_key}");
                    if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                        if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                            let dot_size = box_size * 0.5;
                            let dot_bounds = UiBounds { x: box_bounds.x + (box_size - dot_size) * 0.5, y: box_bounds.y + (box_size - dot_size) * 0.5, width: dot_size, height: dot_size };
                            let (rect, uv) = fit_image_rect_and_uv(dot_bounds, image.uv, image.width, image.height, fit);
                            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                        }
                    }
                }
            }
        }
        // TextInput skins render body + focus ring based on focus state.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::TextInput || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let focused = self.editing.node_path.as_ref().is_some_and(|path| path == &self.plan[index].id);
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let body_state = if focused { UiVisualState::Active } else if hovered { UiVisualState::Hover } else { UiVisualState::Normal };
            // Body
            if let Some(slot) = skin.slots.iter().find(|slot| slot.slot_kind == UiSkinSlotKind::Body && slot.state == body_state) {
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                    if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                        let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                        let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
                        images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                    }
                }
            }
            // Focus ring (only when focused)
            if focused {
                if let Some(slot) = skin.slots.iter().find(|slot| slot.slot_kind == UiSkinSlotKind::FocusRing && slot.state == UiVisualState::Active) {
                    let (resource_key, fit, nine_slice) = match &slot.presentation {
                        UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                        UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                        _ => continue,
                    };
                    let binding_key = format!("{skin_key}/{resource_key}");
                    if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                        if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                            let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
                            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                        }
                    }
                }
            }
        }
        // Tooltip skins render body background only.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Tooltip || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let Some(slot) = skin.slots.iter().find(|slot| slot.slot_kind == UiSkinSlotKind::Body && slot.state == UiVisualState::Normal) else { continue };
            let (resource_key, fit, nine_slice) = match &slot.presentation {
                UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                _ => continue,
            };
            let binding_key = format!("{skin_key}/{resource_key}");
            let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
            let Some(image) = image else { continue };
            if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
            let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
        }
        // Combo / Dropdown / Tabs / Selectable skins render a body image with
        // hover / pressed / disabled state support (same fallback chain as Button).
        for (index, visual) in self.sampled.iter().enumerate() {
            if !matches!(visual.kind, UiNodeKind::Combo | UiNodeKind::Dropdown | UiNodeKind::Tabs | UiNodeKind::Selectable)
                || !sampled_in_mode(visual, mode)
            {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Body, hovered, pressed, enabled) else { continue };
            let (resource_key, fit, nine_slice) = match &slot.presentation {
                UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                _ => continue,
            };
            let binding_key = format!("{skin_key}/{resource_key}");
            let image = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id)));
            let Some(image) = image else { continue };
            if nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) { continue; }
            let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
            let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
            images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
        }
        // DragValue skins render a track background plus an optional body overlay.
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::DragValue || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(skin_key) = self.skin_references.get(&self.plan[index].id) else { continue };
            let Some(skin) = self.skins.get(skin_key) else { continue };
            let hovered = self.pointer_position.is_some_and(|position| contains(visual.bounds, position));
            let pressed = hovered && time_seconds < self.pressed_until_seconds;
            let enabled = visual.enabled;
            // Track (base background)
            if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Track, hovered, pressed, enabled) {
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                    if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                        let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                        let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
                        images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                    }
                }
            }
            // Body (overlay on top of track)
            if let Some(slot) = select_toggle_skin_slot(skin, UiSkinSlotKind::Body, hovered, pressed, enabled) {
                let (resource_key, fit, nine_slice) = match &slot.presentation {
                    UiSkinPresentation::Image { resource_key, fit } => (resource_key, *fit, None),
                    UiSkinPresentation::NineSlice { resource_key, layout } => (resource_key, UiImageFit::Stretch, Some(*layout)),
                    _ => continue,
                };
                let binding_key = format!("{skin_key}/{resource_key}");
                if let Some(image) = self.skin_assets.get(&binding_key).and_then(|asset| self.resident_images.get(&(asset.project_id.clone(), asset.asset_id, asset.revision.0))).or_else(|| self.skin_image_ids.get(&binding_key).and_then(|image_id| self.external_images.get(image_id))) {
                    if !nine_slice.is_some_and(|layout| !layout.validate_for_image(image.width, image.height)) {
                        let (source_insets, target_insets, slice_mode, fill_center) = nine_slice.map(|layout| (layout.source_insets_px.map(|value| value as f32), layout.target_insets, match layout.mode { neon_ui_schema::UiNineSliceMode::Stretch => 0, neon_ui_schema::UiNineSliceMode::Tile => 1, neon_ui_schema::UiNineSliceMode::Mirror => 2 }, u32::from(layout.fill_center))).unwrap_or(([0.0; 4], [0.0; 4], 0, 1));
                        let (rect, uv) = fit_image_rect_and_uv(visual.bounds, image.uv, image.width, image.height, fit);
                        images.push(UiImageInstance { rect, tint: [1.0, 1.0, 1.0, visual.style.opacity], clip: [visual.clip.x, visual.clip.y, visual.clip.x + visual.clip.width, visual.clip.y + visual.clip.height], uv, depth: color_pass_depth(visual.world_depth), paint_group_id: self.plan[index].paint_group_id, source_insets, target_insets, mode: slice_mode, fill_center, _padding: [0; 2] });
                    }
                }
            }
        }
        let image_capacity = images.len().max(popup_images.len());
        if image_capacity > self.image_capacity {
            self.image_capacity = image_capacity.next_power_of_two();
            self.image_buffer = create_image_buffer(device, self.image_capacity);
            self.popup_image_buffer = create_image_buffer(device, self.image_capacity);
        }
        // Do not upload `images` here. The sorted `ordered_images` payload below
        // is the only image batch consumed by the render pass; uploading this
        // unsorted vector first duplicated every image buffer write once per
        // frame, which became expensive as soon as image UI was present.
        let surfaces = self
            .sampled
            .iter()
            .filter_map(|visual| {
                if !sampled_in_mode(visual, mode) {
                    return None;
                }
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
                            depth: color_pass_depth(visual.world_depth),
                            paint_group_id: self
                                .plan
                                .iter()
                                .zip(self.sampled.iter())
                                .position(|(_, candidate)| std::ptr::eq(candidate, visual))
                                .map(|index| self.plan[index].paint_group_id)
                                .unwrap_or(0),
                            source_insets: [0.0; 4],
                            target_insets: [0.0; 4],
                            mode: 0,
                            fill_center: 1,
                            _padding: [0; 2],
                        },
                    ))
            })
            .collect::<Vec<_>>();
        // Canvas data stays declarative until this renderer-owned final draw
        // pass. Expand it directly into point/line GPU instances, never into
        // Panel nodes or an intermediate texture.
        let canvas_data = fragments
            .values()
            .flat_map(|fragment| {
                fragment
                    .effects
                    .iter()
                    .filter_map(move |effect| match effect {
                        neon_ui_schema::UiEffect::CanvasData { node_id, data } => {
                            Some((format!("{}/{}", fragment.fragment_id.0, node_id.0), data))
                        }
                        _ => None,
                    })
            })
            .collect::<HashMap<_, _>>();
        let mut canvas = Vec::<UiCanvasInstance>::new();
        for (index, visual) in self.sampled.iter().enumerate() {
            if visual.kind != UiNodeKind::Canvas || !sampled_in_mode(visual, mode) {
                continue;
            }
            let Some(data) = canvas_data.get(&self.plan[index].id) else {
                continue;
            };
            let scale = visual.world_scale.unwrap_or(1.0);
            let clip = [
                visual.clip.x,
                visual.clip.y,
                visual.clip.x + visual.clip.width,
                visual.clip.y + visual.clip.height,
            ];
            let origin = [visual.bounds.x, visual.bounds.y];
            let depth = color_pass_depth(visual.world_depth);
            let group = self.plan[index].paint_group_id;
            for point in &data.points {
                let position = [
                    origin[0] + point.position[0] * scale,
                    origin[1] + point.position[1] * scale,
                ];
                canvas.push(UiCanvasInstance {
                    start: position,
                    end: position,
                    color: point.color,
                    width: point.radius * 2.0 * scale,
                    kind: 0,
                    clip,
                    depth,
                    paint_group_id: group,
                });
            }
            for line in &data.lines {
                canvas.push(UiCanvasInstance {
                    start: [
                        origin[0] + line.start[0] * scale,
                        origin[1] + line.start[1] * scale,
                    ],
                    end: [
                        origin[0] + line.end[0] * scale,
                        origin[1] + line.end[1] * scale,
                    ],
                    color: line.color,
                    width: line.width * scale,
                    kind: 1,
                    clip,
                    depth,
                    paint_group_id: group,
                });
            }
        }
        let dropdown_texts = self.dropdown_option_texts();
        let list_box_texts = self.list_box_option_texts();
        let tab_texts = self.tab_option_texts();
        let drag_value_texts = self.drag_value_texts();
        let scroll_dynamic = (0..self.plan.len())
            .map(|index| Self::has_scroll_ancestor_in_plan(&self.plan, index))
            .collect::<Vec<_>>();
        let stage = Instant::now();
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
                        if !sampled_in_mode(visual, mode) {
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
                        ) {
                            return None;
                        }
                        // Rich text on non-top-layer nodes: layout directly.
                        // (Static text cache below is keyed on a plain &str,
                        // so rich spans take a separate path for now.)
                        if text.is_none() {
                            if let Some(TextRef::Rich { spans }) = visual.text.as_ref() {
                                return layout_rich_text(device, queue, font, visual, spans);
                            }
                            return None;
                        }
                        let text = text.unwrap();
                        // Static text cache: skip re-layout when the same
                        // node_path and text content were already computed.
                        // WorldUi text uses the same root scale as its panel;
                        // it is not independently auto-sized. Key the cache on
                        // the logical pre-projection geometry so projecting a
                        // panel at a different camera distance reuses the text
                        // layout instead of re-measuring it.
                        // includes the atlas generation so that new glyph
                        // rasterizations trigger a refresh.
                        let node_path = &self.plan[index].id;
                        let horizontal_scroll = (visual.kind == UiNodeKind::TextInput
                            && local_text.is_some())
                        .then_some(self.editing.horizontal_scroll);
                        let logical = visual.logical_bounds;
                        let cache_key = format!(
                            "{node_path}:{text}:{}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}",
                            self.atlas_generation,
                            logical.width,
                            logical.height,
                            logical.content_width,
                            logical.content_height,
                            logical.clip.map(|clip| [clip.width, clip.height]),
                            visual.world_scale,
                            horizontal_scroll,
                        );
                        // Scroll is a per-frame visual transform. A cached
                        // final instance contains rect/clip coordinates and
                        // must not be reused across scroll samples. Keep the
                        // logical cache for static nodes, but rebuild text in
                        // a scrolling subtree so the current panel position
                        // and viewport clip are authoritative.
                        if !scroll_dynamic[index]
                            && let Some(cached) = self.text_layout_cache.get(&cache_key)
                        {
                            let mut instances = cached.text_instances.clone();
                            let delta = [
                                visual.bounds.x - cached.visual_origin[0],
                                visual.bounds.y - cached.visual_origin[1],
                            ];
                            // Paint groups are assigned from the current
                            // flattened plan. World panels can change order as
                            // the camera moves, so a cached glyph layout must
                            // not retain the previous frame's group/depth.
                            // Keep glyph geometry cached, but refresh the
                            // frame-dependent ordering fields here.
                            let depth = color_pass_depth(visual.world_depth);
                            for instance in &mut instances {
                                instance.depth = depth;
                                instance.paint_group_id = visual.paint_group_id;
                            }
                            if let Some(clip) = text_clip(visual) {
                                for instance in &mut instances {
                                    instance.rect[0] += delta[0];
                                    instance.rect[1] += delta[1];
                                    instance.clip = clip;
                                }
                            }
                            return instances.into();
                        }
                        let instances =
                            layout_text(device, queue, font, visual, text, horizontal_scroll);
                        if let Some(instances) = instances {
                            self.layout_counters.text_layout_count =
                                self.layout_counters.text_layout_count.saturating_add(1);
                            if !scroll_dynamic[index] {
                                self.text_layout_cache.insert(
                                    cache_key,
                                    CachedTextLayout {
                                        text_instances: instances.clone(),
                                        visual_origin: [visual.bounds.x, visual.bounds.y],
                                    },
                                );
                            }
                            Some(instances)
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                let mut texts = texts;
                if mode != UiDrawMode::World {
                    for (visual, text) in &list_box_texts {
                        if let Some(instances) =
                            layout_text(device, queue, font, visual, text, None)
                        {
                            texts.extend(instances);
                        }
                    }
                    for (visual, text) in &tab_texts {
                        if let Some(instances) =
                            layout_text(device, queue, font, visual, text, None)
                        {
                            texts.extend(instances);
                        }
                    }
                    for (visual, text) in &drag_value_texts {
                        if let Some(instances) =
                            layout_text(device, queue, font, visual, text, None)
                        {
                            texts.extend(instances);
                        }
                    }
                }
                let mut popup_texts = if mode == UiDrawMode::World {
                    Vec::new()
                } else {
                    dropdown_texts
                        .iter()
                        .flat_map(|(visual, text)| {
                            layout_text(device, queue, font, visual, text, None).unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                };
                if mode != UiDrawMode::World {
                    for (index, visual) in self.sampled.iter().enumerate() {
                        if top_layer[index].is_none() {
                            continue;
                        }
                        if !visible_top_layer[index] {
                            continue;
                        }
                        if matches!(
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
                            ) {
                            match visual.text.as_ref() {
                                Some(TextRef::Rich { spans }) => {
                                    if let Some(instances) = layout_rich_text(device, queue, font, visual, spans) {
                                        popup_texts.extend(instances);
                                    }
                                }
                                Some(text) if text_ref_value(text).is_some() => {
                                    if let Some(instances) = layout_text(device, queue, font, visual, text_ref_value(text).unwrap(), None) {
                                        popup_texts.extend(instances);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                (texts, popup_texts)
            })
            .unwrap_or_default();
        let text_layout_ms = stage.elapsed().as_secs_f32() * 1000.0;
        if texts.len() > self.text_capacity {
            self.text_capacity = texts.len().next_power_of_two();
            self.text_buffer = create_text_buffer(device, self.text_capacity);
        }
        if popup_texts.len() > self.popup_text_capacity {
            self.popup_text_capacity = popup_texts.len().next_power_of_two();
            self.popup_text_buffer = create_text_buffer(device, self.popup_text_capacity);
        }
        let stage = Instant::now();
        if !texts.is_empty() {
            queue.write_buffer(&self.text_buffer, 0, bytemuck::cast_slice(&texts));
        }
        buffer_upload_ms += stage.elapsed().as_secs_f32() * 1000.0;
        // Treat equal world depth as one paint group. Groups are emitted far
        // to near; within a group the panel batch is emitted before its text
        // batch, so text stays above its owning panel while a nearer panel
        // still covers a farther group's text.
        let stage = Instant::now();
        let mut rect_groups: Vec<(u32, Vec<UiInstance>)> = Vec::new();
        for instance in &self.instances {
            let key = instance.paint_group_id;
            if let Some((_, group)) = rect_groups
                .iter_mut()
                .find(|(group_key, _)| *group_key == key)
            {
                group.push(*instance);
            } else {
                rect_groups.push((key, vec![*instance]));
            }
        }
        let mut text_groups: Vec<(u32, Vec<UiTextInstance>)> = Vec::new();
        for text in &texts {
            let key = text.paint_group_id;
            if let Some((_, group)) = text_groups
                .iter_mut()
                .find(|(group_key, _)| *group_key == key)
            {
                group.push(*text);
            } else {
                text_groups.push((key, vec![*text]));
            }
        }
        let mut image_groups: Vec<(u32, Vec<UiImageInstance>)> = Vec::new();
        for image in &images {
            let key = image.paint_group_id;
            if let Some((_, group)) = image_groups
                .iter_mut()
                .find(|(group_key, _)| *group_key == key)
            {
                group.push(*image);
            } else {
                image_groups.push((key, vec![*image]));
            }
        }
        let mut surface_groups: HashMap<u32, Vec<(String, UiImageInstance)>> = HashMap::new();
        for (surface_id, surface) in &surfaces {
            surface_groups
                .entry(surface.paint_group_id)
                .or_default()
                .push((surface_id.clone(), *surface));
        }
        let mut canvas_groups: Vec<(u32, Vec<UiCanvasInstance>)> = Vec::new();
        for instance in &canvas {
            if let Some((_, group)) = canvas_groups
                .iter_mut()
                .find(|(key, _)| *key == instance.paint_group_id)
            {
                group.push(*instance);
            } else {
                canvas_groups.push((instance.paint_group_id, vec![*instance]));
            }
        }
        let mut depth_keys = rect_groups
            .iter()
            .map(|(key, _)| *key)
            .chain(image_groups.iter().map(|(key, _)| *key))
            .chain(surface_groups.keys().copied())
            .chain(canvas_groups.iter().map(|(key, _)| *key))
            .chain(text_groups.iter().map(|(key, _)| *key))
            .collect::<Vec<_>>();
        let group_depths = self
            .plan
            .iter()
            .fold(HashMap::<u32, Option<f32>>::new(), |mut depths, node| {
                depths.entry(node.paint_group_id).or_insert(node.target.world_depth);
                depths
            });
        // World groups are emitted far-to-near. Screen groups have no GPU depth,
        // so their stable declaration order is the group-id tie-breaker.
        depth_keys.sort_by(|a, b| compare_paint_group_order(*a, *b, &group_depths));
        depth_keys.dedup();
        let mut ordered_rects = Vec::new();
        let mut ordered_images = Vec::new();
        let mut ordered_texts = Vec::new();
        let mut rect_ranges = HashMap::new();
        let mut image_ranges = HashMap::new();
        let mut text_ranges = HashMap::new();
        let mut ordered_canvas = Vec::new();
        let mut canvas_ranges = HashMap::new();
        let group_order = depth_keys.clone();
        for key in depth_keys {
            if let Some((_, group)) = rect_groups.iter().find(|(group_key, _)| *group_key == key) {
                let start = ordered_rects.len() as u32;
                ordered_rects.extend_from_slice(group);
                rect_ranges.insert(key, (start, group.len() as u32));
            }
            if let Some((_, group)) = image_groups.iter().find(|(group_key, _)| *group_key == key) {
                let start = ordered_images.len() as u32;
                ordered_images.extend_from_slice(group);
                image_ranges.insert(key, (start, group.len() as u32));
            }
            if let Some((_, group)) = text_groups.iter().find(|(group_key, _)| *group_key == key) {
                let start = ordered_texts.len() as u32;
                ordered_texts.extend_from_slice(group);
                text_ranges.insert(key, (start, group.len() as u32));
            }
            if let Some((_, group)) = canvas_groups
                .iter()
                .find(|(group_key, _)| *group_key == key)
            {
                let start = ordered_canvas.len() as u32;
                ordered_canvas.extend_from_slice(group);
                canvas_ranges.insert(key, (start, group.len() as u32));
            }
        }
        let group_sort_ms = stage.elapsed().as_secs_f32() * 1000.0;

        let stage = Instant::now();
        // The sorted ranges are the actual upload payload. Keep this guard
        // beside the write so later plan expansion cannot overrun a stale
        // capacity calculated from the unsorted snapshot.
        if ordered_rects.len() > self.instance_capacity {
            self.instance_capacity = ordered_rects.len().next_power_of_two();
            self.instance_buffer = create_instance_buffer(device, self.instance_capacity);
            self.depth_instance_capacity = self.instance_capacity;
            self.depth_instance_buffer =
                create_instance_buffer(device, self.depth_instance_capacity);
        }
        if ordered_images.len() > self.image_capacity {
            self.image_capacity = ordered_images.len().next_power_of_two();
            self.image_buffer = create_image_buffer(device, self.image_capacity);
            self.popup_image_buffer = create_image_buffer(device, self.image_capacity);
        }
        if ordered_texts.len() > self.text_capacity {
            self.text_capacity = ordered_texts.len().next_power_of_two();
            self.text_buffer = create_text_buffer(device, self.text_capacity);
        }
        if ordered_canvas.len() > self.canvas_capacity {
            self.canvas_capacity = ordered_canvas.len().next_power_of_two();
            self.canvas_buffer = create_canvas_buffer(device, self.canvas_capacity);
        }
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&ordered_rects),
        );
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        if !ordered_images.is_empty() {
            queue.write_buffer(&self.image_buffer, 0, bytemuck::cast_slice(&ordered_images));
            pass.set_bind_group(
                1,
                &self
                    .image_atlas
                    .as_ref()
                    .expect("resident image atlas")
                    .bind_group,
                &[],
            );
        }
        if !ordered_texts.is_empty() {
            queue.write_buffer(&self.text_buffer, 0, bytemuck::cast_slice(&ordered_texts));
        }
        if !ordered_canvas.is_empty() {
            queue.write_buffer(
                &self.canvas_buffer,
                0,
                bytemuck::cast_slice(&ordered_canvas),
            );
        }
        buffer_upload_ms += stage.elapsed().as_secs_f32() * 1000.0;
        for key in group_order {
            if let Some((start, count)) = rect_ranges.get(&key) {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
                pass.draw(0..6, *start..*start + *count);
            }
            // Each material is composited immediately after its host panel
            // group, before the group's imagery and glyphs. This makes glass
            // response read as a surface treatment instead of a foreground
            // filter over the player artwork and controls.
            if let Some(packages) = material_batches.get(&key) {
                for (package_id, start, count) in packages {
                    let Some(pipeline) = self.material_pipelines.get(package_id) else { continue };
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &self.view_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.material_instance_buffer.slice(..));
                    pass.draw(0..6, *start..*start + *count);
                }
            }
            if let Some((start, count)) = image_ranges.get(&key) {
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(
                    1,
                    &self
                        .image_atlas
                        .as_ref()
                        .expect("resident image atlas")
                        .bind_group,
                    &[],
                );
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                pass.draw(0..6, *start..*start + *count);
            }
            if let Some(group) = surface_groups.get(&key) {
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_vertex_buffer(0, self.image_buffer.slice(..));
                for (surface_id, surface) in group {
                    queue.write_buffer(&self.image_buffer, 0, bytemuck::bytes_of(surface));
                    pass.set_bind_group(
                        1,
                        &self.resident_render_surfaces[surface_id].bind_group,
                        &[],
                    );
                    pass.draw(0..6, 0..1);
                }
            }
            if let Some((start, count)) = canvas_ranges.get(&key) {
                pass.set_pipeline(&self.canvas_pipeline);
                pass.set_bind_group(0, &self.view_bind_group, &[]);
                pass.set_vertex_buffer(0, self.canvas_buffer.slice(..));
                pass.draw(0..6, *start..*start + *count);
            }
            if let Some((start, count)) = text_ranges.get(&key) {
                pass.set_pipeline(&self.text_pipeline);
                pass.set_bind_group(1, &self.resident_font.as_ref().unwrap().bind_group, &[]);
                pass.set_vertex_buffer(0, self.text_buffer.slice(..));
                pass.draw(0..6, *start..*start + *count);
            }
        }

        // Restore the complete color snapshot. External depth uses its own
        // buffer, so its later upload cannot mutate color-pass instances.
        let stage = Instant::now();
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
        if !popup_images.is_empty() {
            queue.write_buffer(
                &self.popup_image_buffer,
                0,
                bytemuck::cast_slice(&popup_images),
            );
            pass.set_pipeline(&self.image_pipeline);
            pass.set_bind_group(0, &self.view_bind_group, &[]);
            pass.set_bind_group(
                1,
                &self
                    .image_atlas
                    .as_ref()
                    .expect("resident image atlas")
                    .bind_group,
                &[],
            );
            pass.set_vertex_buffer(0, self.popup_image_buffer.slice(..));
            pass.draw(0..6, 0..popup_images.len() as u32);
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
        buffer_upload_ms += stage.elapsed().as_secs_f32() * 1000.0;
        self.last_stage_timings = UiDrawStageTimings {
            refresh_plan_ms,
            compose_visuals_ms,
            text_layout_ms,
            group_sort_ms,
            buffer_upload_ms,
        };
    }

    /// Zero the event ring counter before this frame's material passes write to it.
    pub(crate) fn begin_shader_event_frame(&self, queue: &wgpu::Queue) {
        queue.write_buffer(&self.event_buffer, 0, &0u32.to_le_bytes());
    }

    /// Copy the event ring into the staging buffer after all render passes.
    pub(crate) fn finish_shader_event_frame(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.copy_buffer_to_buffer(
            &self.event_buffer,
            0,
            &self.event_staging_buffer,
            0,
            SHADER_EVENT_BUFFER_SIZE,
        );
    }

    /// Map the staging buffer, parse emitted events, and store them for
    /// `take_shader_events`. Blocks until the GPU readback completes.
    pub(crate) fn read_shader_events(&mut self, device: &wgpu::Device) {
        let staging = &self.event_staging_buffer;
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        // recv_timeout avoids hanging the render thread forever if the GPU
        // readback stalls; without this the window freezes and the OS may
        // kill the process.
        let map_result = rx.recv_timeout(std::time::Duration::from_secs(2));
        let Ok(Ok(())) = map_result else {
            return;
        };
        let data = match slice.get_mapped_range() {
            Ok(range) => range,
            Err(_) => {
                staging.unmap();
                return;
            }
        };
        if data.len() < 4 {
            drop(data);
            staging.unmap();
            return;
        }
        let counter = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let _ = counter;
        let count = counter.min(SHADER_EVENT_CAPACITY);
        // WGSL layout: counter at 0, 12-byte pad, then events stride 32
        // (u32 id at +0, 12-byte pad, vec4 payload at +16).
        for i in 0..count {
            let base = 16 + i * 32;
            if base + 32 > data.len() {
                break;
            }
            let event_id = u32::from_le_bytes([
                data[base], data[base + 1], data[base + 2], data[base + 3],
            ]);
            let mut payload = [0.0f32; 4];
            for j in 0..4 {
                let p = base + 16 + j * 4;
                payload[j] = f32::from_le_bytes([
                    data[p], data[p + 1], data[p + 2], data[p + 3],
                ]);
            }
            self.pending_shader_events.push((event_id, payload));
        }
        drop(data);
        staging.unmap();
    }

    /// Drain and return all shader events read back since the last call.
    pub(crate) fn take_shader_events(&mut self) -> Vec<(u32, [f32; 4])> {
        std::mem::take(&mut self.pending_shader_events)
    }

    /// Update generic view-extras uniform data. Runtime does not interpret
    /// the content; shaders are free to use the 10 x vec4 slots as needed.
    pub(crate) fn set_view_extras(&mut self, extras: [[f32; 4]; 10]) {
        self.view_extras = extras;
    }

    pub(crate) fn last_stage_timings(&self) -> UiDrawStageTimings {
        self.last_stage_timings
    }

    pub(crate) fn layout_counters(&self) -> Value {
        json!({
            "layout_count": self.layout_counters.layout_count,
            "text_layout_count": self.layout_counters.text_layout_count,
            "world_transform_update_count": self.layout_counters.world_transform_update_count,
        })
    }

    /// Renderer-final layout diagnostics. Unlike a fragment snapshot, these
    /// values include resolved flex tracks, branch visibility, scroll/clip
    /// transforms, and final logical bounds used for drawing and hit testing.
    pub(crate) fn layout_snapshot(&self) -> Value {
        json!({
            "nodes": self.plan.iter().map(|node| {
                let visual = &node.target;
                json!({
                    "path": node.id,
                    "kind": node.target.kind,
                    "visible": visual.enabled && visual.bounds.width > 0.0 && visual.bounds.height > 0.0,
                    "bounds": {
                        "x": visual.logical_bounds.x,
                        "y": visual.logical_bounds.y,
                        "width": visual.logical_bounds.width,
                        "height": visual.logical_bounds.height,
                    },
                    "clip": {
                        "x": visual.clip.x,
                        "y": visual.clip.y,
                        "width": visual.clip.width,
                        "height": visual.clip.height,
                    },
                })
            }).collect::<Vec<_>>()
        })
    }

    pub(crate) fn last_panel_instance_count(&self) -> usize {
        self.last_panel_instance_count
    }

    pub(crate) fn canvas_diagnostics(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
        viewport: [f32; 2],
    ) -> Value {
        self.refresh_plan(fragments, viewport);
        let mut points = 0usize;
        let mut lines = 0usize;
        let mut canvas_nodes = 0usize;
        for fragment in fragments.values() {
            for effect in &fragment.effects {
                if let neon_ui_schema::UiEffect::CanvasData { data, .. } = effect {
                    points += data.points.len();
                    lines += data.lines.len();
                    canvas_nodes += 1;
                }
            }
        }
        json!({"canvas_nodes": canvas_nodes, "point_instances": points, "line_instances": lines, "total_instances": points + lines, "pipeline": "wgpu.ui.canvas.points_lines.v1"})
    }

    /// World-anchor projection changes node bounds without changing the UI
    /// program revision. External-surface rendering must rebuild the layout
    /// plan before consuming that projected snapshot.
    pub(crate) fn invalidate_plan(&mut self) {
        self.plan_revisions.clear();
        // The text layout cache is tied to the same fragment revision set.
        // When the plan is invalidated (e.g. camera/anchor projection change),
        // the text positions may have shifted, so clear the cache to force
        // re-layout on the next frame (plan §7.3).
        self.text_layout_cache.clear();
    }

    /// World anchor projection changes only the translated/scaled final visual.
    /// Preserve glyph layouts whose cache key is based on logical dimensions,
    /// then translate their instances to the current projected origin at draw
    /// time. A fragment revision or viewport change still uses `invalidate_plan`
    /// and clears the cache.
    pub(crate) fn invalidate_plan_for_world_transform(&mut self) {
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
    /// separate from the 2D painter order used by the color pass. Text glyphs do
    /// not write independent sparse depth: their occlusion belongs to the owning
    /// panel surface, so a far panel's text cannot survive over a nearer panel.
    pub(crate) fn draw_depth<'a>(
        &'a mut self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
    ) {
        let Some(rect_pipeline) = &self.depth_pipeline else {
            return;
        };
        // The exported convention is 0.0 = never occluded (always-visible)
        // and (0.0, 1.0) = Bevy-compatible reversed-Z depth
        // (near / view_distance). This target only ever carries projected world
        // panels, so the raw value is written unchanged. R32Float has no depth
        // test of its own, so emit complete groups far to near; the later near
        // group overwrites the earlier far value.
        let mut groups: HashMap<u32, Vec<UiInstance>> = HashMap::new();
        for instance in &self.instances {
            groups
                .entry(instance.paint_group_id)
                .or_default()
                .push(*instance);
        }
        let mut group_ids = groups.keys().copied().collect::<Vec<_>>();
        let group_depths = self
            .plan
            .iter()
            .fold(HashMap::<u32, Option<f32>>::new(), |mut depths, node| {
                depths.entry(node.paint_group_id).or_insert(node.target.world_depth);
                depths
            });
        group_ids.sort_by(|a, b| compare_paint_group_order(*a, *b, &group_depths));
        let mut ordered_depth_instances = Vec::new();
        let mut ranges = Vec::new();
        for group_id in group_ids {
            if let Some(group) = groups.get(&group_id) {
                let external_depth = group_depths
                    .get(&group_id)
                    .copied()
                    .flatten()
                    .unwrap_or(0.0);
                let start = ordered_depth_instances.len() as u32;
                ordered_depth_instances.extend(group.iter().map(|instance| UiInstance {
                    depth: external_depth,
                    ..*instance
                }));
                ranges.push((start, group.len() as u32));
            }
        }
        if ordered_depth_instances.is_empty() {
            return;
        }
        if self.uploaded_depth_instances != ordered_depth_instances {
            queue.write_buffer(
                &self.depth_instance_buffer,
                0,
                bytemuck::cast_slice(&ordered_depth_instances),
            );
            self.uploaded_depth_instances = ordered_depth_instances.clone();
        }
        pass.set_pipeline(rect_pipeline);
        pass.set_bind_group(0, &self.view_bind_group, &[]);
        pass.set_vertex_buffer(0, self.depth_instance_buffer.slice(..));
        for (start, count) in ranges {
            pass.draw(0..6, start..start + count);
        }
    }

    pub(crate) fn depth_diagnostics(&self) -> Value {
        let mut groups = HashMap::<u32, f32>::new();
        for node in &self.plan {
            if let Some(depth) = node.target.world_depth {
                groups.entry(node.paint_group_id).or_insert(depth);
            }
        }
        let mut depths = groups.values().copied().collect::<Vec<_>>();
        depths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        depths.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        json!({
            "groups": groups.len(),
            "distinct_depths": depths.len(),
            "min_depth": depths.first().copied().unwrap_or(0.0),
            "max_depth": depths.last().copied().unwrap_or(0.0),
            "depths": depths,
        })
    }

    /// Reports the renderer-owned paint groups after flattening and before the
    /// GPU batch ranges are consumed. This is intentionally diagnostic-only: it
    /// exposes the producer's group/depth/order values without making numeric
    /// renderer IDs part of the UI protocol.
    pub(crate) fn paint_order_diagnostics(&self) -> Value {
        let mut group_depths = HashMap::<u32, Option<f32>>::new();
        let mut group_nodes = BTreeMap::<u32, Vec<Value>>::new();
        for node in &self.plan {
            group_depths
                .entry(node.paint_group_id)
                .or_insert(node.target.world_depth);
            group_nodes
                .entry(node.paint_group_id)
                .or_default()
                .push(json!({
                    "id": node.id,
                    "parent_id": node.parent_id,
                    "kind": node.target.kind,
                    "world_depth": node.target.world_depth,
                }));
        }
        let mut group_order = group_nodes.keys().copied().collect::<Vec<_>>();
        group_order.sort_by(|left, right| {
            compare_paint_group_order(*left, *right, &group_depths)
        });
        let groups = group_order
            .iter()
            .filter_map(|group_id| {
                group_nodes.get(group_id).map(|nodes| {
                    json!({
                        "group_id": group_id,
                        "world_depth": group_depths.get(group_id).copied().flatten(),
                        "nodes": nodes,
                    })
                })
            })
            .collect::<Vec<_>>();
        json!({
            "policy": {
                "world": "far_to_near",
                "screen": "surface_declaration_order",
                "within_group": ["rect", "material", "image", "surface", "canvas", "text"],
            },
            "group_order": group_order,
            "groups": groups,
        })
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
        self.nine_slices.clear();
        self.node_cuts.clear();
        self.node_materials.clear();
        self.composition_layers.clear();
        self.image_fits.clear();
        self.skins.clear();
        self.skin_references.clear();
        self.skin_image_ids.clear();
        self.skin_assets.clear();
        for fragment in fragments.values() {
            for effect in &fragment.effects {
                match effect {
                    neon_ui_schema::UiEffect::NineSlice { node_id, layout } => {
                        self.nine_slices.insert(node_id.0.clone(), *layout);
                    }
                    neon_ui_schema::UiEffect::Geometry { node_id, geometry } => {
                        if !geometry.is_default() {
                            self.node_cuts.insert(node_id.0.clone(), geometry.cut);
                        }
                    }
                    neon_ui_schema::UiEffect::Material { node_id, material } => {
                        self.node_materials.insert(node_id.0.clone(), material.clone());
                    }
                    neon_ui_schema::UiEffect::CompositionLayer { node_id, layer } => {
                        self.composition_layers.insert(
                            format!("{}/{}", fragment.fragment_id.0, node_id.0),
                            *layer,
                        );
                    }
                    neon_ui_schema::UiEffect::ContextMenuBinding { node_id, context_menu_id } => {
                        self.context_menu_bindings.insert(
                            format!("{}/{}", fragment.fragment_id.0, node_id.0),
                            context_menu_id.clone(),
                        );
                    }
                    neon_ui_schema::UiEffect::ControlSkin { skin } => {
                        self.skins.insert(skin.key.clone(), skin.clone());
                    }
                    neon_ui_schema::UiEffect::SkinReference { node_id, skin_key } => {
                        self.skin_references.insert(
                            format!("{}/{}", fragment.fragment_id.0, node_id.0),
                            skin_key.clone(),
                        );
                    }
                    neon_ui_schema::UiEffect::SkinResourceBinding {
                        skin_key,
                        resource_key,
                        asset_ref,
                        image_id,
                    } => {
                        let key = format!("{skin_key}/{resource_key}");
                        if let Some(image_id) = image_id {
                            self.skin_image_ids.insert(key.clone(), image_id.clone());
                        }
                        if let Some(asset_ref) = asset_ref {
                            self.skin_assets.insert(key, asset_ref.clone());
                        }
                    }
                    _ => {}
                }
            }
            collect_image_fits(&fragment.root, &mut self.image_fits);
        }
        self.layout_counters.layout_count = self.layout_counters.layout_count.saturating_add(1);
        let display_fragments = self.data_grid_display_fragments(fragments);
        self.reconcile_data_grid_text_display_cache(&display_fragments);
        let nodes = flatten_fragments_with_data_grid_display_cache(
            &display_fragments,
            viewport_logical_size,
            self.resident_font.as_ref(),
            &self.data_grid_text_display_cache,
            &self.available_cameras,
        );
        #[cfg(target_os = "android")]
        {
            let layout_probe = nodes
                .iter()
                .filter(|(id, _, _, _)| {
                    id.ends_with("/gallery-layout")
                        || id.ends_with("/gallery-controls")
                        || id.ends_with("/asset-grid")
                        || id.ends_with("/field-pack")
                })
                .map(|(id, _, visual, _)| {
                    format!(
                        "{{\"id\":{:?},\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
                        id,
                        visual.bounds.x,
                        visual.bounds.y,
                        visual.bounds.width,
                        visual.bounds.height
                    )
                })
                .collect::<Vec<_>>();
            eprintln!(
                "{{\"probe\":\"android-component-gallery-visual-layout\",\"nodes\":[{}]}}",
                layout_probe.join(",")
            );
        }
        // Apply TreeView built-in expand/collapse state.
        // TreeView children are flat Label nodes; hierarchy is expressed by x indent.
        // A collapsed parent hides all subsequent more-indented siblings until the
        // next node at the same or lesser indent.
        let treeview_parents: HashSet<String> = nodes.iter()
            .filter(|(_, _, t, _)| matches!(t.kind, UiNodeKind::TreeView))
            .map(|(id, _, _, _)| id.clone())
            .collect();
        let parent_map: std::collections::HashMap<String, Option<String>> = nodes.iter()
            .map(|(id, pid, _, _)| (id.clone(), pid.clone()))
            .collect();
        // Collect IDs of hidden context menus for descendant filtering.
        // Only the active context menu is visible; all others are hidden.
        // Active ID may be stored without fragment prefix, so match by suffix.
        let active_suffix = self.active_context_menu_id.as_deref()
            .map(|active| format!("/{}", active));
        let is_active_ctx = |id: &str| -> bool {
            self.active_context_menu_id.as_deref().map_or(false, |active| {
                id == active || active_suffix.as_deref().map_or(false, |suf| id.ends_with(suf))
            })
        };
        let hidden_ctx_ids: std::collections::HashSet<String> = nodes.iter()
            .filter(|(id, _, target, _)| {
                matches!(target.kind, UiNodeKind::ContextMenu)
                    && (!self.context_menus_visible || !is_active_ctx(id))
            })
            .map(|(id, _, _, _)| id.clone())
            .collect();
        if self.context_menus_visible {
            let ctx_nodes: Vec<_> = nodes.iter()
                .filter(|(_, _, t, _)| matches!(t.kind, UiNodeKind::ContextMenu))
                .map(|(id, _, t, _)| (id.clone(), t.bounds))
                .collect();
            println!("[ctx-filter] visible={}, active={:?}, suffix={:?}, ctx_nodes={:?}, hidden={:?}",
                self.context_menus_visible, self.active_context_menu_id, active_suffix, ctx_nodes, hidden_ctx_ids);
        }
        let is_ctx_hidden = |id: &str| -> bool {
            let mut current = Some(id.to_string());
            while let Some(cid) = current {
                if hidden_ctx_ids.contains(&cid) {
                    return true;
                }
                current = parent_map.get(&cid).cloned().flatten();
            }
            false
        };
        // Compute position deltas for visible context menus (anchor placement).
        let mut ctx_deltas: std::collections::HashMap<String, (f32, f32)> = std::collections::HashMap::new();
        if let Some([ax, ay]) = self.context_menu_anchor {
            if self.context_menus_visible {
                for (id, _, target, _) in &nodes {
                    if matches!(target.kind, UiNodeKind::ContextMenu)
                        && !hidden_ctx_ids.contains(id)
                    {
                        ctx_deltas.insert(id.clone(), (ax - target.bounds.x, ay - target.bounds.y));
                    }
                }
            }
        }
        let ctx_delta_for = |id: &str| -> Option<(f32, f32)> {
            let mut current = Some(id.to_string());
            while let Some(cid) = current {
                if let Some(delta) = ctx_deltas.get(&cid) {
                    return Some(*delta);
                }
                current = parent_map.get(&cid).cloned().flatten();
            }
            None
        };
        let mut filtered_nodes = Vec::with_capacity(nodes.len());
        let mut skip_until_indent: Option<f32> = None;
        for (id, parent_id, mut target, transition) in nodes {
            // Skip hidden context menus and all their descendants.
            if is_ctx_hidden(&id) {
                if matches!(target.kind, UiNodeKind::ContextMenu) {
                    println!("[ctx-filter] SKIP ctx node: {}", id);
                }
                continue;
            }
            // Move visible context menu and descendants to anchor position.
            if let Some((dx, dy)) = ctx_delta_for(&id) {
                if matches!(target.kind, UiNodeKind::ContextMenu) {
                    println!("[ctx-filter] KEEP ctx node: {}, bounds=({:.0},{:.0},{:.0},{:.0}), delta=({:.0},{:.0})",
                        id, target.bounds.x, target.bounds.y, target.bounds.width, target.bounds.height, dx, dy);
                }
                target.bounds.x += dx;
                target.bounds.y += dy;
            }
            let parent_is_treeview = parent_id.as_ref()
                .is_some_and(|pid| treeview_parents.contains(pid));
            if parent_is_treeview && matches!(target.kind, UiNodeKind::Label) {
                let indent = target.bounds.x;
                if let Some(threshold) = skip_until_indent {
                    if indent > threshold {
                        continue; // hidden by collapsed parent
                    } else {
                        skip_until_indent = None;
                    }
                }
                // Check if this node is a collapsible parent (has children)
                let node_path = &id;
                let is_expanded = self.builtin_toggles.get(node_path).copied().unwrap_or(true);
                if !is_expanded {
                    // Find the next node at same or lesser indent to know where to stop
                    skip_until_indent = Some(indent);
                }
            } else {
                skip_until_indent = None;
            }
            filtered_nodes.push((id, parent_id, target, transition));
        }
        let nodes = filtered_nodes;
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
                paint_group_id: 0,
            });
        }
        self.plan_index.clear();
        self.plan_index.extend(
            self.plan
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id.clone(), index)),
        );
        for index in 0..self.plan.len() {
            let id = self.plan[index].id.clone();
            let inherited = self.plan[index]
                .parent_id
                .as_deref()
                .and_then(|parent| self.composition_layers.get(parent).copied())
                .unwrap_or_default();
            self.composition_layers.entry(id).or_insert(inherited);
        }
        // World snapshots have already had CameraVisibility effects consumed
        // by the host-side projection filter. Identify each projected panel by
        // its own inherited world depth instead of relying on those removed
        // effects; otherwise every panel collapses into one paint group and the
        // exported depth ring contains a single value. Screen roots are also
        // explicit: the fragment surface and each direct child are independent
        // painter groups, so a later panel carries its image/text with it.
        assign_paint_group_ids(&mut self.plan);
        for index in 0..self.plan.len() {
            let group_id = self.plan[index].paint_group_id;
            self.plan[index].target.paint_group_id = group_id;
            self.sampled[index].paint_group_id = group_id;
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
        self.hit_id_by_node.clear();
        let mut hit_nodes = Vec::new();
        for index in 0..self.plan.len() {
            let node_path = self.plan[index].id.clone();
            let hit_node_path = node_path.clone();
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
            if (!is_interactive_control(&kind)
                && !has_drag_binding
                && !declarations.contains_key(&node_path))
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
            self.hit_id_by_node.insert(hit_node_path, hit_id);
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

    fn text_input_overlay_instances(&self) -> Vec<UiInstance> {
        let mut overlays = Vec::new();
        let Some(node_path) = self.editing.node_path.as_ref() else {
            return overlays;
        };
        let Some(index) = self.plan.iter().position(|node| &node.id == node_path) else {
            return overlays;
        };
        let visual = &self.sampled[index];
        let Some(font) = self.resident_font.as_ref() else {
            return overlays;
        };
        let range = self.editing.selection_range();
        if !range.is_empty() {
            let start = text_advance(&font.font, &self.editing.committed, range.start)
                - self.editing.horizontal_scroll;
            let end = text_advance(&font.font, &self.editing.committed, range.end)
                - self.editing.horizontal_scroll;
            overlays.push(overlay_instance(
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
            overlays.push(overlay_instance(
                caret,
                input_clip(visual),
                [0.84, 0.98, 0.96, 1.0],
            ));
        }
        overlays
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
                        paint_group_id: 0,
                        ..UiInstance::zeroed()
                    },
                    UiInstance {
                        rect: [thumb.x, thumb.y, thumb.width, thumb.height],
                        fill: [0.34, 0.80, 0.64, 0.95],
                        border: [0.34, 0.80, 0.64, 0.95],
                        params: [0.0, 4.0, visual.style.opacity, visual.clip_radius],
                        clip,
                        depth: 0.0,
                        paint_group_id: 0,
                        ..UiInstance::zeroed()
                    },
                ])
            })
            .flatten()
            .collect()
    }

    fn sample_with_history(
        &mut self,
        id: &str,
        target: &UiVisual,
        transition: Option<&UiTransition>,
        time_seconds: f32,
    ) -> UiVisual {
        let superseded = self.active.get(id).cloned();
        let sampled = Self::sample(
            &mut self.current,
            &mut self.active,
            id,
            target,
            transition,
            time_seconds,
        );
        if let Some(previous) = superseded
            && transition.is_some()
            && previous.target != *target
        {
            self.animation_history
                .push_back(animation_instance_from_active(
                    id,
                    &previous,
                    UiAnimationStatus::Superseded,
                ));
            while self.animation_history.len() > 64 {
                self.animation_history.pop_front();
            }
        }
        sampled
    }

    fn sample(
        current: &mut HashMap<String, UiVisual>,
        active: &mut HashMap<String, ActiveTransition>,
        id: &str,
        target: &UiVisual,
        transition: Option<&UiTransition>,
        time_seconds: f32,
    ) -> UiVisual {
        // World anchor projection is a frame-local placement, not a UI motion.
        // Keep the presentation transition alive while snapping x/y to the
        // latest projected anchor so camera dragging cannot restart or lag
        // behind a size/color animation.
        if target.world_depth.is_some()
            && let Some(active_transition) = active.get_mut(id)
            && Self::same_world_visual_except_position(&active_transition.target, target)
        {
            if transition_finished(active_transition, time_seconds) {
                current.insert(id.to_owned(), target.clone());
                return target.clone();
            }
            active_transition.from.bounds.x = target.bounds.x;
            active_transition.from.bounds.y = target.bounds.y;
            active_transition.target.bounds.x = target.bounds.x;
            active_transition.target.bounds.y = target.bounds.y;
            current.insert(id.to_owned(), target.clone());
            return target.clone();
        }
        if let Some(active_transition) = active.get(id)
            && active_transition.target == *target
        {
            if transition_finished(active_transition, time_seconds) {
                current.insert(id.to_owned(), target.clone());
                return target.clone();
            }
            current.insert(id.to_owned(), target.clone());
            return target.clone();
        }
        // Retarget/cancel samples the old descriptor exactly once. This is the
        // only CPU interpolation point: steady-state frames leave the panel
        // record untouched and WGSL advances it from the time uniform.
        let source = active
            .get(id)
            .map(|active_transition| sample_transition(active_transition, time_seconds))
            .or_else(|| current.get(id).cloned());
        // The enter_transition stays on the fragment after the motion has
        // already completed. If the current rendered value already equals the
        // target, re-starting the motion is a pure no-op that only reprints
        // "start"/"complete" and re-enters `active`. Skip it and stay settled.
        if source.as_ref() == Some(target)
            || (target.world_depth.is_some()
                && source
                    .as_ref()
                    .is_some_and(|source| Self::same_world_visual_except_position(source, target)))
        {
            current.insert(id.to_owned(), target.clone());
            return target.clone();
        }
        // A subtree motion may be attached to numeric descendants so an
        // authoritative value can interpolate. Do not turn layout-only noise
        // on unchanged numeric controls into active transitions: the parent
        // panel already carries the structural motion, and these nodes would
        // otherwise restart on every revised layout sample.
        if let Some(transition) = transition
            && transition.from.bounds.is_none()
            && transition.from.background_color.is_none()
            && transition.from.border_color.is_none()
            && transition.from.border_width.is_none()
            && transition.from.corner_radius.is_none()
            && transition.from.opacity.is_none()
            && matches!(
                (&source, &target.presentation),
                (
                    Some(UiVisual {
                        presentation: Some(UiControlPresentation::Numeric { value: from, .. }),
                        ..
                    }),
                    Some(UiControlPresentation::Numeric { value: to, .. })
                ) if (*from - *to).abs() <= f32::EPSILON
            )
        {
            current.insert(id.to_owned(), target.clone());
            return target.clone();
        }
        let sampled = match transition {
            Some(transition) if transition.duration_ms > 0 => {
                let mut from = source.unwrap_or_else(|| transition_source(target, transition));
                if target.world_depth.is_some() {
                    from.bounds.x = target.bounds.x;
                    from.bounds.y = target.bounds.y;
                }
                let next_active = ActiveTransition {
                    from,
                    target: target.clone(),
                    started_at_seconds: time_seconds,
                    transition: transition.clone(),
                };
                active.insert(id.to_owned(), next_active);
                target.clone()
            }
            _ => target.clone(),
        };
        current.insert(id.to_owned(), sampled.clone());
        sampled
    }

    fn same_world_visual_except_position(left: &UiVisual, right: &UiVisual) -> bool {
        let mut left = left.clone();
        let mut right = right.clone();
        left.bounds.x = 0.0;
        left.bounds.y = 0.0;
        right.bounds.x = 0.0;
        right.bounds.y = 0.0;
        left.clip.x = 0.0;
        left.clip.y = 0.0;
        right.clip.x = 0.0;
        right.clip.y = 0.0;
        left == right
    }

    fn instance(&self, visual: &UiVisual, node_path: &str, time_seconds: f32) -> UiInstance {
        if node_path.contains("split") {
            splitter_debug(&format!("[INSTANCE] path={} kind={:?} bounds=({},{},{},{}) bg={:?}", node_path, visual.kind, visual.bounds.x, visual.bounds.y, visual.bounds.width, visual.bounds.height, visual.style.background_color));
        }
        let mut visual = visual.clone();
        // Apply persistent built-in toggle state (survives fragment re-submission)
        if let Some(&selected) = self.builtin_toggles.get(node_path)
            && matches!(
                visual.kind,
                UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
            )
        {
            visual.presentation = Some(UiControlPresentation::Toggle { selected });
        }
        // Transient value previews (drag in progress) override built-in state
        if let Some(UiSemanticPayloadValue::Bool { value }) = self.value_previews.get(node_path)
            && matches!(
                visual.kind,
                UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
            )
        {
            visual.presentation = Some(UiControlPresentation::Toggle { selected: *value });
        }
        let mut bounds = visual.bounds;
        let pointer_over = self
            .pointer_position
            .is_some_and(|position| contains(bounds, position));
        let selected = matches!(
            &visual.presentation,
            Some(UiControlPresentation::Toggle { selected: true })
                | Some(UiControlPresentation::Choice { selected: true, .. })
        );
        let style = resolve_component_style(
            &visual.kind,
            visual.style,
            visual.presentation.as_ref(),
            UiStateFlags {
                hovered: pointer_over,
                pressed: (pointer_over && time_seconds < self.pressed_until_seconds)
                    || self.splitter_drag.as_ref().is_some_and(|d| d.splitter_path == node_path),
                focused: self.focused_control.as_deref() == Some(node_path),
                disabled: !visual.enabled,
                selected,
                checked: selected,
                open: self.open_dropdown.as_deref() == Some(node_path),
            },
        );
        let fill = style.background_color;
        if node_path.contains("split-h") {
            let has_active = self.active.get(node_path).is_some();
            splitter_debug(&format!("[FILL] path={} resolved_fill={:?} has_active_transition={}", node_path, fill, has_active));
        }
        if visual.kind == UiNodeKind::Button
            && pointer_over
            && time_seconds < self.pressed_until_seconds
        {
            bounds.y += 1.0;
            bounds.height = (bounds.height - 1.0).max(0.0);
        }
        // A child without its own transition follows its parent's GPU motion
        // through `from_rect`. Its final clip is otherwise too narrow to
        // contain the child at the parent's starting position, so retain the
        // union of the target clip and the translated starting clip.
        let parent_transition_offset = self.parent_transition_offset(node_path);
        let paint_clip = parent_transition_offset.map_or(visual.clip, |offset| {
            union_bounds(visual.clip, translate_bounds(visual.clip, offset))
        });
        let mut instance = UiInstance {
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
                paint_clip.x,
                paint_clip.y,
                paint_clip.x + paint_clip.width,
                paint_clip.y + paint_clip.height,
            ],
            depth: color_pass_depth(visual.world_depth),
            paint_group_id: visual.paint_group_id,
            from_rect: [bounds.x, bounds.y, bounds.width, bounds.height],
            from_fill: fill,
            from_border: style.border_color,
            from_params: [
                style.border_width,
                style.corner_radius,
                style.opacity,
                visual.clip_radius,
            ],
            animation: [0.0; 4],
            cut: [0.0; 4],
        };
        if let Some(cut) = self.node_cuts.get(
            node_path
                .rsplit('/')
                .next()
                .unwrap_or(node_path),
        ) {
            instance.cut = resolve_shell_cut([bounds.width, bounds.height], *cut);
        }
        if let Some(active) = self.active.get(node_path) {
            let from_style = if active.from.style == UiStyle::default() {
                default_component_style(&active.from.kind)
            } else {
                active.from.style
            };
            instance.from_rect = [
                active.from.bounds.x,
                active.from.bounds.y,
                active.from.bounds.width,
                active.from.bounds.height,
            ];
            instance.from_fill = from_style.background_color;
            instance.from_border = from_style.border_color;
            instance.from_params = [
                from_style.border_width,
                from_style.corner_radius,
                from_style.opacity,
                active.from.clip_radius,
            ];
            instance.animation = [
                active.started_at_seconds + active.transition.delay_ms as f32 / 1000.0,
                active.transition.duration_ms as f32 / 1000.0,
                gpu_easing(active.transition.easing),
                1.0,
            ];
        } else if let Some(offset) = parent_transition_offset {
            // A parent panel's GPU track also moves its descendants. Encode
            // the inherited start offset into the child's one-time record so
            // the vertex shader can apply the same track without CPU sampling
            // or a second per-frame upload.
            instance.from_rect[0] += offset[0];
            instance.from_rect[1] += offset[1];
            if let Some((_, active)) = self.find_parent_transition(node_path) {
                instance.animation = [
                    active.started_at_seconds + active.transition.delay_ms as f32 / 1000.0,
                    active.transition.duration_ms as f32 / 1000.0,
                    gpu_easing(active.transition.easing),
                    1.0,
                ];
            }
        }
        instance.cut = resolve_shell_cut([bounds.width, bounds.height], instance.cut);
        instance
    }

    /// Largest material-bearing panel after final layout. Windows Composition
    /// uses this as the bounded source region for its system backdrop; it is
    /// renderer-derived rather than authored as an HWND rectangle.
    pub(crate) fn primary_material_shell(&self) -> Option<(UiBounds, [f32; 4])> {
        self.plan
            .iter()
            .filter_map(|node| {
                // Nodes in the behind_glass composition layer are render
                // layers, not shell containers. They must never be selected
                // as the primary shell, otherwise their (typically absent)
                // cut geometry replaces the real shell's cut corners.
                let layer = self
                    .composition_layers
                    .get(&node.id)
                    .copied()
                    .unwrap_or_default();
                if layer == neon_ui_schema::UiCompositionLayer::BehindGlass {
                    return None;
                }
                let key = node.id.rsplit('/').next()?;
                self.node_materials.get(key).map(|_| {
                    let bounds = node.target.bounds;
                    let cut = self.node_cuts.get(key).copied().unwrap_or([0.0; 4]);
                    (bounds, resolve_shell_cut([bounds.width, bounds.height], cut))
                })
            })
            .max_by(|(left_bounds, left_cut), (right_bounds, right_cut)| {
                let left_area = left_bounds.width * left_bounds.height;
                let right_area = right_bounds.width * right_bounds.height;
                match left_area.partial_cmp(&right_area).unwrap_or(std::cmp::Ordering::Equal) {
                    // Equal area: prefer the node that actually declares cut
                    // corners so rectangular splash/transition overlays cannot
                    // win the shell selection on the first frame.
                    std::cmp::Ordering::Equal => {
                        let left_cut_sum: f32 = left_cut.iter().sum();
                        let right_cut_sum: f32 = right_cut.iter().sum();
                        left_cut_sum.partial_cmp(&right_cut_sum).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    other => other,
                }
            })
    }

    /// Creates the visual-only draw layer declared by `material`. It reuses the
    /// panel vertex ABI so material overflow stays in logical coordinates while
    /// the hit pass continues to emit only the host node.
    fn material_instance(
        &self,
        visual: &UiVisual,
        node_path: &str,
        material: &UiMaterialRef,
        time_seconds: f32,
    ) -> UiInstance {
        let mut instance = self.instance(visual, node_path, time_seconds);
        let bounds = material.draw_bounds(visual.bounds);
        instance.rect = [bounds.x, bounds.y, bounds.width, bounds.height];
        instance.from_rect = instance.rect;
        // Material layers are purposefully translucent. The normal panel
        // shader supplies the composition-safe fallback when a custom package
        // is not resident yet; package-specific pipelines replace this record
        // in the material pass.
        let (fill, border) = match material.package_id.as_str() {
            "pulse-neon-edge" => ([0.56, 1.0, 0.04, 0.045], [0.72, 1.0, 0.10, 0.92]),
            "pulse-equalizer" => ([0.42, 1.0, 0.05, 0.10], [0.65, 1.0, 0.12, 0.80]),
            _ => ([0.92, 1.0, 0.78, 0.10], [0.74, 1.0, 0.22, 0.55]),
        };
        instance.fill = fill;
        instance.border = border;
        instance.from_fill = fill;
        instance.from_border = border;
        instance.params[0] = if material.package_id == "pulse-neon-edge" { 1.25 } else { 0.8 };
        instance.params[1] = 0.0;
        instance.params[2] = 1.0;
        instance.from_params = instance.params;
        instance.clip = [
            bounds.x,
            bounds.y,
            bounds.x + bounds.width,
            bounds.y + bounds.height,
        ];
        instance
    }

    fn find_parent_transition(&self, node_path: &str) -> Option<(&str, &ActiveTransition)> {
        let mut parent = self
            .plan
            .iter()
            .find(|node| node.id == node_path)
            .and_then(|node| node.parent_id.as_deref());
        while let Some(parent_id) = parent {
            if let Some(active) = self.active.get(parent_id) {
                return Some((parent_id, active));
            }
            parent = self
                .plan
                .iter()
                .find(|node| node.id == parent_id)
                .and_then(|node| node.parent_id.as_deref());
        }
        None
    }

    fn parent_transition_offset(&self, node_path: &str) -> Option<[f32; 2]> {
        self.find_parent_transition(node_path).map(|(_, active)| {
            [
                active.from.bounds.x - active.target.bounds.x,
                active.from.bounds.y - active.target.bounds.y,
            ]
        })
    }

    fn component_chrome_instances(&self, visual: &UiVisual, node_path: &str) -> Vec<UiInstance> {
        let mut preview = visual.clone();
        if let Some(value) = self.value_previews.get(node_path) {
            preview.presentation = match (&preview.presentation, value) {
                (
                    Some(UiControlPresentation::Toggle { .. }),
                    UiSemanticPayloadValue::Bool { value },
                ) => Some(UiControlPresentation::Toggle { selected: *value }),
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
        let pointer_over = self
            .pointer_position
            .is_some_and(|position| contains(preview.bounds, position));
        let selected = matches!(
            &preview.presentation,
            Some(UiControlPresentation::Toggle { selected: true })
                | Some(UiControlPresentation::Choice { selected: true, .. })
        );
        preview.style = resolve_component_style(
            &preview.kind,
            preview.style,
            preview.presentation.as_ref(),
            UiStateFlags {
                hovered: pointer_over,
                pressed: pointer_over && self.pressed_until_seconds > 0.0,
                focused: self.focused_control.as_deref() == Some(node_path),
                disabled: !preview.enabled,
                selected,
                checked: selected,
                open: self.open_dropdown.as_deref() == Some(node_path),
            },
        );
        let mut instances = if matches!(preview.kind, UiNodeKind::Button | UiNodeKind::Slider)
            && self.skin_references.contains_key(node_path)
        {
            Vec::new()
        } else {
            component_chrome_instances(&preview)
        };
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
                depth: color_pass_depth(preview.world_depth),
                paint_group_id: visual.paint_group_id,
                ..UiInstance::zeroed()
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
            paint_group_id: 0,
            ..UiInstance::zeroed()
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
                paint_group_id: 0,
                ..UiInstance::zeroed()
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiComponentMetrics {
    min_width: f32,
    min_height: f32,
    horizontal_padding: f32,
    text_inset: f32,
    control_glyph_width: f32,
    control_glyph_gap: f32,
    track_height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UiComponentCapabilities {
    has_text: bool,
    interactive: bool,
    numeric: bool,
    choice: bool,
    toggle: bool,
    popup: bool,
    scroll: bool,
    top_layer: bool,
    virtualized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiComponentSpec {
    metrics: UiComponentMetrics,
    capabilities: UiComponentCapabilities,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UiStateFlags {
    hovered: bool,
    pressed: bool,
    focused: bool,
    disabled: bool,
    selected: bool,
    checked: bool,
    open: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UiStylePatch {
    background_color: Option<[f32; 4]>,
    border_color: Option<[f32; 4]>,
    border_width: Option<f32>,
    corner_radius: Option<f32>,
    opacity: Option<f32>,
}

impl From<SchemaStylePatch> for UiStylePatch {
    fn from(value: SchemaStylePatch) -> Self {
        Self {
            background_color: value.background_color,
            border_color: value.border_color,
            border_width: value.border_width,
            corner_radius: value.corner_radius,
            opacity: value.opacity,
        }
    }
}

fn apply_style_patch(mut style: UiStyle, patch: UiStylePatch) -> UiStyle {
    if let Some(value) = patch.background_color {
        style.background_color = value;
    }
    if let Some(value) = patch.border_color {
        style.border_color = value;
    }
    if let Some(value) = patch.border_width {
        style.border_width = value;
    }
    if let Some(value) = patch.corner_radius {
        style.corner_radius = value;
    }
    if let Some(value) = patch.opacity {
        style.opacity = value;
    }
    style
}

fn resolve_component_style(
    kind: &UiNodeKind,
    authored: UiStyle,
    presentation: Option<&UiControlPresentation>,
    flags: UiStateFlags,
) -> UiStyle {
    let mut style = if authored == UiStyle::default() {
        default_component_style(kind)
    } else {
        authored
    };
    // Splitter: opaque black matching window clear color = thin seam between panels.
    // 8px hit area, visually appears as a dark divider.
    if matches!(kind, UiNodeKind::Splitter) {
        style.background_color = [0.0, 0.0, 0.0, 1.0];
        style.border_color = [0.0, 0.0, 0.0, 0.0];
        style.border_width = 0.0;
    }
    let selected = flags.selected || flags.checked;
    if selected {
        style = apply_style_patch(
            style,
            UiStylePatch {
                border_color: Some([0.34, 0.80, 0.64, 0.95]),
                ..UiStylePatch::default()
            },
        );
    }
    if flags.focused && style.border_width > 0.0 {
        style = apply_style_patch(
            style,
            UiStylePatch {
                border_color: Some([0.72, 0.92, 1.0, 1.0]),
                border_width: Some(style.border_width.max(1.0)),
                ..UiStylePatch::default()
            },
        );
    }
    if flags.hovered {
        style.background_color = [
            (style.background_color[0] * 1.14).min(1.0),
            (style.background_color[1] * 1.14).min(1.0),
            (style.background_color[2] * 1.14).min(1.0),
            style.background_color[3],
        ];
    }
    if flags.pressed {
        style.background_color = [
            style.background_color[0] * 0.88,
            style.background_color[1] * 0.88,
            style.background_color[2] * 0.88,
            style.background_color[3],
        ];
    }
    if flags.disabled {
        style = apply_style_patch(
            style,
            UiStylePatch {
                background_color: Some([
                    style.background_color[0] * 0.62,
                    style.background_color[1] * 0.62,
                    style.background_color[2] * 0.62,
                    style.background_color[3],
                ]),
                border_color: Some([
                    style.border_color[0] * 0.62,
                    style.border_color[1] * 0.62,
                    style.border_color[2] * 0.62,
                    style.border_color[3],
                ]),
                opacity: Some(style.opacity * 0.58),
                ..UiStylePatch::default()
            },
        );
    }
    let _ = (kind, presentation, flags.open);
    style
}

fn component_spec(kind: &UiNodeKind) -> UiComponentSpec {
    let text = matches!(
        kind,
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
            | UiNodeKind::Tooltip
            | UiNodeKind::Modal
            | UiNodeKind::Dialog
            | UiNodeKind::Selectable
            | UiNodeKind::ListBox
            | UiNodeKind::Scrollbar
            | UiNodeKind::ProgressBar
            | UiNodeKind::Switch
            | UiNodeKind::Toast
            | UiNodeKind::MenuBar
    );
    let interactive = matches!(
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
            | UiNodeKind::Switch
            | UiNodeKind::MenuBar
            | UiNodeKind::Accordion
    );
    let metrics = UiComponentMetrics {
        min_width: 0.0,
        min_height: match kind {
            UiNodeKind::Button
            | UiNodeKind::Checkbox
            | UiNodeKind::RadioButton
            | UiNodeKind::Slider
            | UiNodeKind::DragValue
            | UiNodeKind::Selectable
            | UiNodeKind::Switch => 30.0,
            UiNodeKind::TextInput | UiNodeKind::Combo | UiNodeKind::Dropdown | UiNodeKind::Tabs | UiNodeKind::MenuBar => {
                32.0
            }
            UiNodeKind::ListBox => 90.0,
            UiNodeKind::Scrollbar => 20.0,
            UiNodeKind::ProgressBar => 24.0,
            UiNodeKind::Spinner => 24.0,
            UiNodeKind::Divider => 1.0,
            _ => 0.0,
        },
        horizontal_padding: match kind {
            UiNodeKind::Button => 10.0,
            UiNodeKind::TextInput => 6.0,
            _ => 8.0,
        },
        text_inset: if *kind == UiNodeKind::TextInput {
            TEXT_INPUT_INSET * 2.0
        } else {
            8.0
        },
        control_glyph_width: matches!(
            kind,
            UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
        )
        .then_some(18.0)
        .unwrap_or(0.0),
        control_glyph_gap: 8.0,
        track_height: match kind {
            UiNodeKind::Slider | UiNodeKind::Scrollbar => 4.0,
            UiNodeKind::ProgressBar => 24.0,
            _ => 0.0,
        },
    };
    UiComponentSpec {
        metrics,
        capabilities: UiComponentCapabilities {
            has_text: text,
            interactive,
            numeric: matches!(
                kind,
                UiNodeKind::Slider
                    | UiNodeKind::DragValue
                    | UiNodeKind::ProgressBar
                    | UiNodeKind::Scrollbar
            ),
            choice: matches!(
                kind,
                UiNodeKind::Combo | UiNodeKind::Dropdown | UiNodeKind::Tabs | UiNodeKind::ListBox
            ),
            toggle: matches!(
                kind,
                UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable | UiNodeKind::Switch
            ),
            popup: matches!(kind, UiNodeKind::Combo | UiNodeKind::Dropdown),
            scroll: matches!(
                kind,
                UiNodeKind::ListBox | UiNodeKind::Scrollbar | UiNodeKind::DataGrid
            ),
            top_layer: matches!(
                kind,
                UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog | UiNodeKind::ContextMenu | UiNodeKind::Toast
            ),
            virtualized: *kind == UiNodeKind::DataGrid,
        },
    }
}

fn collect_image_fits(node: &UiNode, output: &mut HashMap<String, UiImageFit>) {
    if node.kind == UiNodeKind::Image {
        if let Some(layout) = node.layout {
            output.insert(node.node_id.0.clone(), layout.image_fit);
        }
    }
    for child in &node.children {
        collect_image_fits(child, output);
    }
}

fn select_button_skin_slot<'a>(skin: &'a UiControlSkin, hovered: bool, pressed: bool, enabled: bool) -> Option<&'a UiSkinSlot> {
    let states = if !enabled {
        [UiVisualState::Disabled, UiVisualState::Normal, UiVisualState::Normal]
    } else if pressed {
        [UiVisualState::Pressed, UiVisualState::Hover, UiVisualState::Normal]
    } else if hovered {
        [UiVisualState::Hover, UiVisualState::Normal, UiVisualState::Normal]
    } else {
        [UiVisualState::Normal, UiVisualState::Normal, UiVisualState::Normal]
    };
    states.iter().find_map(|state| {
        skin.slots.iter().find(|slot| slot.slot_kind == UiSkinSlotKind::Body && slot.state == *state)
    })
}

fn select_slider_skin_slot<'a>(skin: &'a UiControlSkin, kind: UiSkinSlotKind, hovered: bool, pressed: bool, enabled: bool) -> Option<&'a UiSkinSlot> {
    let states = if !enabled {
        [UiVisualState::Disabled, UiVisualState::Normal, UiVisualState::Normal]
    } else if kind == UiSkinSlotKind::Fill {
        [UiVisualState::Active, UiVisualState::Normal, UiVisualState::Normal]
    } else if pressed {
        [UiVisualState::Pressed, UiVisualState::Hover, UiVisualState::Normal]
    } else if hovered {
        [UiVisualState::Hover, UiVisualState::Normal, UiVisualState::Normal]
    } else {
        [UiVisualState::Normal, UiVisualState::Normal, UiVisualState::Normal]
    };
    states.iter().find_map(|state| skin.slots.iter().find(|slot| slot.slot_kind == kind && slot.state == *state))
}

/// Select a skin slot for toggle controls (Checkbox / RadioButton).
/// Supports Disabled → Pressed → Hover → Normal fallback chain for any slot kind.
fn select_toggle_skin_slot<'a>(
    skin: &'a UiControlSkin,
    kind: UiSkinSlotKind,
    hovered: bool,
    pressed: bool,
    enabled: bool,
) -> Option<&'a UiSkinSlot> {
    let states = if !enabled {
        [UiVisualState::Disabled, UiVisualState::Normal, UiVisualState::Normal]
    } else if pressed {
        [UiVisualState::Pressed, UiVisualState::Hover, UiVisualState::Normal]
    } else if hovered {
        [UiVisualState::Hover, UiVisualState::Normal, UiVisualState::Normal]
    } else {
        [UiVisualState::Normal, UiVisualState::Normal, UiVisualState::Normal]
    };
    states.iter().find_map(|state| skin.slots.iter().find(|slot| slot.slot_kind == kind && slot.state == *state))
}

/// Select a skin slot for Scrollbar track / thumb.
/// Supports Pressed → Hover → Normal fallback chain.
fn select_scrollbar_skin_slot<'a>(
    skin: &'a UiControlSkin,
    kind: UiSkinSlotKind,
    hovered: bool,
    pressed: bool,
) -> Option<&'a UiSkinSlot> {
    let states = if pressed {
        [UiVisualState::Pressed, UiVisualState::Hover, UiVisualState::Normal]
    } else if hovered {
        [UiVisualState::Hover, UiVisualState::Normal, UiVisualState::Normal]
    } else {
        [UiVisualState::Normal, UiVisualState::Normal, UiVisualState::Normal]
    };
    states.iter().find_map(|state| skin.slots.iter().find(|slot| slot.slot_kind == kind && slot.state == *state))
}

fn fit_image_rect_and_uv(
    bounds: UiBounds,
    mut uv: [f32; 4],
    image_width: u32,
    image_height: u32,
    fit: UiImageFit,
) -> ([f32; 4], [f32; 4]) {
    let mut rect = [bounds.x, bounds.y, bounds.width, bounds.height];
    if image_width == 0 || image_height == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 || fit == UiImageFit::Stretch {
        return (rect, uv);
    }
    let source_aspect = image_width as f32 / image_height as f32;
    let target_aspect = bounds.width / bounds.height;
    match fit {
        UiImageFit::Cover if source_aspect > target_aspect => {
            let visible = target_aspect / source_aspect;
            let inset = (1.0 - visible) * 0.5;
            let width = uv[2] - uv[0];
            uv[0] += width * inset;
            uv[2] -= width * inset;
        }
        UiImageFit::Cover => {
            let visible = source_aspect / target_aspect;
            let inset = (1.0 - visible) * 0.5;
            let height = uv[3] - uv[1];
            uv[1] += height * inset;
            uv[3] -= height * inset;
        }
        UiImageFit::Contain if source_aspect > target_aspect => {
            let height = bounds.width / source_aspect;
            rect[1] += (bounds.height - height) * 0.5;
            rect[3] = height;
        }
        UiImageFit::Contain => {
            let width = bounds.height * source_aspect;
            rect[0] += (bounds.width - width) * 0.5;
            rect[2] = width;
        }
        UiImageFit::Stretch => {}
    }
    (rect, uv)
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
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
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
        UiNodeKind::Splitter => UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
        UiNodeKind::ContextMenu => UiStyle {
            background_color: [0.16, 0.19, 0.22, 0.98],
            border_color: [0.40, 0.48, 0.55, 0.90],
            border_width: 1.0,
            corner_radius: 6.0,
            opacity: 1.0,
        },
        UiNodeKind::TreeView => UiStyle {
            background_color: [0.10, 0.11, 0.13, 1.0],
            border_color: [0.28, 0.38, 0.34, 0.70],
            border_width: 1.0,
            corner_radius: 4.0,
            opacity: 1.0,
        },
        UiNodeKind::Switch => UiStyle {
            background_color: [0.09, 0.12, 0.12, 1.0],
            border_color: [0.27, 0.46, 0.40, 0.82],
            border_width: 1.0,
            corner_radius: 10.0,
            opacity: 1.0,
        },
        UiNodeKind::Toast => UiStyle {
            background_color: [0.14, 0.17, 0.20, 0.96],
            border_color: [0.38, 0.46, 0.54, 0.85],
            border_width: 1.0,
            corner_radius: 6.0,
            opacity: 1.0,
        },
        UiNodeKind::MenuBar => UiStyle {
            background_color: [0.12, 0.14, 0.16, 1.0],
            border_color: [0.30, 0.38, 0.44, 0.80],
            border_width: 1.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
        UiNodeKind::Accordion => UiStyle {
            background_color: [0.10, 0.12, 0.14, 1.0],
            border_color: [0.28, 0.36, 0.42, 0.70],
            border_width: 1.0,
            corner_radius: 4.0,
            opacity: 1.0,
        },
        UiNodeKind::Spinner => UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
        UiNodeKind::Divider => UiStyle {
            background_color: [0.22, 0.28, 0.32, 1.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
        // Containers, labels, images, and render surfaces do not get implicit
        // component chrome. Their authored default is a sentinel used by the
        // component resolver, so make the renderer fallback transparent here.
        _ => UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        },
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
        depth: color_pass_depth(visual.world_depth),
        paint_group_id: visual.paint_group_id,
        ..UiInstance::zeroed()
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
        UiNodeKind::Button => vec![],
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
                x: bounds.x + 12.0,
                y: center_y - 2.0,
                width: (bounds.width - 24.0).max(1.0),
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
                    depth: color_pass_depth(visual.world_depth),
                    paint_group_id: visual.paint_group_id,
                    ..UiInstance::zeroed()
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
        UiNodeKind::TreeView => {
            // TreeView base chrome: row separators every 24px
            let row_height = 24.0;
            let num_rows = ((bounds.height - 8.0) / row_height).floor() as i32;
            (0..num_rows)
                .map(|i| {
                    let y = bounds.y + 8.0 + i as f32 * row_height;
                    chrome(
                        UiBounds { x: bounds.x + 4.0, y, width: bounds.width - 8.0, height: 1.0 },
                        [0.12, 0.16, 0.15, 0.5],
                        [0.0, 0.0, 0.0, 0.0],
                        0.0,
                    )
                })
                .collect()
        }
        UiNodeKind::ContextMenu => {
            // ContextMenu chrome: subtle inner border highlight
            vec![chrome(
                UiBounds { x: bounds.x + 1.0, y: bounds.y + 1.0, width: bounds.width - 2.0, height: bounds.height - 2.0 },
                [0.0, 0.0, 0.0, 0.0],
                [0.25, 0.32, 0.38, 0.4],
                4.0,
            )]
        }
        UiNodeKind::Splitter => {
            // Splitter chrome: center grip line
            // horizontal = drag along X axis (vertical bar), same semantics as begin_splitter_drag
            let horizontal = bounds.width < bounds.height;
            if horizontal {
                vec![chrome(
                    UiBounds { x: bounds.x + bounds.width * 0.5 - 1.0, y: bounds.y + 4.0, width: 2.0, height: bounds.height - 8.0 },
                    [0.30, 0.38, 0.35, 0.6],
                    [0.0, 0.0, 0.0, 0.0],
                    1.0,
                )]
            } else {
                vec![chrome(
                    UiBounds { x: bounds.x + 4.0, y: bounds.y + bounds.height * 0.5 - 1.0, width: bounds.width - 8.0, height: 2.0 },
                    [0.30, 0.38, 0.35, 0.6],
                    [0.0, 0.0, 0.0, 0.0],
                    1.0,
                )]
            }
        }
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
        UiNodeKind::Switch => {
            let track = UiBounds {
                x: bounds.x + 4.0,
                y: center_y - 7.0,
                width: (bounds.width - 8.0).max(28.0),
                height: 14.0,
            };
            let thumb_size = 10.0;
            let thumb_x = if selected {
                track.x + track.width - thumb_size - 2.0
            } else {
                track.x + 2.0
            };
            vec![
                chrome(track, if selected { [0.16, 0.35, 0.28, 1.0] } else { muted }, if selected { mint } else { [0.40, 0.44, 0.50, 0.8] }, 7.0),
                chrome(
                    UiBounds { x: thumb_x, y: center_y - thumb_size * 0.5, width: thumb_size, height: thumb_size },
                    [0.90, 0.92, 0.95, 1.0],
                    [0.90, 0.92, 0.95, 1.0],
                    5.0,
                ),
            ]
        }
        UiNodeKind::Spinner => {
            // Static arc ring; animation is handled by the render loop via
            // time-based rotation when a skin is not applied.
            let size = bounds.width.min(bounds.height).min(24.0).max(8.0);
            let cx = bounds.x + bounds.width * 0.5;
            let cy = bounds.y + bounds.height * 0.5;
            let half = size * 0.5;
            vec![
                // Background ring (full circle, muted)
                chrome(
                    UiBounds { x: cx - half, y: cy - half, width: size, height: size },
                    [0.0, 0.0, 0.0, 0.0],
                    [0.25, 0.30, 0.35, 0.6],
                    half,
                ),
                // Foreground arc (top-right quarter, accent)
                chrome(
                    UiBounds { x: cx - half + 1.0, y: cy - half + 1.0, width: size - 2.0, height: (size - 2.0) * 0.5 },
                    [0.0, 0.0, 0.0, 0.0],
                    mint,
                    half - 1.0,
                ),
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
            UiDrawMode::All,
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
pub(crate) fn render_hit_ids_with_renderer_for_test(
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

fn create_canvas_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("neon3-ui-canvas-instances"),
        size: (capacity.max(1) * std::mem::size_of::<UiCanvasInstance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn text_ref_value(text: &TextRef) -> Option<&str> {
    match text {
        TextRef::Key { key, .. } => (!key.trim().is_empty()).then_some(key.as_str()),
        TextRef::Literal { value } => (!value.is_empty()).then_some(value.as_str()),
        TextRef::Rich { .. } => None,
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
    // Glyph raster bounds can extend slightly past the advance width. Keep a
    // small logical safety allowance inside the inherited parent clip so the
    // final glyph is not shaved by the node's nominal right edge.
    let right = (visual.bounds.x + visual.bounds.width + 2.0).min(clip.x + clip.width);
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

/// Shared text line-breaking result. Both intrinsic measurement and glyph
/// layout use the same underlying `break_text_lines` so they can never
/// disagree about how many lines text occupies or how wide it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TextMeasure {
    line_count: u32,
    max_line_width: f32,
    total_height: f32,
    line_height: f32,
}

/// Whether `ch` belongs to a CJK script where line-breaking is allowed
/// between any two characters (no whitespace word boundaries).
fn is_cjk_breakable(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F | // CJK Symbols and Punctuation
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0x3400..=0x4DBF | // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0xAC00..=0xD7AF | // Hangul Syllables
        0xF900..=0xFAFF | // CJK Compatibility Ideographs
        0xFF00..=0xFFEF   // Halfwidth and Fullwidth Forms (full-width latin/digits/punct)
    )
}

/// A single atomic unit for line-breaking.
#[derive(Debug)]
enum TextToken {
    /// A contiguous run of non-space, non-CJK characters (a Latin word,
    /// number, or symbol run). Must not be split mid-word unless it
    /// overflows the entire line width.
    Word(Vec<char>),
    /// A single CJK character. May break before or after.
    Cjk(char),
    /// A space character. Collapsible at line start/end.
    Space,
}

/// Tokenize `text` into line-breaking units. Explicit `\n` is returned as
/// `Word(vec!['\n'])` so the caller can detect it without a separate enum
/// variant; it is always flushed before being pushed.
fn tokenize_text(text: &str) -> Vec<TextToken> {
    let mut tokens = Vec::new();
    let mut word: Vec<char> = Vec::new();
    for ch in text.chars() {
        if ch == '\n' {
            if !word.is_empty() {
                tokens.push(TextToken::Word(std::mem::take(&mut word)));
            }
            tokens.push(TextToken::Word(vec!['\n']));
            continue;
        }
        if ch == ' ' {
            if !word.is_empty() {
                tokens.push(TextToken::Word(std::mem::take(&mut word)));
            }
            tokens.push(TextToken::Space);
        } else if is_cjk_breakable(ch) {
            if !word.is_empty() {
                tokens.push(TextToken::Word(std::mem::take(&mut word)));
            }
            tokens.push(TextToken::Cjk(ch));
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        tokens.push(TextToken::Word(word));
    }
    tokens
}

/// Break `text` into character groups per line using the same wrapping
/// rules that `measure_text_lines` and `layout_text` rely on.  This is the
/// single source of truth for line-breaking decisions.
///
/// Rules:
/// - Latin words (contiguous non-space non-CJK) break only at word
///   boundaries; a word longer than the line is split character-by-character.
/// - CJK characters may break between any two characters.
/// - Spaces are collapsed at line start and omitted at line end.
/// - Explicit `\n` always forces a line break.
fn break_text_lines(
    text: &str,
    available_width: f32,
    advance: &impl Fn(usize, char) -> f32,
) -> Vec<Vec<char>> {
    /// Push the current line after stripping trailing spaces. Trailing spaces
    /// are never visually meaningful and must not count toward line width.
    fn push_line(lines: &mut Vec<Vec<char>>, line: &mut Vec<char>) {
        while line.last() == Some(&' ') {
            line.pop();
        }
        lines.push(std::mem::take(line));
    }

    let width = available_width.max(1.0);
    let mut lines: Vec<Vec<char>> = Vec::new();
    let mut line: Vec<char> = Vec::new();
    let mut line_width = 0.0;
    let mut char_index = 0usize;

    for token in tokenize_text(text) {
        match token {
            TextToken::Word(chars) => {
                // Explicit newline marker.
                if chars.len() == 1 && chars[0] == '\n' {
                    push_line(&mut lines, &mut line);
                    line_width = 0.0;
                    char_index += 1;
                    continue;
                }
                let word_width: f32 = chars
                    .iter()
                    .enumerate()
                    .map(|(i, &ch)| advance(char_index + i, ch))
                    .sum();
                // If the word does not fit on the current line and the line
                // is non-empty, break before the word.
                if !line.is_empty() && line_width + word_width > width {
                    push_line(&mut lines, &mut line);
                    line_width = 0.0;
                }
                if word_width <= width {
                    // Word fits on a (possibly fresh) line.
                    let n = chars.len();
                    for (i, ch) in chars.into_iter().enumerate() {
                        line_width += advance(char_index + i, ch);
                        line.push(ch);
                    }
                    char_index += n;
                } else {
                    // Word itself overflows the line: split character by
                    // character (same behaviour as the old per-character
                    // breaker for unavoidable overflow).
                    for ch in chars {
                        let gw = advance(char_index, ch);
                        if !line.is_empty() && line_width + gw > width {
                            push_line(&mut lines, &mut line);
                            line_width = 0.0;
                        }
                        line_width += gw;
                        line.push(ch);
                        char_index += 1;
                    }
                }
            }
            TextToken::Cjk(ch) => {
                let gw = advance(char_index, ch);
                if !line.is_empty() && line_width + gw > width {
                    push_line(&mut lines, &mut line);
                    line_width = 0.0;
                }
                line_width += gw;
                line.push(ch);
                char_index += 1;
            }
            TextToken::Space => {
                let gw = advance(char_index, ' ');
                // Collapse leading spaces; trailing spaces are stripped by
                // push_line. Never break *because* of a space.
                if line.is_empty() {
                    char_index += 1;
                    continue;
                }
                line_width += gw;
                line.push(' ');
                char_index += 1;
            }
        }
    }

    if !line.is_empty() || lines.is_empty() {
        push_line(&mut lines, &mut line);
    }
    lines
}

/// Measure text lines using the same breaking rules as `break_text_lines`.
/// This is the function that `intrinsic_size` calls to get the aggregate
/// measure; `layout_text` calls `break_text_lines` directly and then
/// rasterizes each line.
fn measure_text_lines(
    text: &str,
    available_width: f32,
    line_height: f32,
    advance: &impl Fn(usize, char) -> f32,
) -> TextMeasure {
    let lines = break_text_lines(text, available_width, advance);
    let line_count = lines.len() as u32;
    let max_line_width = lines
        .iter()
        .map(|line| {
            line.iter()
                .enumerate()
                .map(|(i, &ch)| advance(i, ch))
                .sum::<f32>()
        })
        .fold(0.0f32, f32::max);
    TextMeasure {
        line_count,
        max_line_width,
        total_height: line_height * line_count as f32,
        line_height,
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
    if clip[2] <= 0.0 || clip[3] <= 0.0
        || visual.bounds.width <= 0.0 || visual.bounds.height <= 0.0
        || visual.logical_bounds.width <= 0.0
    {
        return None;
    }
    // Glyph scale comes only from the owning WorldUi root transform. It is not
    // derived from text bounds or content height, so camera distance cannot
    // create an independent text-layout feedback loop.
    let text_scale = visual.world_scale.unwrap_or(1.0);
    // Text measurement always runs in logical (pre-projection) space. The
    // logical safe inset is subtracted once; a WorldUi scale must not turn
    // the 8px inset into a different effective inset, and camera distance
    // cannot change how many glyphs fit on a line.
    let wrap_width =
        (visual.logical_bounds.width.max(1.0) - text_safe_inset(&visual.kind)).max(1.0);
    let advance = |_idx: usize, ch: char| {
        // Use atlas glyph advance (cached) so line-breaking is identical to
        // what intrinsic_size would compute.
        font.font.metrics(ch, FONT_RASTER_SIZE).advance_width
    };
    let char_lines = break_text_lines(text, wrap_width, &advance);
    let block_height = font.line_height * text_scale * char_lines.len() as f32;
    let top = visual.bounds.y + ((visual.bounds.height - block_height).max(0.0) * 0.5);
    let mut result = Vec::new();
    for (line_index, glyph_chars) in char_lines.into_iter().enumerate() {
        let mut glyphs = Vec::new();
        let mut line_advance = 0.0;
        for ch in glyph_chars {
            let glyph = ensure_glyph(device, queue, font, ch).ok()?;
            line_advance += glyph.advance * text_scale;
            glyphs.push(glyph);
        }
        let mut x = if visual.kind == UiNodeKind::Button {
            visual.bounds.x + ((visual.bounds.width - line_advance).max(0.0) * 0.5)
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
        let baseline =
            top + font.ascent * text_scale + line_index as f32 * font.line_height * text_scale;
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
                depth: color_pass_depth(visual.world_depth),
                paint_group_id: visual.paint_group_id,
            });
            x += glyph.advance * text_scale;
        }
    }
    Some(result)
}

fn layout_rich_text(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    font: &mut ResidentFont,
    visual: &UiVisual,
    spans: &[neon_ui_schema::UiRichTextSpan],
) -> Option<Vec<UiTextInstance>> {
    let clip = text_clip(visual)?;
    if clip[2] <= 0.0 || clip[3] <= 0.0
        || visual.bounds.width <= 0.0 || visual.bounds.height <= 0.0
        || visual.logical_bounds.width <= 0.0
    {
        return None;
    }
    let world_scale = visual.world_scale.unwrap_or(1.0);

    // Flatten spans into per-character style so line-breaking can use the
    // real scaled advance of each character (different spans may have
    // different scales).
    #[derive(Clone, Copy)]
    struct StyledChar {
        ch: char,
        scale: f32,
        color: [f32; 4],
    }
    let mut styled: Vec<StyledChar> = Vec::new();
    for span in spans {
        let scale = world_scale * span.scale;
        let color = [
            span.color[0],
            span.color[1],
            span.color[2],
            span.color[3] * visual.style.opacity,
        ];
        for ch in span.value.chars() {
            styled.push(StyledChar { ch, scale, color });
        }
    }
    if styled.is_empty() {
        return Some(Vec::new());
    }

    let wrap_width =
        (visual.logical_bounds.width.max(1.0) - text_safe_inset(&visual.kind)).max(1.0);
    let flat_text: String = styled.iter().map(|s| s.ch).collect();

    // Advance for line-breaking: use the per-character scaled advance.
    let advance = |idx: usize, _ch: char| -> f32 {
        styled
            .get(idx)
            .map(|s| font.font.metrics(s.ch, FONT_RASTER_SIZE).advance_width * s.scale)
            .unwrap_or(0.0)
    };

    let char_lines = break_text_lines(&flat_text, wrap_width, &advance);

    // Line height uses the maximum scale across the whole rich text block.
    let max_scale = styled.iter().map(|s| s.scale).fold(1.0_f32, f32::max);
    let line_height = font.line_height * max_scale;
    let block_height = line_height * char_lines.len() as f32;
    let top = visual.bounds.y + ((visual.bounds.height - block_height).max(0.0) * 0.5);

    let mut result = Vec::new();
    let mut global_idx = 0usize;
    for (line_index, line_chars) in char_lines.into_iter().enumerate() {
        let baseline = top + font.ascent * max_scale + line_index as f32 * line_height;
        // Compute line advance first for Button centering.
        let line_advance: f32 = line_chars
            .iter()
            .enumerate()
            .map(|(i, _ch)| {
                let s = styled[global_idx + i];
                font.font.metrics(s.ch, FONT_RASTER_SIZE).advance_width * s.scale
            })
            .sum();
        let mut x = if visual.kind == UiNodeKind::Button {
            visual.bounds.x + ((visual.bounds.width - line_advance).max(0.0) * 0.5)
        } else {
            visual.bounds.x
                + if visual.kind == UiNodeKind::TextInput {
                    TEXT_INPUT_INSET * world_scale
                } else if matches!(
                    visual.kind,
                    UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable
                ) {
                    30.0 * world_scale
                } else {
                    10.0 * world_scale
                }
        };
        for _ch in line_chars {
            let s = styled[global_idx];
            let glyph = ensure_glyph(device, queue, font, s.ch).ok()?;
            result.push(UiTextInstance {
                rect: [
                    x + glyph.xmin * s.scale,
                    baseline + glyph.plane_min_y * s.scale,
                    glyph.width * s.scale,
                    glyph.height * s.scale,
                ],
                color: s.color,
                clip,
                uv: glyph.uv,
                depth: color_pass_depth(visual.world_depth),
                paint_group_id: visual.paint_group_id,
            });
            x += glyph.advance * s.scale;
            global_idx += 1;
        }
    }
    Some(result)
}

/// Logical horizontal safe inset applied to text measurement and drawing.
/// The same value must be used for intrinsic measurement and actual layout.
fn text_safe_inset(kind: &UiNodeKind) -> f32 {
    component_spec(kind).metrics.text_inset
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
        paint_group_id: 0,
        ..UiInstance::zeroed()
    }
}

/// Check if node at `idx` is a descendant of (or equal to) node at `ancestor_idx`.
fn is_descendant(plan: &[PlannedNode], idx: usize, ancestor_idx: usize) -> bool {
    if idx == ancestor_idx {
        return true;
    }
    let mut current = idx;
    while let Some(pid) = plan[current].parent_id.as_deref() {
        if let Some(pidx) = plan.iter().position(|n| n.id == pid) {
            if pidx == ancestor_idx {
                return true;
            }
            current = pidx;
        } else {
            break;
        }
    }
    false
}

fn top_layer_roots(plan: &[PlannedNode], indices: &HashMap<&str, usize>) -> Vec<Option<usize>> {
    let mut roots = vec![None; plan.len()];
    for (index, node) in plan.iter().enumerate() {
        roots[index] = if node.target.world_depth.is_none()
            && matches!(
                node.target.kind,
                UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog | UiNodeKind::ContextMenu
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
        // A fragment root is projected into the current viewport, but an
        // explicitly authored root box still defines its own clip boundary.
        // Preserve that boundary as the initial inherited clip; roots that
        // use auto dimensions continue to clip to the viewport only.
        let authored_root_clip = (fragment.root.layout.is_some()
            && fragment.root.bounds.width > 0.0
            && fragment.root.bounds.height > 0.0)
            .then_some(fragment.root.bounds);
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
        let external_image_bindings = fragment
            .effects
            .iter()
            .filter_map(|effect| match effect {
                neon_ui_schema::UiEffect::ImageBinding { node_id, image_id } => {
                    Some((node_id.0.clone(), image_id.clone()))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let canvas_data = fragment
            .effects
            .iter()
            .filter_map(|effect| match effect {
                neon_ui_schema::UiEffect::CanvasData { node_id, data } => {
                    Some((node_id.0.clone(), data.clone()))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut root = fragment.root.clone();
        // Ordinary fragment roots remain viewport-sized for compatibility with
        // surface composition. A root with an authored entry transition must
        // retain its authored target geometry, otherwise the transition is
        // silently rewritten to the viewport before sampling.
        let preserve_authored_root = root
            .enter_transition
            .as_ref()
            .and_then(|transition| transition.from.bounds)
            .is_some_and(|from| {
                root.bounds.width > 0.0
                    && root.bounds.height > 0.0
                    && (root.bounds.x - from.x).abs() > f32::EPSILON
            });
        let root_uses_viewport = !preserve_authored_root;
        if root_uses_viewport {
            root.bounds = UiBounds {
                x: 0.0,
                y: 0.0,
                width: viewport_logical_size[0],
                height: viewport_logical_size[1],
            };
        }
        flatten_node(
            &mut result,
            &fragment.fragment_id.0,
            &root,
            [0.0, 0.0],
            authored_root_clip,
            None,
            None,
            font,
            root_uses_viewport.then_some(viewport_logical_size),
            false,
            &hidden_world_nodes,
            &external_image_bindings,
            &canvas_data,
            None,
            None,
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
                logical_bounds: logical_box_from_bounds(
                    UiBounds {
                        x: grid.bounds.x,
                        y: grid.bounds.y,
                        width: content_width,
                        height: row_height,
                    },
                    Some(UiBounds {
                        x: grid.bounds.x,
                        y: grid.bounds.y,
                        width: content_width,
                        height: row_height,
                    }),
                ),
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
                world_scale: None,
                paint_group_id: 0,
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
                    logical_bounds: logical_box_from_bounds(
                        UiBounds {
                            x: grid.bounds.x,
                            y,
                            width: content_width,
                            height: row_height,
                        },
                        Some(body_clip),
                    ),
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
                    world_scale: None,
                    paint_group_id: grid.paint_group_id,
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
                        let mut presentation = cell
                            .presentation_override
                            .as_ref()
                            .map(data_grid_cell_presentation)
                            .unwrap_or_else(|| data_grid_column_presentation(&column.presentation));
                        // A Select column backed by a Bool is the gallery's
                        // owner toggle. It must be a toggle presentation, not
                        // a Combo with an empty option list.
                        if matches!(
                            column.presentation,
                            neon_ui_schema::UiDataGridPresentation::Select { .. }
                        ) && matches!(cell.value, neon_ui_schema::UiInputValue::Bool { .. }) {
                            presentation = (
                                UiNodeKind::Selectable,
                                Some(UiControlPresentation::Toggle {
                                    selected: matches!(
                                        cell.value,
                                        neon_ui_schema::UiInputValue::Bool { value: true }
                                    ),
                                }),
                            );
                        }
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
        neon_ui_schema::UiInputValue::CanvasData { .. } => "canvas_data".into(),
        neon_ui_schema::UiInputValue::Struct { fields } => format!("{{{} fields}}", fields.len()),
        neon_ui_schema::UiInputValue::Array { elements, .. } => format!("[{} elements]", elements.len()),
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

fn scale_world_bounds(bounds: UiBounds, scale: Option<f32>, origin: Option<[f32; 2]>) -> UiBounds {
    let (Some(scale), Some(origin)) = (scale, origin) else {
        return bounds;
    };
    UiBounds {
        x: origin[0] + (bounds.x - origin[0]) * scale,
        y: origin[1] + (bounds.y - origin[1]) * scale,
        width: bounds.width * scale,
        height: bounds.height * scale,
    }
}

/// Emits Canvas marks directly at their owning Canvas node's tree position.
/// This preserves fragment/sibling painter order and prevents an accidental
/// global overlay pass. Geometry is clipped to the Canvas bounds before it is
/// handed to the ordinary UI panel pipeline.
#[cfg(test)]
fn append_canvas_marks(
    out: &mut Vec<(String, Option<String>, UiVisual, Option<UiTransition>)>,
    canvas_path: &str,
    bounds: &UiBounds,
    inherited_clip: &UiBounds,
    node: &UiNode,
    data: &neon_ui_schema::UiCanvasData,
    world_scale: Option<f32>,
    _world_origin: Option<[f32; 2]>,
    world_depth: Option<f32>,
) {
    let clip = intersect_clip(Some(*inherited_clip), *bounds);
    let mut push = |suffix: String, rect: UiBounds, color: [f32; 4], radius: f32| {
        let rect = intersect_clip(Some(rect), clip);
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let mut style = node.style;
        style.background_color = color;
        style.border_color = color;
        style.border_width = 0.0;
        style.corner_radius = radius.min(rect.width.min(rect.height) * 0.5);
        out.push((
            format!("{canvas_path}/{suffix}"),
            Some(canvas_path.into()),
            UiVisual {
                bounds: rect,
                logical_bounds: LogicalLayoutBox {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    content_x: rect.x,
                    content_y: rect.y,
                    content_width: rect.width,
                    content_height: rect.height,
                    clip: Some(clip),
                },
                style,
                kind: UiNodeKind::Panel,
                enabled: false,
                clip,
                clip_radius: 0.0,
                image: None,
                surface: None,
                text: None,
                presentation: None,
                scroll: false,
                declared_scroll_offset: [0.0; 2],
                world_depth,
                world_scale,
                paint_group_id: 0,
            },
            None,
        ));
    };
    for point in &data.points {
        let radius = point.radius;
        push(
            format!("__canvas_point.{}", point.id),
            UiBounds {
                x: bounds.x + point.position[0] - radius,
                y: bounds.y + point.position[1] - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            },
            point.color,
            radius,
        );
    }
    for line in &data.lines {
        let min_x = line.start[0].min(line.end[0]);
        let min_y = line.start[1].min(line.end[1]);
        push(
            format!("__canvas_line.{}", line.id),
            UiBounds {
                x: bounds.x + min_x - line.width * 0.5,
                y: bounds.y + min_y - line.width * 0.5,
                width: (line.start[0] - line.end[0]).abs().max(line.width) + line.width,
                height: (line.start[1] - line.end[1]).abs().max(line.width) + line.width,
            },
            line.color,
            line.width * 0.5,
        );
    }
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
    external_image_bindings: &HashMap<String, String>,
    canvas_data: &HashMap<String, neon_ui_schema::UiCanvasData>,
    inherited_depth: Option<f32>,
    inherited_scale: Option<f32>,
    inherited_world_origin: Option<[f32; 2]>,
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
    let world_scale = node.world_scale.or(inherited_scale);
    let world_origin = inherited_world_origin
        .or_else(|| world_scale.map(|_| [bounds.x + bounds.width * 0.5, bounds.y + bounds.height]));
    // Keep layout-space bounds untouched. World scale is applied once to the
    // final visual below; children continue to resolve against the stable
    // logical subtree, never against a previously scaled parent.
    let visual_bounds = scale_world_bounds(bounds, world_scale, world_origin);
    let top_layer = inherited_top_layer
        || matches!(
            node.kind,
            UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog | UiNodeKind::ContextMenu
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
    // ContextMenu nodes are always included in the flattened list; their
    // actual visibility is controlled by the active_context_menu_id filter
    // that runs after flattening. Other invisible nodes are skipped here.
    if (!node.visible && !matches!(node.kind, UiNodeKind::ContextMenu))
        || hidden_world_nodes.contains(node.node_id.0.as_str())
    {
        return;
    }
    if node.style.opacity > 0.0 {
        let node_path = format!("{fragment_id}/{}", node.node_id.0);
        let logical_bounds = LogicalLayoutBox {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
            content_x: bounds.x + node_layout.padding[3],
            content_y: bounds.y + node_layout.padding[0],
            content_width: (bounds.width - node_layout.padding[1] - node_layout.padding[3])
                .max(0.0),
            content_height: (bounds.height - node_layout.padding[0] - node_layout.padding[2])
                .max(0.0),
            clip: own_clip,
        };
        out.push((
            node_path.clone(),
            parent_id.map(str::to_owned),
            UiVisual {
                bounds: visual_bounds,
                logical_bounds,
                style: node.style,
                kind: node.kind.clone(),
                enabled: node.enabled,
                clip: scale_world_bounds(effective_clip, world_scale, world_origin),
                clip_radius: own_clip_radius.unwrap_or(0.0),
                image: external_image_bindings
                    .get(&node.node_id.0)
                    .map(|image_id| AssetRef {
                        project_id: format!("external:{image_id}"),
                        asset_id: 0,
                        revision: neon_protocol::Revision(0),
                        kind: "image".into(),
                    })
                    .or_else(|| node.image.clone()),
                surface: node.surface.clone(),
                text: node.text.clone(),
                presentation: None,
                scroll: node_layout.clip == UiClipPolicy::Scroll,
                declared_scroll_offset: node_layout.scroll_offset,
                world_depth: node.world_depth.or(inherited_depth),
                world_scale,
                paint_group_id: 0,
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
            external_image_bindings,
            canvas_data,
            node.world_depth.or(inherited_depth),
            world_scale,
            world_origin,
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
    let intrinsic_value = if height { intrinsic[1] } else { intrinsic[0] };
    // A scroll container's explicit dimension is its viewport, not an
    // intrinsic-content minimum. Its children contribute to scroll extent and
    // are translated during composition; expanding the container here would
    // collapse max_offset to zero and make the whole scroll panel grow with
    // its content.
    if node
        .layout
        .is_some_and(|value| value.clip == UiClipPolicy::Scroll)
        && declared > 0.0
    {
        return clamp_dimension(declared, layout, height);
    }
    // §4.1: explicit w/h is a minimum guarantee, not a ceiling. If the
    // intrinsic content is larger, the resolved size grows to accommodate
    // it.  This prevents text from being silently clipped by a fixed height.
    let mut value = if declared > 0.0 {
        declared.max(intrinsic_value)
    } else {
        intrinsic_value
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
    if let Some(text_ref) = node.text.as_ref() {
        let rich_text;
        let text = match text_ref {
            TextRef::Rich { spans } => {
                rich_text = spans.iter().map(|span| span.value.as_str()).collect::<String>();
                rich_text.as_str()
            }
            _ => match text_ref_value(text_ref) {
                Some(text) => text,
                None => return [0.0, 0.0],
            },
        };
        let line_height = font.map_or(FONT_RASTER_SIZE, |font| font.line_height);
        let advance = |_idx: usize, ch: char| {
            font.map_or_else(
                || {
                    if ch.is_ascii() {
                        FONT_RASTER_SIZE * 0.5
                    } else {
                        FONT_RASTER_SIZE
                    }
                },
                |font| font.font.metrics(ch, FONT_RASTER_SIZE).advance_width,
            )
        };
        // Use the same safe inset as layout_text to ensure intrinsic and
        // actual measurement agree on the available width.
        let text_inset = text_safe_inset(&node.kind);
        let available_width = if node.bounds.width > text_inset {
            (node.bounds.width - text_inset).max(1.0)
        } else {
            text.chars().enumerate().map(|(i, ch)| advance(i, ch)).sum::<f32>()
        };
        // intrinsic_size and layout_text share break_text_lines via
        // measure_text_lines, so the line count and max width always match
        // what is actually drawn.
        let measure = measure_text_lines(text, available_width, line_height, &advance);
        return [
            if node.bounds.width > 0.0 {
                node.bounds.width
            } else {
                measure.max_line_width + text_inset
            },
            (node.bounds.height.max(measure.total_height)),
        ];
    }
    let layout = node.layout.unwrap_or_default();
    if node.children.is_empty() {
        return [0.0, 0.0];
    }
    let children = node
        .children
        .iter()
        .filter(|child| child.visible)
        .map(|child| intrinsic_size(child, font))
        .collect::<Vec<_>>();
    let gap = layout.gap * children.len().saturating_sub(1) as f32;
    let padding_width = layout.padding[1] + layout.padding[3];
    let padding_height = layout.padding[0] + layout.padding[2];
    match layout.mode {
        UiLayoutMode::Row => [
            node.bounds
                .width
                .max(children.iter().map(|size| size[0]).sum::<f32>() + gap + padding_width),
            node.bounds
                .height
                .max(children.iter().map(|size| size[1]).fold(0.0, f32::max) + padding_height),
        ],
        UiLayoutMode::Column => [
            node.bounds
                .width
                .max(children.iter().map(|size| size[0]).fold(0.0, f32::max) + padding_width),
            node.bounds
                .height
                .max(children.iter().map(|size| size[1]).sum::<f32>() + gap + padding_height),
        ],
        _ => [0.0, 0.0],
    }
}

fn is_interactive_control(kind: &UiNodeKind) -> bool {
    component_spec(kind).capabilities.interactive
}

fn resolve_children(
    node: &UiNode,
    _bounds: UiBounds,
    parent_layout: UiLayout,
    inner: UiBounds,
    font: Option<&ResidentFont>,
) -> Vec<UiBounds> {
    if format!("{:?}", node.node_id).contains("split-container") {
        for child in &node.children {
            splitter_debug(&format!("[LAYOUT] child={:?} bounds=({},{},{},{}) mode={:?}", child.node_id, child.bounds.x, child.bounds.y, child.bounds.width, child.bounds.height, parent_layout.mode));
        }
    }
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
                    // Scroll is a final subtree transform. Keep absolute
                    // layout coordinates stable here so track calculation,
                    // text layout, hit geometry, and composition all share
                    // one logical coordinate system.
                    x: inner.x + child.bounds.x,
                    y: inner.y + child.bounds.y,
                    width,
                    height,
                }
            })
            .collect();
    }
    let row = parent_layout.mode == UiLayoutMode::Row;
    let available = if row { inner.width } else { inner.height };
    // Flow containers support two intentional child modes. The ordinary case
    // has no authored offset and participates in the row/column track. A child
    // with an authored x or y is positioned relative to the parent's content
    // box, does not consume a flex track, and therefore cannot shift siblings.
    // Previously offsets were silently ignored by row/column layout, which made
    // expanded branch content overlap at the flow origin.
    let participates_in_flow = |child: &UiNode| {
        child.visible && child.bounds.x == 0.0 && child.bounds.y == 0.0
    };
    let participating_count = node.children.iter().filter(|child| participates_in_flow(child)).count();
    let mut main_sizes = node
        .children
        .iter()
        .map(|child| {
            if !participates_in_flow(child) {
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
            if !participates_in_flow(child) {
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
        if participates_in_flow(child) {
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
                if !participates_in_flow(child) {
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
            if !child.visible && !matches!(child.kind, UiNodeKind::ContextMenu) {
                return UiBounds {
                    x: inner.x,
                    y: inner.y,
                    width: 0.0,
                    height: 0.0,
                };
            }
            let layout = child.layout.unwrap_or_default();
            if !participates_in_flow(child) {
                return UiBounds {
                    x: inner.x + child.bounds.x,
                    y: inner.y + child.bounds.y,
                    width: resolved_dimension(child.bounds.width, child, &layout, font, false),
                    height: resolved_dimension(child.bounds.height, child, &layout, font, true),
                };
            }
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
                    x: inner.x + cursor,
                    y: inner.y + cross_offset,
                    width: main_sizes[index],
                    height: cross_size,
                }
            } else {
                UiBounds {
                    x: inner.x + cross_offset,
                    y: inner.y + cursor,
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
    let presentation = match (&target.presentation, from.numeric_value) {
        (Some(UiControlPresentation::Numeric { min, max, .. }), Some(value)) => {
            Some(UiControlPresentation::Numeric {
                value,
                min: *min,
                max: *max,
            })
        }
        _ => target.presentation.clone(),
    };
    let bounds = from.bounds.unwrap_or(target.bounds);
    UiVisual {
        bounds,
        logical_bounds: logical_box_from_bounds(bounds, from_bounds_clip(from.bounds, target)),
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
        presentation,
        scroll: target.scroll,
        declared_scroll_offset: target.declared_scroll_offset,
        world_depth: target.world_depth,
        world_scale: target.world_scale,
        paint_group_id: target.paint_group_id,
    }
}

fn from_bounds_clip(from: Option<UiBounds>, target: &UiVisual) -> Option<UiBounds> {
    // Logical clip follows the animated bounds box while keeping the inherited
    // projection clip unchanged.
    from.map(|_| target.logical_bounds.clip.unwrap_or(target.clip))
        .or(target.logical_bounds.clip)
}

fn sample_transition(active: &ActiveTransition, time_seconds: f32) -> UiVisual {
    let elapsed_ms = ((time_seconds - active.started_at_seconds) * 1000.0).max(0.0);
    let progress = ((elapsed_ms - active.transition.delay_ms as f32)
        / active.transition.duration_ms as f32)
        .clamp(0.0, 1.0);
    let t = ease(progress, active.transition.easing);
    let presentation = match (&active.from.presentation, &active.target.presentation) {
        (
            Some(UiControlPresentation::Numeric {
                value: from_value, ..
            }),
            Some(UiControlPresentation::Numeric {
                value: target_value,
                min,
                max,
            }),
        ) => Some(UiControlPresentation::Numeric {
            value: lerp(*from_value, *target_value, t),
            min: *min,
            max: *max,
        }),
        _ => active.target.presentation.clone(),
    };
    let bounds = lerp_bounds(active.from.bounds, active.target.bounds, t);
    UiVisual {
        bounds,
        logical_bounds: logical_box_from_bounds(bounds, active.target.logical_bounds.clip),
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
        presentation,
        scroll: active.target.scroll,
        declared_scroll_offset: active.target.declared_scroll_offset,
        world_depth: active.target.world_depth,
        world_scale: active.target.world_scale,
        paint_group_id: active.target.paint_group_id,
    }
}

/// Build a uniform logical box whose content area equals its bounds (no
/// padding). Used for visuals constructed outside `flatten_node`, such as
/// transition intermediates and data grid rows.
fn logical_box_from_bounds(bounds: UiBounds, clip: Option<UiBounds>) -> LogicalLayoutBox {
    LogicalLayoutBox {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
        content_x: bounds.x,
        content_y: bounds.y,
        content_width: bounds.width,
        content_height: bounds.height,
        clip,
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

fn format_easing(easing: UiEasing) -> &'static str {
    match easing {
        UiEasing::Linear => "linear",
        UiEasing::EaseIn => "ease_in",
        UiEasing::EaseOut => "ease_out",
        UiEasing::EaseInOut => "ease_in_out",
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

fn translate_bounds(bounds: UiBounds, offset: [f32; 2]) -> UiBounds {
    UiBounds {
        x: bounds.x + offset[0],
        y: bounds.y + offset[1],
        ..bounds
    }
}

fn union_bounds(left: UiBounds, right: UiBounds) -> UiBounds {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let right_edge = (left.x + left.width).max(right.x + right.width);
    let bottom_edge = (left.y + left.height).max(right.y + right.height);
    UiBounds {
        x,
        y,
        width: (right_edge - x).max(0.0),
        height: (bottom_edge - y).max(0.0),
    }
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
        ServiceName, UiImageSource,
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
            bytes: include_bytes!("../assets/fonts/SarasaUiSC-Light.ttf").to_vec(),
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
    fn button_skin_selection_uses_local_pressed_hover_idle_fallback() {
        let skin = UiControlSkin {
            key: "pulse".into(),
            component_kind: UiNodeKind::Button,
            slots: vec![
                UiSkinSlot {
                    slot_kind: UiSkinSlotKind::Body,
                    state: UiVisualState::Normal,
                    presentation: UiSkinPresentation::Image { resource_key: "idle".into(), fit: UiImageFit::Stretch },
                },
                UiSkinSlot {
                    slot_kind: UiSkinSlotKind::Body,
                    state: UiVisualState::Hover,
                    presentation: UiSkinPresentation::Image { resource_key: "hover".into(), fit: UiImageFit::Stretch },
                },
            ],
        };
        let resource = |hovered, pressed| match &select_button_skin_slot(&skin, hovered, pressed).unwrap().presentation {
            UiSkinPresentation::Image { resource_key, .. } => resource_key.as_str(),
            _ => unreachable!(),
        };
        assert_eq!(resource(false, false), "idle");
        assert_eq!(resource(true, false), "hover");
        assert_eq!(resource(true, true), "hover");
    }

    #[test]
    fn slider_skin_selection_uses_required_slots_and_local_state_fallback() {
        let skin = UiControlSkin {
            key: "volume".into(),
            component_kind: UiNodeKind::Slider,
            slots: vec![
                UiSkinSlot { slot_kind: UiSkinSlotKind::Track, state: UiVisualState::Normal, presentation: UiSkinPresentation::Default },
                UiSkinSlot { slot_kind: UiSkinSlotKind::Fill, state: UiVisualState::Active, presentation: UiSkinPresentation::Default },
                UiSkinSlot { slot_kind: UiSkinSlotKind::Thumb, state: UiVisualState::Hover, presentation: UiSkinPresentation::Default },
            ],
        };
        assert_eq!(select_slider_skin_slot(&skin, UiSkinSlotKind::Track, false, false).unwrap().slot_kind, UiSkinSlotKind::Track);
        assert_eq!(select_slider_skin_slot(&skin, UiSkinSlotKind::Fill, true, true).unwrap().state, UiVisualState::Active);
        assert_eq!(select_slider_skin_slot(&skin, UiSkinSlotKind::Thumb, true, true).unwrap().state, UiVisualState::Hover);
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
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 8.0,
                    y: 8.0,
                    width: 20.0,
                    height: 20.0,
                },
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 160.0,
                    height: 80.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let target_a = UiVisual {
            bounds: UiBounds {
                x: 48.0,
                y: 8.0,
                width: 40.0,
                height: 40.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 48.0,
                    y: 8.0,
                    width: 40.0,
                    height: 40.0,
                },
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 160.0,
                    height: 80.0,
                }),
            ),
            ..source.clone()
        };
        let target_b = UiVisual {
            bounds: UiBounds {
                x: 104.0,
                y: 8.0,
                width: 40.0,
                height: 40.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 104.0,
                    y: 8.0,
                    width: 40.0,
                    height: 40.0,
                },
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 160.0,
                    height: 80.0,
                }),
            ),
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
                paint_group_id: 0,
            },
            PlannedNode {
                id: "fixture/target-a".into(),
                parent_id: None,
                target: target_a.clone(),
                transition: None,
                instance_index: None,
                paint_group_id: 0,
            },
            PlannedNode {
                id: "fixture/target-b".into(),
                parent_id: None,
                target: target_b.clone(),
                transition: None,
                instance_index: None,
                paint_group_id: 0,
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
            logical_bounds: logical_box_from_bounds(
                logical,
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let node_path = "grid/assets/data-grid-row-asset-42/cell-name".to_owned();
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: node_path.clone(),
            parent_id: None,
            target: visual.clone(),
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
                logical_bounds: logical_box_from_bounds(bounds, Some(bounds)),
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
                world_scale: None,
                paint_group_id: 0,
            },
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
        assert_eq!(renderer.text_input_debug_snapshot()["active"], true);
        assert_eq!(renderer.text_input_debug_snapshot()["cursor"], 7);
        assert!(renderer.commit_ime_text("X").is_none());
        assert_eq!(renderer.editing.committed, "displayX");
        renderer.focus_text_input(input.clone());
        assert_eq!(renderer.editing.committed, "displayX");
        let (binding, value) = renderer.finish_data_grid_text_input().unwrap();
        assert_eq!(value, "displayX");
        assert_eq!(binding.data_grid_cell.unwrap().column_key, "name");
        assert!(!renderer.data_grid_text_input_active());
        assert_eq!(renderer.text_input_debug_snapshot()["active"], false);

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
                logical_bounds: logical_box_from_bounds(input.bounds, Some(input.bounds)),
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
                world_scale: None,
                paint_group_id: 0,
            },
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
                logical_bounds: logical_box_from_bounds(
                    bounds,
                    Some(UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 200.0,
                        height: 160.0,
                    }),
                ),
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
                world_scale: None,
                paint_group_id: 0,
            },
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
        renderer.close_dropdown();
        renderer.plan[0].target.kind = UiNodeKind::Combo;
        renderer.set_pointer_position([30.0, 40.0]);
        assert!(renderer.toggle_dropdown_at_pointer());
        assert!(renderer.dropdown_debug_snapshot()["popup"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.len() == 2));
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
            logical_bounds: logical_box_from_bounds(
                bounds,
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 100.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.plan.push(PlannedNode {
            id: node_path.clone(),
            parent_id: None,
            target: visual.clone(),
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 10.0,
                    y: 10.0,
                    width: 40.0,
                    height: 20.0,
                },
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        renderer.plan.push(PlannedNode {
            id: "gallery/slider".into(),
            parent_id: None,
            target: visual,
            transition: None,
            instance_index: None,
            paint_group_id: 0,
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
    fn focus_uses_topmost_plan_order_instead_of_hash_map_iteration_order() {
        let (device, _queue) = test_device("neon3-ui-focus-plan-order");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let mut lower = node();
        lower.kind = UiNodeKind::Button;
        lower.bounds = UiBounds {
            x: 10.0,
            y: 10.0,
            width: 60.0,
            height: 30.0,
        };
        lower.enter_transition = None;
        let mut upper = lower.clone();
        upper.node_id = UiNodeId("upper".into());
        upper.bounds.x = 20.0;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("focus-order".into()),
            revision: Revision(1),
            root: {
                let mut root = node();
                root.enter_transition = None;
                root.children = vec![lower, upper];
                root
            },
            effects: Vec::new(),
        };
        let fragments = HashMap::from([(UiFragmentId("focus-order".into()), fragment)]);
        renderer.refresh_plan(&fragments, [128.0, 96.0]);
        renderer.refresh_hit_bindings(&fragments);
        renderer.compose_sampled_visuals(0.0);
        renderer.set_pointer_position([30.0, 20.0]);
        assert!(renderer.focus_control_at_pointer());
        assert_eq!(
            renderer.focused_control.as_deref(),
            Some("focus-order/upper")
        );
    }

    #[test]
    fn toggle_prediction_changes_visual_immediately_and_returns_semantic_value() {
        let (device, _queue) = test_device("neon3-ui-toggle-prediction");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let mut toggle = node();
        toggle.node_id = UiNodeId("feature-toggle".into());
        toggle.kind = UiNodeKind::Checkbox;
        toggle.bounds = UiBounds {
            x: 10.0,
            y: 10.0,
            width: 80.0,
            height: 30.0,
        };
        toggle.enter_transition = None;
        let mut root = node();
        root.enter_transition = None;
        root.children = vec![toggle];
        let fragment_id = UiFragmentId("toggle-prediction".into());
        let fragment = UiFragment {
            fragment_id: fragment_id.clone(),
            revision: Revision(1),
            root,
            effects: vec![UiEffect::ControlPresentation {
                node_id: UiNodeId("feature-toggle".into()),
                state: UiControlPresentation::Toggle { selected: true },
            }],
        };
        let fragments = HashMap::from([(fragment_id, fragment)]);
        renderer.refresh_plan(&fragments, [128.0, 96.0]);
        renderer.refresh_hit_bindings(&fragments);
        renderer.compose_sampled_visuals(0.0);
        let (value, commit) = renderer
            .finish_toggle_control("toggle-prediction/feature-toggle")
            .unwrap();
        assert_eq!(value, UiSemanticPayloadValue::Bool { value: false });
        assert!(matches!(commit, LocalPresentationCommit::Value { .. }));
        assert!(matches!(
            renderer
                .value_previews
                .get("toggle-prediction/feature-toggle"),
            Some(UiSemanticPayloadValue::Bool { value: false })
        ));
    }

    #[test]
    fn component_spec_covers_all_declared_node_kinds_without_duplicate_policy() {
        let kinds = [
            UiNodeKind::Panel,
            UiNodeKind::Label,
            UiNodeKind::Button,
            UiNodeKind::Image,
            UiNodeKind::RenderSurface,
            UiNodeKind::TextInput,
            UiNodeKind::Checkbox,
            UiNodeKind::RadioButton,
            UiNodeKind::Slider,
            UiNodeKind::DragValue,
            UiNodeKind::Combo,
            UiNodeKind::Dropdown,
            UiNodeKind::Tabs,
            UiNodeKind::Tooltip,
            UiNodeKind::Modal,
            UiNodeKind::Dialog,
            UiNodeKind::Selectable,
            UiNodeKind::ListBox,
            UiNodeKind::Scrollbar,
            UiNodeKind::ProgressBar,
            UiNodeKind::DataGrid,
        ];
        for kind in kinds {
            let spec = component_spec(&kind);
            assert!(spec.metrics.text_inset >= 0.0);
            if spec.capabilities.interactive {
                assert!(is_interactive_control(&kind));
            }
            if spec.capabilities.popup {
                assert!(matches!(kind, UiNodeKind::Combo | UiNodeKind::Dropdown));
            }
            if spec.capabilities.top_layer {
                assert!(matches!(
                    kind,
                    UiNodeKind::Tooltip | UiNodeKind::Modal | UiNodeKind::Dialog | UiNodeKind::ContextMenu
                ));
            }
        }
        assert_eq!(component_spec(&UiNodeKind::Button).metrics.min_height, 30.0);
        assert_eq!(
            component_spec(&UiNodeKind::TextInput).metrics.text_inset,
            TEXT_INPUT_INSET * 2.0
        );
        assert_eq!(
            component_spec(&UiNodeKind::ProgressBar).metrics.min_height,
            24.0
        );
        assert!(
            component_spec(&UiNodeKind::DataGrid)
                .capabilities
                .virtualized
        );
    }

    #[test]
    fn disabled_style_has_priority_over_hover_and_pressed_style() {
        let normal = default_component_style(&UiNodeKind::Button);
        let style = resolve_component_style(
            &UiNodeKind::Button,
            normal,
            None,
            UiStateFlags {
                hovered: true,
                pressed: true,
                disabled: true,
                ..UiStateFlags::default()
            },
        );
        assert!(style.opacity < normal.opacity);
        assert!(style.background_color[0] < normal.background_color[0]);
    }

    #[test]
    fn selected_and_focus_style_are_resolved_without_changing_layout_metrics() {
        let normal = default_component_style(&UiNodeKind::Checkbox);
        let selected = resolve_component_style(
            &UiNodeKind::Checkbox,
            normal,
            Some(&UiControlPresentation::Toggle { selected: true }),
            UiStateFlags {
                selected: true,
                checked: true,
                focused: true,
                ..UiStateFlags::default()
            },
        );
        assert_ne!(selected.border_color, normal.border_color);
        assert_eq!(
            component_spec(&UiNodeKind::Checkbox).metrics.min_height,
            30.0
        );
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
            logical_bounds: logical_box_from_bounds(viewport, Some(viewport)),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let child = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 300.0,
                    height: 300.0,
                },
                Some(viewport),
            ),
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
                paint_group_id: 0,
            },
            PlannedNode {
                id: "f/content".into(),
                parent_id: Some("f/scroll".into()),
                target: child,
                transition: None,
                instance_index: None,
                paint_group_id: 0,
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
            logical_bounds: logical_box_from_bounds(viewport, Some(viewport)),
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
            world_depth: None,
            world_scale: None,
            paint_group_id: 0,
        };
        let child = UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 300.0,
                    height: 300.0,
                },
                Some(viewport),
            ),
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
                paint_group_id: 0,
            },
            PlannedNode {
                id: "f/content".into(),
                parent_id: Some("f/scroll".into()),
                target: child,
                transition: None,
                instance_index: None,
                paint_group_id: 0,
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
            motion_key: None,
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
            world_scale: None,
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
                world_depth: None,
                world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
                world_depth: None,
                world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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

    #[test]
    fn registered_material_source_compiles_into_an_isolated_pipeline() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, _queue) = test_device("neon3-ui-material-pipeline");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let source = br#"
            fn material(input: MaterialInput) -> vec4<f32> {
                let edge = 1.0 - smoothstep(0.0, 0.1, input.geometry_edge);
                return vec4<f32>(0.5 + edge * 0.2, 0.9, 0.1, 0.2 + edge * 0.4);
            }
        "#;
        renderer.sync_material_packages(&device, &[UiShaderPackage {
            package_id: "test-pulse-material".into(),
            version: 1,
            source_digest: "0000000000000000".into(),
            source_bytes: source.to_vec(),
            entry_point: "material".into(),
            fallback: "standard_ui".into(),
            parameters: Vec::new(),
        }]);
        assert!(renderer.material_pipelines.contains_key("test-pulse-material"));
        assert_eq!(renderer.material_pipelines.len(), 1);
    }

    #[test]
    fn shell_resolver_keeps_cut_polygon_convex_on_narrow_bounds() {
        let cut = resolve_shell_cut([100.0, 40.0], [40.0, 40.0, 40.0, 40.0]);
        assert_eq!(cut, [20.0, 20.0, 20.0, 20.0]);
        assert!(cut[0] + cut[3] <= 100.0);
        assert!(cut[3] + cut[2] <= 100.0);
        assert!(cut[0] + cut[1] <= 40.0);
        assert!(cut[2] + cut[3] <= 40.0);
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
                motion_key: None,
            }),
            children: Vec::new(),
            world_depth: None,
            world_scale: None,
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
    fn gpu_panel_frame_keeps_corners_fixed_and_stretches_edges() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-ui-nine-slice");
        let mut image_node = node();
        image_node.node_id = UiNodeId("frame".into());
        image_node.kind = UiNodeKind::Panel;
        image_node.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 32.0,
            height: 32.0,
        };
        image_node.enter_transition = None;
        image_node.style.background_color = [0.0, 0.0, 0.0, 0.0];
        image_node.style.border_color = [0.0, 0.0, 0.0, 0.0];
        image_node.image = Some(AssetRef {
            project_id: "nine-slice-test".into(),
            asset_id: 1,
            revision: Revision(1),
            kind: "image".into(),
        });
        let source_pixels = [
            [255, 0, 0, 255],
            [16, 16, 16, 255],
            [16, 16, 16, 255],
            [0, 255, 0, 255],
            [16, 16, 16, 255],
            [32, 32, 32, 255],
            [32, 32, 32, 255],
            [16, 16, 16, 255],
            [16, 16, 16, 255],
            [32, 32, 32, 255],
            [32, 32, 32, 255],
            [16, 16, 16, 255],
            [0, 0, 255, 255],
            [16, 16, 16, 255],
            [16, 16, 16, 255],
            [255, 255, 0, 255],
        ];
        let image_bytes = source_pixels.into_iter().flatten().collect::<Vec<_>>();
        let fragment = UiFragment {
            fragment_id: UiFragmentId("nine-slice-fragment".into()),
            revision: Revision(1),
            root: image_node,
            effects: vec![UiEffect::NineSlice {
                node_id: UiNodeId("frame".into()),
                layout: neon_ui_schema::UiNineSlice {
                    source_insets_px: [1, 1, 1, 1],
                    target_insets: [8.0, 8.0, 8.0, 8.0],
                    mode: neon_ui_schema::UiNineSliceMode::Stretch,
                    fill_center: true,
                },
            }],
        };
        let asset = AssetBytes {
            asset: AssetRef {
                project_id: "nine-slice-test".into(),
                asset_id: 1,
                revision: Revision(1),
                kind: "image".into(),
            },
            media_type: "application/x-neon-rgba8".into(),
            width: Some(4),
            height: Some(4),
            bytes: image_bytes,
        };
        let pixels = render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(fragment.fragment_id.clone(), fragment)]),
            [32, 32],
            0.0,
            &[asset],
            Vec::new(),
        );
        let pixel = |x: usize, y: usize| {
            let offset = (y * 32 + x) * 4;
            &pixels[offset..offset + 4]
        };
        assert_eq!(pixel(0, 0), &[255, 0, 0, 255]);
        assert_eq!(pixel(31, 0), &[0, 255, 0, 255]);
        assert_eq!(pixel(0, 31), &[0, 0, 255, 255]);
        assert_eq!(pixel(31, 31), &[255, 255, 0, 255]);
        assert_eq!(pixel(16, 16), &[32, 32, 32, 255]);
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
            world_scale: None,
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
    fn screen_paint_groups_follow_surface_children_and_total_order() {
        let mut root = node();
        root.node_id = UiNodeId("surface".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Overlay,
            ..UiLayout::default()
        });
        root.enter_transition = None;

        let mut lower = node();
        lower.node_id = UiNodeId("lower".into());
        lower.enter_transition = None;
        let mut nested = node();
        nested.node_id = UiNodeId("nested".into());
        nested.enter_transition = None;
        lower.children = vec![nested];

        let mut upper = node();
        upper.node_id = UiNodeId("upper".into());
        upper.enter_transition = None;
        root.children = vec![lower, upper];

        let fragment_id = UiFragmentId("screen-order".into());
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
            [200.0, 100.0],
            None,
        );
        let mut plan = flattened
            .into_iter()
            .enumerate()
            .map(|(index, (id, parent_id, target, transition))| PlannedNode {
                id,
                parent_id,
                target,
                transition,
                instance_index: Some(index),
                paint_group_id: 0,
            })
            .collect::<Vec<_>>();
        assign_paint_group_ids(&mut plan);

        let group = |suffix: &str| {
            plan.iter()
                .find(|node| node.id.ends_with(suffix))
                .expect("screen-order node exists")
                .paint_group_id
        };
        assert_eq!(group("/surface"), 1);
        assert_eq!(group("/lower"), 2);
        assert_eq!(group("/nested"), 2);
        assert_eq!(group("/upper"), 3);

        let no_depth = HashMap::from([(1, None), (2, None), (3, None)]);
        assert_eq!(
            compare_paint_group_order(1, 2, &no_depth),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_paint_group_order(2, 3, &no_depth),
            std::cmp::Ordering::Less
        );
        let mixed_depth = HashMap::from([(1, Some(0.8)), (2, Some(0.2)), (3, None)]);
        assert_eq!(
            compare_paint_group_order(2, 1, &mixed_depth),
            std::cmp::Ordering::Less,
            "far World group must be emitted before near World group"
        );
        assert_eq!(
            compare_paint_group_order(2, 3, &mixed_depth),
            std::cmp::Ordering::Less,
            "World groups must be emitted before Screen groups"
        );
    }

    #[test]
    fn screen_overlay_cannot_reveal_lower_group_text_or_image() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-screen-paint-order");
        let transparent = UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        let mut root = node();
        root.node_id = UiNodeId("surface".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 96.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Overlay,
            ..UiLayout::default()
        });
        root.style = transparent;
        root.enter_transition = None;

        let mut lower = node();
        lower.node_id = UiNodeId("lower-panel".into());
        lower.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 96.0,
        };
        lower.style = UiStyle {
            background_color: [0.24, 0.02, 0.02, 1.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        lower.enter_transition = None;
        let image_asset = AssetRef {
            project_id: "screen-order-gpu".into(),
            asset_id: 1,
            revision: Revision(1),
            kind: "image".into(),
        };
        let image_node = |id: &str, x: f32, y: f32| {
            let mut value = node();
            value.node_id = UiNodeId(id.into());
            value.kind = UiNodeKind::Image;
            value.bounds = UiBounds {
                x,
                y,
                width: 16.0,
                height: 16.0,
            };
            value.style = transparent;
            value.image = Some(image_asset.clone());
            value.enter_transition = None;
            value
        };
        let mut lower_text = node();
        lower_text.node_id = UiNodeId("lower-text".into());
        lower_text.kind = UiNodeKind::Label;
        lower_text.bounds = UiBounds {
            x: 0.0,
            y: 30.0,
            width: 96.0,
            height: 24.0,
        };
        lower_text.style = transparent;
        lower_text.text = Some(TextRef::Literal {
            value: "LOWER".into(),
        });
        lower_text.enter_transition = None;
        lower.children = vec![
            image_node("lower-image-visible", 8.0, 0.0),
            image_node("lower-image-covered", 56.0, 30.0),
            lower_text,
        ];

        let mut upper = node();
        upper.node_id = UiNodeId("upper-panel".into());
        upper.bounds = UiBounds {
            x: 48.0,
            y: 16.0,
            width: 64.0,
            height: 64.0,
        };
        upper.style = UiStyle {
            background_color: [0.0, 0.0, 0.0, 1.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        upper.enter_transition = None;
        root.children = vec![lower, upper];

        let fragment_id = UiFragmentId("screen-order-gpu".into());
        let fragments = HashMap::from([(
            fragment_id.clone(),
            UiFragment {
                fragment_id,
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer
            .preload_image(
                &device,
                &queue,
                &AssetBytes {
                    asset: image_asset,
                    media_type: "application/x-neon-rgba8".into(),
                    width: Some(2),
                    height: Some(2),
                    bytes: vec![255; 2 * 2 * 4],
                },
            )
            .expect("screen-order image must upload");
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [128, 96],
            1.0,
        );
        let bright = |x: u32, y: u32| {
            let offset = ((y * 128 + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 4];
            pixel[0] > 120 && pixel[1] > 120 && pixel[2] > 120 && pixel[3] > 100
        };
        let white = |x: u32, y: u32| {
            let offset = ((y * 128 + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 4];
            pixel[0] > 220 && pixel[1] > 220 && pixel[2] > 220 && pixel[3] > 220
        };
        let visible_image_pixels = (8..24)
            .flat_map(|x| (0..16).map(move |y| (x, y)))
            .filter(|(x, y)| white(*x, *y))
            .count();
        let covered_image_pixels = (56..72)
            .flat_map(|x| (30..46).map(move |y| (x, y)))
            .filter(|(x, y)| white(*x, *y))
            .count();
        let visible_text_pixels = (8..48)
            .flat_map(|x| (26..58).map(move |y| (x, y)))
            .filter(|(x, y)| bright(*x, *y))
            .count();
        let covered_text_pixels = (48..112)
            .flat_map(|x| (26..70).map(move |y| (x, y)))
            .filter(|(x, y)| bright(*x, *y))
            .count();
        let diagnostics = renderer.paint_order_diagnostics();
        let lower_group = renderer
            .plan
            .iter()
            .find(|node| node.id.ends_with("/lower-panel"))
            .expect("lower panel is planned")
            .paint_group_id;
        let upper_group = renderer
            .plan
            .iter()
            .find(|node| node.id.ends_with("/upper-panel"))
            .expect("upper panel is planned")
            .paint_group_id;
        let pass = lower_group < upper_group
            && visible_image_pixels > 0
            && covered_image_pixels == 0
            && visible_text_pixels > 0
            && covered_text_pixels == 0;
        println!(
            "{}",
            json!({
                "probe": "screen-ui-paint-order.v1",
                "frame_sequence": 1,
                "input": {
                    "surface": "screen-order-gpu",
                    "lower_panel": "lower-panel",
                    "upper_panel": "upper-panel",
                    "image": "lower-image-covered",
                    "overlap": {"x": [48, 112], "y": [26, 70]},
                },
                "producer": {
                    "group_order": diagnostics.get("group_order"),
                    "lower_group": lower_group,
                    "upper_group": upper_group,
                    "world_depth": null,
                },
                "consumer": {
                    "buffer_id": "offscreen:screen-order-gpu:f1",
                    "coordinate_space": "logical-pixel",
                    "visible_image_pixels": visible_image_pixels,
                    "covered_image_pixels": covered_image_pixels,
                    "visible_text_pixels": visible_text_pixels,
                    "covered_text_pixels": covered_text_pixels,
                },
                "diagnostic": {
                    "missing_data": visible_image_pixels == 0 || visible_text_pixels == 0,
                    "stale_data": false,
                    "coordinate_mismatch": false,
                    "comparison_direction_error": covered_image_pixels > 0 || covered_text_pixels > 0,
                },
                "result": if pass { "passed" } else { "failed" },
                "pass": pass,
            })
        );
        assert!(pass, "Screen UI overlay order must cover lower text");
    }

    #[test]
    fn material_instances_keep_their_own_gpu_ranges() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-material-instance-ranges");
        let transparent = UiStyle {
            background_color: [0.0, 0.0, 0.0, 0.0],
            border_color: [0.0, 0.0, 0.0, 0.0],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        let mut root = node();
        root.node_id = UiNodeId("surface".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 128.0,
            height: 96.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Overlay,
            ..UiLayout::default()
        });
        root.style = transparent;
        root.enter_transition = None;

        let package = |package_id: &str, color: &str| UiShaderPackage {
            package_id: package_id.into(),
            version: 1,
            source_digest: format!("test-{package_id}"),
            source_bytes: format!(
                "fn material(input: MaterialInput) -> vec4<f32> {{ return vec4<f32>({color}, 1.0); }}"
            )
            .into_bytes(),
            entry_point: "material".into(),
            fallback: "standard_ui".into(),
            parameters: Vec::new(),
        };
        let material = |package_id: &str| UiMaterialRef {
            package_id: package_id.into(),
            ..UiMaterialRef::default()
        };
        let panel = |id: &str, bounds: UiBounds| {
            let mut value = node();
            value.node_id = UiNodeId(id.into());
            value.bounds = bounds;
            value.style = transparent;
            value.enter_transition = None;
            value
        };
        root.children = vec![
            panel(
                "small-panel",
                UiBounds {
                    x: 80.0,
                    y: 60.0,
                    width: 32.0,
                    height: 32.0,
                },
            ),
            panel(
                "large-panel",
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 64.0,
                    height: 64.0,
                },
            ),
        ];
        let fragment_id = UiFragmentId("material-ranges".into());
        let fragments = HashMap::from([(
            fragment_id.clone(),
            UiFragment {
                fragment_id,
                revision: Revision(1),
                root,
                effects: vec![
                    UiEffect::Material {
                        node_id: UiNodeId("small-panel".into()),
                        material: material("a-small-red"),
                    },
                    UiEffect::Material {
                        node_id: UiNodeId("large-panel".into()),
                        material: material("z-large-blue"),
                    },
                ],
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.sync_material_packages(
            &device,
            &[
                package("a-small-red", "1.0, 0.0, 0.0"),
                package("z-large-blue", "0.0, 0.0, 1.0"),
            ],
        );
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [128, 96],
            1.0,
        );
        let pixel = |x: u32, y: u32| {
            let offset = ((y * 128 + x) * 4) as usize;
            [
                pixels[offset],
                pixels[offset + 1],
                pixels[offset + 2],
                pixels[offset + 3],
            ]
        };
        let small_pixel = pixel(96, 76);
        let large_pixel = pixel(20, 20);
        let small_is_red = small_pixel[0] > 200
            && small_pixel[1] < 40
            && small_pixel[2] < 40
            && small_pixel[3] > 200;
        let large_is_blue = large_pixel[2] > 200
            && large_pixel[0] < 40
            && large_pixel[1] < 40
            && large_pixel[3] > 200;
        let diagnostics = renderer.paint_order_diagnostics();
        let pass = small_is_red && large_is_blue;
        println!(
            "{}",
            json!({
                "probe": "material-instance-ranges.v1",
                "frame_sequence": 1,
                "input": {
                    "small_panel": {"bounds": [80, 60, 32, 32], "material": "a-small-red"},
                    "large_panel": {"bounds": [0, 0, 64, 64], "material": "z-large-blue"},
                },
                "producer": {
                    "material_buffer": "dedicated-material-instance-buffer",
                    "group_order": diagnostics.get("group_order"),
                    "material_batches": [
                        {"package": "a-small-red", "instance_count": 1},
                        {"package": "z-large-blue", "instance_count": 1},
                    ],
                },
                "consumer": {
                    "buffer_id": "offscreen:material-ranges:f1",
                    "coordinate_space": "logical-pixel",
                    "small_pixel": small_pixel,
                    "large_pixel": large_pixel,
                },
                "diagnostic": {
                    "missing_data": !small_is_red || !large_is_blue,
                    "stale_data": false,
                    "coordinate_mismatch": false,
                    "comparison_direction_error": false,
                },
                "result": if pass { "passed" } else { "failed" },
                "pass": pass,
            })
        );
        assert!(pass, "material instances must retain their own geometry ranges");
    }

    #[test]
    fn unified_hit_image_uses_last_overlapping_panel_without_bubbling() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-unified-hit-order");
        let mut root = node();
        root.enter_transition = None;
        let child = |id: &str| {
            let mut value = node();
            value.node_id = UiNodeId(id.into());
            value.kind = UiNodeKind::Button;
            value.enter_transition = None;
            value.bounds = UiBounds {
                x: 10.0,
                y: 10.0,
                width: 40.0,
                height: 30.0,
            };
            value
        };
        root.children = vec![child("lower"), child("upper")];
        let invoke = |action: &str| UiIntent::Invoke {
            action: action.into(),
            params: json!({}),
        };
        let fragment = UiFragment {
            fragment_id: neon_ui_schema::UiFragmentId("combined".into()),
            revision: Revision(1),
            root,
            effects: vec![
                neon_ui_schema::UiEffect::BoundSemanticIntent {
                    node_id: UiNodeId("lower".into()),
                    intent: invoke("lower"),
                },
                neon_ui_schema::UiEffect::BoundSemanticIntent {
                    node_id: UiNodeId("upper".into()),
                    intent: invoke("upper"),
                },
            ],
        };
        let fragments = HashMap::from([(fragment.fragment_id.clone(), fragment)]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let hits = render_hit_ids_with_renderer_for_test(
            &mut renderer,
            &device,
            &queue,
            &fragments,
            [100, 80],
        );
        let hit_id = hits[20 * 100 + 20];
        let binding = renderer
            .hit_binding(hit_id)
            .expect("overlap must produce one topmost ID");
        assert_eq!(binding.node_path, "combined/upper");
        assert_eq!(binding.intent, Some(invoke("upper")));
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
                world_depth: None,
                world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
            world_scale: None,
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
            world_depth: None,
            world_scale: None,
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
            world_depth: None,
            world_scale: None,
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
            world_depth: None,
            world_scale: None,
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
    fn flatten_keeps_declared_column_layout_in_logical_coordinates() {
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
                world_depth: None,
                world_scale: None,
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
                world_depth: None,
                world_scale: None,
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
        // Scroll is applied once by sampled composition. Flattening must keep
        // authored logical track positions stable for all children.
        assert_eq!(nodes[1].2.logical_bounds.y, 4.0);
        assert_eq!(nodes[2].2.logical_bounds.y, 16.0);
    }

    #[test]
    fn scroll_composition_moves_whole_panel_without_collapsing_child_tracks() {
        let (device, _queue) = test_device("neon3-scroll-whole-panel");
        let mut first = node();
        first.node_id = UiNodeId("first-label".into());
        first.kind = UiNodeKind::Label;
        first.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 20.0,
        };
        first.text = Some(TextRef::Literal {
            value: "First".into(),
        });
        first.enter_transition = None;
        let mut second = node();
        second.node_id = UiNodeId("second-label".into());
        second.kind = UiNodeKind::Label;
        second.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 20.0,
        };
        second.text = Some(TextRef::Literal {
            value: "Second".into(),
        });
        second.enter_transition = None;
        let mut scroll = node();
        scroll.node_id = UiNodeId("scroll-panel".into());
        scroll.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 30.0,
        };
        scroll.layout = Some(UiLayout {
            mode: UiLayoutMode::Column,
            gap: 2.0,
            clip: UiClipPolicy::Scroll,
            ..UiLayout::default()
        });
        scroll.children = vec![first, second];
        scroll.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("root".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        root.children = vec![scroll];
        root.enter_transition = None;
        let fragment_id = UiFragmentId("scroll-regression".into());
        let fragment = UiFragment {
            fragment_id: fragment_id.clone(),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let fragments = HashMap::from([(fragment_id, fragment)]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([100, 100], [100.0, 100.0]);
        renderer.refresh_plan(&fragments, [100.0, 100.0]);
        renderer
            .scroll_offsets
            .insert("scroll-regression/scroll-panel".into(), [0.0, 10.0]);
        renderer.compose_sampled_visuals(0.0);
        let first = renderer
            .plan
            .iter()
            .position(|node| node.id.ends_with("/first-label"))
            .unwrap();
        let second = renderer
            .plan
            .iter()
            .position(|node| node.id.ends_with("/second-label"))
            .unwrap();
        let first_y = renderer.sampled[first].bounds.y;
        let second_y = renderer.sampled[second].bounds.y;
        assert_eq!(renderer.sampled[first].logical_bounds.y, 0.0);
        assert_eq!(renderer.sampled[second].logical_bounds.y, 22.0);
        assert_eq!(second_y - first_y, 22.0);
        assert_eq!(first_y, -10.0);
        assert_eq!(second_y, 12.0);
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
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 10.0,
                    y: 20.0,
                    width: 100.0,
                    height: 80.0,
                },
                Some(UiBounds {
                    x: -1_000_000.0,
                    y: -1_000_000.0,
                    width: 2_000_000.0,
                    height: 2_000_000.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
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
    fn animation_instance_reports_running_and_completed_lifecycle() {
        let target = UiVisual {
            bounds: UiBounds {
                x: 10.0,
                y: 20.0,
                width: 40.0,
                height: 30.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 10.0,
                    y: 20.0,
                    width: 40.0,
                    height: 30.0,
                },
                None,
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let transition = UiTransition {
            delay_ms: 0,
            duration_ms: 100,
            easing: UiEasing::Linear,
            from: UiTransitionState {
                opacity: Some(0.0),
                ..UiTransitionState::default()
            },
            motion_key: Some("test.motion".into()),
        };
        let active = ActiveTransition {
            from: transition_source(&target, &transition),
            target,
            started_at_seconds: 1.0,
            transition,
        };
        assert_eq!(
            active
                .animation_instance("test/node", Revision(4), 1.05)
                .status,
            UiAnimationStatus::Running
        );
        assert_eq!(
            active
                .animation_instance("test/node", Revision(4), 1.2)
                .status,
            UiAnimationStatus::Completed
        );
    }

    #[test]
    fn cancel_animation_records_cancelled_lifecycle_and_pins_target() {
        let (device, _queue) = test_device("neon3-animation-cancel");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let target = UiVisual {
            bounds: UiBounds {
                x: 5.0,
                y: 7.0,
                width: 40.0,
                height: 30.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 5.0,
                    y: 7.0,
                    width: 40.0,
                    height: 30.0,
                },
                None,
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let transition = UiTransition {
            delay_ms: 0,
            duration_ms: 300,
            easing: UiEasing::Linear,
            from: UiTransitionState {
                opacity: Some(0.0),
                ..UiTransitionState::default()
            },
            motion_key: Some("cancel-test".into()),
        };
        renderer.active.insert(
            "test/cancel".into(),
            ActiveTransition {
                from: transition_source(&target, &transition),
                target: target.clone(),
                started_at_seconds: 1.0,
                transition,
            },
        );
        assert!(renderer.cancel_animation("test/cancel"));
        assert!(!renderer.active.contains_key("test/cancel"));
        assert_eq!(renderer.current["test/cancel"], target);
        assert!(
            matches!(renderer.animation_history.back(), Some(animation) if animation.status == UiAnimationStatus::Cancelled)
        );
    }

    #[test]
    fn retarget_records_superseded_and_uses_current_sample_as_new_from() {
        let (device, _queue) = test_device("neon3-animation-retarget");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let visual = |x: f32| UiVisual {
            bounds: UiBounds {
                x,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                None,
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let transition = UiTransition {
            delay_ms: 0,
            duration_ms: 100,
            easing: UiEasing::Linear,
            from: UiTransitionState {
                bounds: Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                }),
                ..UiTransitionState::default()
            },
            motion_key: Some("retarget".into()),
        };
        let first = visual(100.0);
        renderer.sample_with_history("retarget/node", &first, Some(&transition), 1.0);
        let midpoint = sample_transition(renderer.active.get("retarget/node").unwrap(), 1.05);
        let second = visual(200.0);
        renderer.sample_with_history("retarget/node", &second, Some(&transition), 1.05);
        let active = renderer.active.get("retarget/node").unwrap();
        assert_eq!(active.from.bounds.x, midpoint.bounds.x);
        assert!(
            matches!(renderer.animation_history.back(), Some(animation) if animation.status == UiAnimationStatus::Superseded)
        );
    }

    #[test]
    fn transition_samples_numeric_control_values() {
        let visual = |value: f32| UiVisual {
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 16.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 16.0,
                },
                Some(UiBounds {
                    x: -1_000_000.0,
                    y: -1_000_000.0,
                    width: 2_000_000.0,
                    height: 2_000_000.0,
                }),
            ),
            style: UiStyle::default(),
            kind: UiNodeKind::ProgressBar,
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
            presentation: Some(UiControlPresentation::Numeric {
                value,
                min: 0.0,
                max: 100.0,
            }),
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: None,
            world_scale: None,
            paint_group_id: 0,
        };
        let target = visual(100.0);
        let transition = UiTransition {
            delay_ms: 0,
            duration_ms: 200,
            easing: UiEasing::Linear,
            from: UiTransitionState {
                numeric_value: Some(0.0),
                ..UiTransitionState::default()
            },
            motion_key: None,
        };
        let active = ActiveTransition {
            from: transition_source(&target, &transition),
            target: target.clone(),
            started_at_seconds: 1.0,
            transition,
        };
        match sample_transition(&active, 1.1).presentation {
            Some(UiControlPresentation::Numeric { value, min, max }) => {
                assert!((value - 50.0).abs() < 0.001);
                assert_eq!((min, max), (0.0, 100.0));
            }
            _ => panic!("numeric presentation must be sampled"),
        }
        assert_eq!(
            sample_transition(&active, 1.2).presentation,
            target.presentation
        );
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
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                Some(UiBounds {
                    x: -1_000_000.0,
                    y: -1_000_000.0,
                    width: 2_000_000.0,
                    height: 2_000_000.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
        };
        let target = UiVisual {
            bounds: UiBounds {
                x: 100.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 100.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                Some(UiBounds {
                    x: -1_000_000.0,
                    y: -1_000_000.0,
                    width: 2_000_000.0,
                    height: 2_000_000.0,
                }),
            ),
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
            world_scale: None,
            paint_group_id: 0,
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
                motion_key: None,
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
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                Some(UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                }),
            ),
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
            world_depth: None,
            world_scale: None,
            paint_group_id: 0,
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
                motion_key: None,
            }),
            1.0,
        );
        assert!(renderer.has_active_animation(1.005));
        assert!(!renderer.has_active_animation(1.020));
    }

    #[test]
    fn world_projection_moves_immediately_without_restarting_presentation_motion() {
        let target = UiVisual {
            bounds: UiBounds {
                x: 100.0,
                y: 120.0,
                width: 200.0,
                height: 70.0,
            },
            logical_bounds: logical_box_from_bounds(
                UiBounds {
                    x: 100.0,
                    y: 120.0,
                    width: 200.0,
                    height: 70.0,
                },
                Some(UiBounds {
                    x: 100.0,
                    y: 120.0,
                    width: 200.0,
                    height: 70.0,
                }),
            ),
            style: UiStyle::default(),
            kind: UiNodeKind::Panel,
            enabled: true,
            clip: UiBounds {
                x: 100.0,
                y: 120.0,
                width: 200.0,
                height: 70.0,
            },
            clip_radius: 0.0,
            image: None,
            surface: None,
            text: None,
            presentation: None,
            scroll: false,
            declared_scroll_offset: [0.0; 2],
            world_depth: Some(0.5),
            world_scale: None,
            paint_group_id: 0,
        };
        let transition = UiTransition {
            delay_ms: 0,
            duration_ms: 500,
            easing: UiEasing::EaseInOut,
            from: UiTransitionState {
                bounds: Some(UiBounds {
                    x: 100.0,
                    y: 120.0,
                    width: 200.0,
                    height: 70.0,
                }),
                background_color: Some([0.0, 0.0, 0.0, 1.0]),
                ..UiTransitionState::default()
            },
            motion_key: Some("world-panel".into()),
        };
        let mut current = HashMap::new();
        let mut active = HashMap::new();
        UiWgpuRenderer::sample(
            &mut current,
            &mut active,
            "world/p0",
            &target,
            Some(&transition),
            1.0,
        );
        let mut moved = target.clone();
        moved.bounds.x = 700.0;
        moved.bounds.y = 540.0;
        moved.clip.x = 700.0;
        moved.clip.y = 540.0;
        let sampled = UiWgpuRenderer::sample(
            &mut current,
            &mut active,
            "world/p0",
            &moved,
            Some(&transition),
            1.016,
        );
        assert_eq!([sampled.bounds.x, sampled.bounds.y], [700.0, 540.0]);
        assert_eq!(
            active.len(),
            1,
            "camera motion must not create a new transition"
        );
        assert!(active["world/p0"].started_at_seconds == 1.0);
    }

    #[test]
    fn world_transform_update_does_not_relayout_static_text() {
        let (device, queue) = test_device("neon3-world-transform-counters");
        let mut text = node();
        text.node_id = UiNodeId("world-label".into());
        text.kind = UiNodeKind::Label;
        text.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 24.0,
        };
        text.text = Some(TextRef::Literal {
            value: "World label".into(),
        });
        text.world_scale = Some(1.0);
        text.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("world-root".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        root.world_scale = Some(1.0);
        root.world_depth = Some(0.5);
        root.children = vec![text];
        root.enter_transition = None;
        let fragment_id = UiFragmentId("world-counter".into());
        let fragment = UiFragment {
            fragment_id: fragment_id.clone(),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::from([(fragment_id.clone(), fragment)]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [200, 100],
            0.0,
        );
        let before = renderer.layout_counters();
        let root = &mut fragments
            .get_mut(&fragment_id)
            .expect("world fragment exists")
            .root;
        root.bounds.x = 60.0;
        root.bounds.y = 20.0;
        renderer.invalidate_plan_for_world_transform();
        let _ = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [200, 100],
            0.016,
        );
        let after = renderer.layout_counters();
        assert_eq!(before["text_layout_count"], after["text_layout_count"]);
        assert!(after["layout_count"].as_u64() > before["layout_count"].as_u64());
    }

    #[test]
    fn intrinsic_text_height_accounts_for_wrapped_lines() {
        let mut value = node();
        value.node_id = UiNodeId("wrapped".into());
        value.text = Some(TextRef::Literal {
            value: "选择一个入口，开始观察怪物面板、统一命中图和本地动画。".into(),
        });
        value.bounds.width = 120.0;
        value.bounds.height = 0.0;
        let size = intrinsic_size(&value, None);
        assert!(size[1] > FONT_RASTER_SIZE);
        assert_eq!(size[0], 120.0);
    }

    #[test]
    fn city_copy_width_requires_two_lines_before_font_residency() {
        let mut value = node();
        value.node_id = UiNodeId("city-copy".into());
        value.text = Some(TextRef::Literal {
            value: "选择一个入口，开始观察怪物面板、统一命中图和本地动画。".into(),
        });
        value.bounds.width = 388.0;
        value.bounds.height = 0.0;
        let size = intrinsic_size(&value, None);
        assert_eq!(size[0], 388.0);
        assert!(
            size[1] >= FONT_RASTER_SIZE * 2.0,
            "city copy must reserve two lines, got {}",
            size[1]
        );
    }

    #[test]
    fn declared_button_height_is_preserved_by_auto_parent_measurement() {
        let mut button = node();
        button.node_id = UiNodeId("city-enter".into());
        button.kind = UiNodeKind::Button;
        button.text = Some(TextRef::Literal {
            value: "进入城市".into(),
        });
        button.bounds.width = 388.0;
        button.bounds.height = 48.0;
        let size = intrinsic_size(&button, None);
        assert_eq!(size[0], 388.0);
        assert!(size[1] >= 48.0);
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

    #[test]
    fn ui_instance_abi_matches_vertex_attributes() {
        // The color/depth vertex buffer layout encodes offsets 0..=168 and the
        // WGSL `VsIn` reads locations 0..=11 as Float32x4/Float32. Freeze the
        // `#[repr(C)]` layout so a future field reorder cannot silently break
        // the shader bindings.
        assert_eq!(std::mem::size_of::<UiInstance>(), 184);
        assert_eq!(std::mem::align_of::<UiInstance>(), 4);
        #[rustfmt::skip]
        let offsets = [
            (0,   16), // rect
            (16,  16), // fill
            (32,  16), // border
            (48,  16), // params
            (64,  16), // clip
            (80,  4),  // depth (f32)
            (84,  4),  // paint_group_id (u32)
            (88,  16), // from_rect
            (104, 16), // from_fill
            (120, 16), // from_border
            (136, 16), // from_params
            (152, 16), // animation
            (168, 16), // cut
        ];
        let mut cursor = 0usize;
        for (offset, size) in offsets {
            assert_eq!(cursor, offset, "UiInstance field offset drift at {cursor}");
            cursor += size;
        }
        assert_eq!(cursor, std::mem::size_of::<UiInstance>());
        let instance = UiInstance::zeroed();
        assert_eq!(instance.animation, [0.0; 4]);
        assert_eq!(instance.cut, [0.0; 4]);
        assert_eq!(instance.depth, 0.0);
        assert_eq!(instance.paint_group_id, 0);
    }

    #[test]
    fn ui_view_abi_matches_time_uniform() {
        // viewport(8) + color_mode(4) + time_seconds(4) + extras(10×16=160) = 176
        assert_eq!(std::mem::size_of::<UiView>(), 176);
        assert_eq!(std::mem::align_of::<UiView>(), 4);
        let view = UiView {
            viewport: [12.0, 34.0],
            color_mode: 1,
            time_seconds: 1.25,
            extras: [[0.0; 4]; 10],
        };
        let bytes = bytemuck::bytes_of(&view);
        assert_eq!(&bytes[0..4], &12.0f32.to_ne_bytes());
        assert_eq!(&bytes[4..8], &34.0f32.to_ne_bytes());
        assert_eq!(&bytes[8..12], &1u32.to_ne_bytes());
        assert_eq!(&bytes[12..16], &1.25f32.to_ne_bytes());
    }

    // ── Intrinsic text measurement (§14.1) ────────────────────────────

    const TEST_LINE_HEIGHT: f32 = 20.0;

    fn fixed_advance(_idx: usize, ch: char) -> f32 {
        if ch.is_ascii() { 8.0 } else { 16.0 }
    }

    #[test]
    fn measure_ascii_single_line() {
        let m = measure_text_lines("Hello", 200.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 1);
        assert_eq!(m.max_line_width, 40.0); // 5 × 8
        assert_eq!(m.total_height, TEST_LINE_HEIGHT);
    }

    #[test]
    fn measure_cjk_single_line() {
        // 地(16) 形(16) U(8) I(8) = 48, no wrapping at width 200
        let m = measure_text_lines("地形UI", 200.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 1);
        assert_eq!(m.max_line_width, 48.0);
    }

    #[test]
    fn measure_cjk_auto_wrap_keeps_latin_word_intact() {
        // 地(16) 形(16) [UI word=16] 测(16) 试(16) = 80 total.
        // Width 40: "UI" is one word and must not be split.
        //   line1: 地形 (32) — UI would overflow, break before it
        //   line2: UI测 (8+8+16=32) — 试 would overflow, break before it
        //   line3: 试 (16)
        // → 3 lines, max width 32
        let m = measure_text_lines("地形UI测试", 40.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 3);
        assert_eq!(m.max_line_width, 32.0);
        assert_eq!(m.total_height, TEST_LINE_HEIGHT * 3.0);
        let lines = break_text_lines("地形UI测试", 40.0, &fixed_advance);
        assert_eq!(lines[0].iter().collect::<String>(), "地形");
        assert_eq!(lines[1].iter().collect::<String>(), "UI测");
        assert_eq!(lines[2].iter().collect::<String>(), "试");
    }

    #[test]
    fn measure_explicit_newline() {
        let m = measure_text_lines("A\nBC", 200.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 2);
        // First line: "A" → 8, second line: "BC" → 16
        assert_eq!(m.max_line_width, 16.0);
    }

    #[test]
    fn measure_overflow_single_token() {
        // "Super" = 5 × 8 = 40, width = 16, so it wraps: Su/pe/r → 3 lines
        let m = measure_text_lines("Super", 16.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 3);
        assert_eq!(m.max_line_width, 16.0); // each line = 2 chars × 8
    }

    #[test]
    fn measure_emoji_uses_fallback_advance() {
        // For non-ASCII, fixed_advance returns 16 per char
        let m = measure_text_lines("😀🌊", 200.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 1);
        assert_eq!(m.max_line_width, 32.0);
    }

    #[test]
    fn measure_font_not_loaded_fallback() {
        // When font is None, intrinsic_size falls back to FONT_RASTER_SIZE estimates
        let node = UiNode {
            node_id: UiNodeId("fallback-test".into()),
            kind: UiNodeKind::Label,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: Some(TextRef::Literal {
                value: "Hello".into(),
            }),
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            children: Vec::new(),
            world_depth: None,
            world_scale: None,
        };
        let [width, height] = intrinsic_size(&node, None);
        // Without font, each ASCII char = FONT_RASTER_SIZE * 0.5 = 8.0
        // 5 chars × 8.0 = 40.0 + inset 8 = 48.0 (auto width)
        assert_eq!(width, 48.0);
        // Height = 1 line × FONT_RASTER_SIZE = 16
        assert_eq!(height, 16.0);
    }

    #[test]
    fn measure_break_text_lines_roundtrip() {
        // Verify that break_text_lines and measure_text_lines agree
        let text = "Hello\nWorld";
        let width = 100.0;
        let advance = &fixed_advance;
        let lines = break_text_lines(text, width, advance);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].iter().collect::<String>(), "Hello");
        assert_eq!(lines[1].iter().collect::<String>(), "World");
        let m = measure_text_lines(text, width, TEST_LINE_HEIGHT, advance);
        assert_eq!(m.line_count as usize, lines.len());
    }

    #[test]
    fn measure_english_breaks_at_word_boundary() {
        // "Hello World" = Hello(40) + space(8) + World(40) = 88
        // Width 56: Hello(40) fits; space+World would be 48, 40+48=88>56
        //   → break before World, line1 = "Hello" (40), line2 = "World" (40)
        let m = measure_text_lines("Hello World", 56.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 2);
        assert_eq!(m.max_line_width, 40.0);
        let lines = break_text_lines("Hello World", 56.0, &fixed_advance);
        assert_eq!(lines[0].iter().collect::<String>(), "Hello");
        assert_eq!(lines[1].iter().collect::<String>(), "World");
    }

    #[test]
    fn measure_long_word_overflows_char_by_char() {
        // "Super" = 5×8=40, width=16. Word itself overflows → split char by char.
        // Su(16) / pe(16) / r(8) → 3 lines
        let m = measure_text_lines("Super", 16.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 3);
        assert_eq!(m.max_line_width, 16.0);
        let lines = break_text_lines("Super", 16.0, &fixed_advance);
        assert_eq!(lines[0].iter().collect::<String>(), "Su");
        assert_eq!(lines[1].iter().collect::<String>(), "pe");
        assert_eq!(lines[2].iter().collect::<String>(), "r");
    }

    #[test]
    fn measure_spaces_collapsed_at_line_edges() {
        // "A  B" width 24: A(8)+space(8)+space(8)=24 fits on line1.
        // B would overflow → line2 = "B". Trailing spaces on line1 are stripped.
        let lines = break_text_lines("A  B", 24.0, &fixed_advance);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].iter().collect::<String>(), "A");
        assert_eq!(lines[1].iter().collect::<String>(), "B");
    }

    #[test]
    fn measure_trailing_space_that_overflows_is_dropped() {
        // "AB C" width 24: AB(16)+space(8)=24 exactly fits.
        // C(8) would overflow → line2 = "C". Trailing space on line1 is stripped.
        let lines = break_text_lines("AB C", 24.0, &fixed_advance);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].iter().collect::<String>(), "AB");
        assert_eq!(lines[1].iter().collect::<String>(), "C");
    }

    #[test]
    fn measure_cjk_breaks_between_any_chars() {
        // Pure CJK: 地(16) 形(16) 编(16) 辑(16) = 64, width 32
        // → [地形](32) / [编辑](32), 2 lines
        let m = measure_text_lines("地形编辑", 32.0, TEST_LINE_HEIGHT, &fixed_advance);
        assert_eq!(m.line_count, 2);
        assert_eq!(m.max_line_width, 32.0);
        let lines = break_text_lines("地形编辑", 32.0, &fixed_advance);
        assert_eq!(lines[0].iter().collect::<String>(), "地形");
        assert_eq!(lines[1].iter().collect::<String>(), "编辑");
    }

    #[test]
    fn measure_explicit_newline_creates_empty_line() {
        // "A\n\nB" → 3 lines: "A", "", "B"
        let lines = break_text_lines("A\n\nB", 100.0, &fixed_advance);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].iter().collect::<String>(), "A");
        assert!(lines[1].is_empty());
        assert_eq!(lines[2].iter().collect::<String>(), "B");
    }

    // ── Container layout tests (§14.2) ──────────────────────────────

    #[test]
    fn column_padding_gap_affects_child_tracks() {
        let mut child1 = node();
        child1.node_id = UiNodeId("a".into());
        child1.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
        };
        child1.enter_transition = None;
        let mut child2 = node();
        child2.node_id = UiNodeId("b".into());
        child2.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 15.0,
        };
        child2.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("col".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Column,
            padding: [4.0, 6.0, 4.0, 6.0], // top, right, bottom, left
            gap: 3.0,
            ..UiLayout::default()
        });
        root.children = vec![child1, child2];
        root.enter_transition = None;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("col-pad-gap".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::new();
        fragments.insert(UiFragmentId("col-pad-gap".into()), fragment);
        let (device, _queue) = test_device("neon3-col-pad-gap");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([256, 256], [256.0, 256.0]);
        renderer.refresh_plan(&fragments, [256.0, 256.0]);
        // Child A: y = padding_top = 4
        // Child B: y = padding_top + child_a_height + gap = 4 + 10 + 3 = 17
        let a = renderer.plan.iter().find(|n| n.id.ends_with("/a")).unwrap();
        let b = renderer.plan.iter().find(|n| n.id.ends_with("/b")).unwrap();
        assert!(
            (a.target.logical_bounds.y - 4.0).abs() < 0.001,
            "child A y should be padding_top"
        );
        assert!(
            (b.target.logical_bounds.y - 17.0).abs() < 0.001,
            "child B y should include padding + height + gap"
        );
    }

    #[test]
    fn flow_children_with_authored_offsets_are_absolute_and_do_not_consume_tracks() {
        let child = |id: &str, x: f32, y: f32, height: f32| UiNode {
            node_id: UiNodeId(id.into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds { x, y, width: 40.0, height },
            layout: Some(UiLayout::default()),
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            children: Vec::new(),
        };
        let root = UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 200.0, height: 160.0 },
            layout: Some(UiLayout { mode: UiLayoutMode::Column, gap: 6.0, padding: [4.0, 4.0, 4.0, 4.0], ..UiLayout::default() }),
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            children: vec![child("first", 0.0, 0.0, 20.0), child("overlay", 60.0, 48.0, 30.0), child("second", 0.0, 0.0, 20.0)],
        };
        let bounds = resolve_children(
            &root,
            root.bounds,
            root.layout.unwrap(),
            UiBounds { x: 4.0, y: 4.0, width: 192.0, height: 152.0 },
            None,
        );
        assert_eq!(bounds[0].y, 4.0);
        assert_eq!(bounds[1].x, 64.0);
        assert_eq!(bounds[1].y, 52.0);
        assert_eq!(bounds[2].y, 30.0);
    }

    #[test]
    fn absolute_children_use_parent_content_origin() {
        let mut child = node();
        child.node_id = UiNodeId("child".into());
        child.bounds = UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 10.0 };
        child.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("root".into());
        root.bounds = UiBounds { x: 100.0, y: 50.0, width: 200.0, height: 160.0 };
        root.layout = Some(UiLayout { padding: [11.0, 13.0, 17.0, 19.0], ..UiLayout::default() });
        root.children = vec![child];
        let bounds = resolve_children(
            &root,
            root.bounds,
            root.layout.unwrap(),
            UiBounds { x: 119.0, y: 61.0, width: 168.0, height: 132.0 },
            None,
        );
        assert_eq!(bounds[0].x, 119.0);
        assert_eq!(bounds[0].y, 61.0);
    }

    #[test]
    fn row_padding_gap_affects_child_tracks() {
        let mut child1 = node();
        child1.node_id = UiNodeId("a".into());
        child1.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
        };
        child1.enter_transition = None;
        let mut child2 = node();
        child2.node_id = UiNodeId("b".into());
        child2.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 15.0,
        };
        child2.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("row".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Row,
            padding: [2.0, 4.0, 2.0, 4.0], // top, right, bottom, left
            gap: 5.0,
            ..UiLayout::default()
        });
        root.children = vec![child1, child2];
        root.enter_transition = None;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("row-pad-gap".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::new();
        fragments.insert(UiFragmentId("row-pad-gap".into()), fragment);
        let (device, _queue) = test_device("neon3-row-pad-gap");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([256, 256], [256.0, 256.0]);
        renderer.refresh_plan(&fragments, [256.0, 256.0]);
        // Child A: x = padding_left = 4
        // Child B: x = 4 + child_a_width + gap = 4 + 20 + 5 = 29
        let a = renderer.plan.iter().find(|n| n.id.ends_with("/a")).unwrap();
        let b = renderer.plan.iter().find(|n| n.id.ends_with("/b")).unwrap();
        assert!((a.target.logical_bounds.x - 4.0).abs() < 0.001);
        assert!((b.target.logical_bounds.x - 29.0).abs() < 0.001);
    }

    #[test]
    fn invisible_child_does_not_occupy_track_space() {
        let mut child1 = node();
        child1.node_id = UiNodeId("visible".into());
        child1.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
        };
        child1.enter_transition = None;
        let mut child2 = node();
        child2.node_id = UiNodeId("hidden".into());
        child2.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 10.0,
        };
        child2.visible = false;
        child2.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("col".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Column,
            gap: 2.0,
            ..UiLayout::default()
        });
        root.children = vec![child1, child2];
        root.enter_transition = None;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("branch-space".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::new();
        fragments.insert(UiFragmentId("branch-space".into()), fragment);
        let (device, _queue) = test_device("neon3-invisible-child");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([256, 256], [256.0, 256.0]);
        renderer.refresh_plan(&fragments, [256.0, 256.0]);
        // Only the visible child should appear in the plan
        assert!(renderer.plan.iter().any(|n| n.id.ends_with("/visible")));
        assert!(!renderer.plan.iter().any(|n| n.id.ends_with("/hidden")));
    }

    #[test]
    fn explicit_height_does_not_clip_text_content() {
        let (device, _queue) = test_device("neon3-text-no-clip");
        let mut text_node = node();
        text_node.node_id = UiNodeId("label".into());
        text_node.kind = UiNodeKind::Label;
        text_node.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 8.0,
        };
        text_node.text = Some(TextRef::Literal {
            value: "Hello World".into(),
        });
        text_node.enter_transition = None;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("text-no-clip".into()),
            revision: Revision(1),
            root: text_node,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::new();
        fragments.insert(UiFragmentId("text-no-clip".into()), fragment);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([256, 256], [256.0, 256.0]);
        renderer.refresh_plan(&fragments, [256.0, 256.0]);
        let label = renderer
            .plan
            .iter()
            .find(|n| n.id.ends_with("/label"))
            .unwrap();
        // §4.1: explicit height is a minimum guarantee, not a clip ceiling.
        // The label's resolved height must be >= 1 line height (~16-20px) even
        // though the declared height is only 8.
        assert!(
            label.target.logical_bounds.height >= 16.0,
            "explicit height 8 must not clip text; resolved height = {}",
            label.target.logical_bounds.height,
        );
    }

    #[test]
    fn margin_does_not_double_count_in_parent_track() {
        let mut child = node();
        child.node_id = UiNodeId("child".into());
        child.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 10.0,
        };
        child.layout = Some(UiLayout {
            margin: [2.0, 0.0, 0.0, 0.0], // top margin only
            ..UiLayout::default()
        });
        child.enter_transition = None;
        let mut root = node();
        root.node_id = UiNodeId("col".into());
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
        root.layout = Some(UiLayout {
            mode: UiLayoutMode::Column,
            ..UiLayout::default()
        });
        root.children = vec![child];
        root.enter_transition = None;
        let fragment = UiFragment {
            fragment_id: UiFragmentId("margin".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        };
        let mut fragments = HashMap::new();
        fragments.insert(UiFragmentId("margin".into()), fragment);
        let (device, _queue) = test_device("neon3-margin");
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.update_viewport([256, 256], [256.0, 256.0]);
        renderer.refresh_plan(&fragments, [256.0, 256.0]);
        let child_visual = renderer
            .plan
            .iter()
            .find(|n| n.id.ends_with("/child"))
            .unwrap();
        // Child y should be offset by margin top (2px), but the child's own
        // height should be 10 (not 12 = 10 + 2*1). Margin is not added to
        // the child's own bounds — it's applied to the parent's track.
        assert!(
            (child_visual.target.logical_bounds.y - 2.0).abs() < 0.001,
            "child y should be margin top (2), got {}",
            child_visual.target.logical_bounds.y
        );
        assert!(
            (child_visual.target.logical_bounds.height - 10.0).abs() < 0.001,
            "child height should be 10 (margin not added to bounds), got {}",
            child_visual.target.logical_bounds.height
        );
    }

    #[test]
    fn external_image_binding_uploads_and_renders_without_asset_ref() {
        let _gpu_test = GPU_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (device, queue) = test_device("neon3-external-image");
        let source = UiImageSource {
            image_id: "external-test-image".into(),
            media_type: "application/x-neon-rgba8".into(),
            width: 2,
            height: 2,
            bytes: vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
        };
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let texture = renderer
            .preload_external_image(&device, &queue, &source)
            .expect("external image must upload");
        assert_eq!(texture.texture_index, 0);
        assert_eq!(texture.region.width, 2);
        assert_eq!(texture.region.height, 2);
        let mut root = node();
        root.node_id = UiNodeId("external-image".into());
        root.kind = UiNodeKind::Image;
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
        };
        root.image = None;
        root.enter_transition = None;
        let fragment_id = UiFragmentId("external-image-render".into());
        let fragments = HashMap::from([(
            fragment_id.clone(),
            UiFragment {
                fragment_id,
                revision: Revision(1),
                root,
                effects: vec![UiEffect::ImageBinding {
                    node_id: UiNodeId("external-image".into()),
                    image_id: "external-test-image".into(),
                }],
            },
        )]);
        let pixels = render_renderer_offscreen_for_test(
            &mut renderer,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [16, 16],
            0.0,
        );
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0 && pixel[3] > 0)
        );
    }
}
