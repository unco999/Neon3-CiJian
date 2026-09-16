//! Renderer-local code editor presentation.
//!
//! Architecture (user-decided): the editor is a fully independent component
//! whose editing semantics live in the ui-runtime (`neon-ui-runtime`'s
//! `editor_component`, which owns the `neon-editor` core: buffer, highlight,
//! completion, undo/redo). This renderer no longer holds any editing state —
//! it consumes `UiEffect::CodeEditorPresentation` snapshots riding the
//! UiFragment, projects them into glyph/rect instances with the resident
//! font, and forwards input events (`UiEditorInputEvent`) to the ui-runtime
//! component through the injected sink. Pointer positions are mapped to
//! (line, column) here with the resident font; the editing semantics that
//! consume them run in the ui-runtime.

use std::collections::HashMap;

use neon_ui_schema::{
    UiCodeEditorDeclaration, UiCodeEditorPresentation, UiEditorCompletionItem,
    UiEditorEditFx, UiEditorInputEvent, UiEditorKeyKind, UiNodeKind,
};
use winit::keyboard::{Key, NamedKey};

use super::editor_theme::{EditorTheme, editor_theme_from};
use super::{
    ResidentFont, UiBounds, UiFragment, UiTextInstance, color_pass_depth, contains,
    ensure_glyph, overlay_instance,
};

/// How many completion items the popup shows before scrolling internally.
const COMPLETION_VISIBLE_ITEMS: usize = 8;
/// Horizontal padding inside the completion popup (logical px).
const COMPLETION_PAD: f32 = 8.0;
/// Blink half-period for the caret (seconds).
const CARET_BLINK_SECONDS: f32 = 0.6;
/// Keep the caret solid right after an edit.
const CARET_SOLID_AFTER_EDIT_SECONDS: f32 = 0.5;

/// One queued editor commit for the UI host. Carries the stable node path,
/// the declared `event` action and the full current document text.
#[derive(Clone, Debug)]
pub struct EditorCommit {
    pub node_path: String,
    pub event_action: Option<String>,
    pub document: String,
}

/// Open completion popup state (snapshotted presentation data).
#[derive(Clone, Debug)]
pub(super) struct EditorCompletionState {
    pub items: Vec<UiEditorCompletionItem>,
    pub selected: usize,
}

/// Editor-local caret/selection position (the ui-runtime owns the real
/// semantic position; this mirrors the presentation snapshot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct EditorPosition {
    pub line: u32,
    pub column: u32,
}

impl EditorPosition {
    pub const START: Self = Self { line: 0, column: 0 };
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

/// One token span mirror: char offset, char length and the stable class name.
#[derive(Clone, Debug)]
pub(super) struct SpanRef {
    pub start: u32,
    pub len: u32,
    pub class: String,
}

/// Renderer-local mirror of one code-editor presentation. No editing state
/// lives here; everything is rebuilt from `UiCodeEditorPresentation` when its
/// revision changes.
pub(super) struct EditorRuntimeState {
    pub declaration: UiCodeEditorDeclaration,
    /// Source text split into lines (cached from the presentation source).
    pub lines: Vec<String>,
    /// Per-row token spans (start / len / class) expanded from the
    /// presentation token rows.
    pub token_spans: Vec<Vec<SpanRef>>,
    pub caret: EditorPosition,
    pub selection_anchor: Option<EditorPosition>,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub focus: bool,
    pub preedit: String,
    pub completion: Option<EditorCompletionState>,
    pub edit_fx: Vec<UiEditorEditFx>,
    pub font_scale: f32,
    pub last_edit_seconds: f32,
    /// Set when a new presentation arrives so the plan-reuse early return
    /// cannot skip the layout pass; cleared at the end of layout_editors.
    pub layout_dirty: bool,
    /// Revision of the presentation this mirror was built from (the
    /// ui-runtime bumps it on every semantic change).
    pub presentation_revision: u64,
    /// Full rows region laid out by the renderer (x/y/width/height in the
    /// same logical space as the node bounds). The lowered panel bounds may
    /// not grow with content, so pointer hit-testing falls back to this.
    pub content_rect: UiBounds,
}

impl EditorRuntimeState {
    fn from_presentation(
        declaration: UiCodeEditorDeclaration,
        presentation: UiCodeEditorPresentation,
    ) -> Self {
        let lines: Vec<String> = presentation.source.split('\n').map(str::to_string).collect();
        let mut token_spans: Vec<Vec<SpanRef>> = Vec::with_capacity(presentation.token_rows.len());
        for row in &presentation.token_rows {
            let spans: Vec<SpanRef> = row
                .iter()
                .map(|span| SpanRef {
                    start: span.start,
                    len: span.text.chars().count() as u32,
                    class: span.class.clone(),
                })
                .collect();
            token_spans.push(spans);
        }
        let selection_anchor = match (
            presentation.selection_anchor_line,
            presentation.selection_anchor_column,
        ) {
            (Some(line), Some(column)) => Some(EditorPosition::new(line, column)),
            _ => None,
        };
        let completion = presentation.completion.map(|c| EditorCompletionState {
            items: c.items,
            selected: c.selected as usize,
        });
        Self {
            declaration,
            lines,
            token_spans,
            caret: EditorPosition::new(presentation.caret_line, presentation.caret_column),
            selection_anchor,
            scroll_x: presentation.scroll_x,
            scroll_y: presentation.scroll_y,
            focus: presentation.focus,
            preedit: presentation.preedit,
            completion,
            edit_fx: presentation.edit_fx,
            font_scale: presentation.font_scale,
            last_edit_seconds: presentation.last_edit_seconds,
            layout_dirty: true,
            presentation_revision: presentation.revision,
            content_rect: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
        }
    }
}

/// Everything `layout_editors` produced for one frame. The four lists are
/// merged into the renderer's normal instance/text passes by the caller, so
/// editor chrome participates in the same clipping and paint-group ordering
/// as every other unified-UI visual.
#[derive(Default)]
pub(super) struct EditorLayoutOutput {
    /// Code text and line-number glyph instances (drawn in the text pass,
    /// beneath the caret but above the panel fill).
    pub editor_texts: Vec<UiTextInstance>,
    /// Completion candidate labels (drawn in the top-layer popup text pass).
    pub editor_popup_texts: Vec<UiTextInstance>,
    /// Current-line highlight + selection rectangles (drawn beneath glyphs).
    pub editor_rects: Vec<super::UiInstance>,
    /// Caret + completion popup background/selected-item highlight (drawn
    /// above glyphs, in the popup instance pass).
    pub editor_popup_rects: Vec<super::UiInstance>,
    /// Code-glyph instances routed to the per-package text-material pass when
    /// the code_editor node declares a `text_material` or a token class has a
    /// dedicated material, plus transient edit fx batches. Each entry is
    /// `(package_id, instances)`; glyph rects/clips are pre-expanded by the
    /// material overflow. Line numbers stay in `editor_texts`.
    pub editor_text_materials: Vec<(String, Vec<UiTextInstance>)>,
}

/// Raster/metrics pixel size for an editor: glyphs are rasterized 1:1 at the
/// declared font size so small sizes render crisp (no fractional downscale).
fn editor_px(declaration: &UiCodeEditorDeclaration, font_scale: f32) -> f32 {
    declaration.font_size as f32 * font_scale
}

/// Line metrics at the editor's declared size.
fn editor_line_metrics(
    font: &ResidentFont,
    declaration: &UiCodeEditorDeclaration,
    font_scale: f32,
) -> fontdue::LineMetrics {
    font.font
        .horizontal_line_metrics(editor_px(declaration, font_scale))
        .unwrap_or(fontdue::LineMetrics {
            ascent: font.ascent,
            descent: font.ascent - font.line_height,
            line_gap: 0.0,
            new_line_size: font.line_height,
        })
}

fn editor_row_height(
    font: &ResidentFont,
    declaration: &UiCodeEditorDeclaration,
    font_scale: f32,
) -> f32 {
    // fontdue::Layout also rounds the line advance up (ceil), keeping every
    // row boundary on an integer pixel so baselines stay pixel-aligned.
    editor_line_metrics(font, declaration, font_scale)
        .new_line_size
        .ceil()
}

fn editor_gutter_width(
    declaration: &UiCodeEditorDeclaration,
    line_count: u32,
    font_scale: f32,
) -> f32 {
    if !declaration.line_numbers {
        return 6.0;
    }
    let px = editor_px(declaration, font_scale);
    let digits = line_count.max(1).to_string().len() as f32;
    8.0 + digits * (px * 0.5) + 14.0
}

/// Advance of one character at `px` (non-mutating; falls back to font
/// metrics so measurement never forces rasterization of the character).
fn char_advance(font: &ResidentFont, ch: char, px: f32) -> f32 {
    font.glyphs
        .get(&(ch, px.round().max(1.0) as u32))
        .map_or_else(
            || font.font.metrics(ch, px).advance_width,
            |glyph| glyph.advance,
        )
}

/// Advance of the first `column` characters of `line_text` at `px`.
fn line_prefix_advance(font: &ResidentFont, line_text: &str, column: u32, px: f32) -> f32 {
    line_text
        .chars()
        .take(column as usize)
        .map(|ch| char_advance(font, ch, px))
        .sum()
}

/// Full advance of `line_text` at `px`.
fn line_full_advance(font: &ResidentFont, line_text: &str, px: f32) -> f32 {
    line_text.chars().map(|ch| char_advance(font, ch, px)).sum()
}

/// Pointer x (relative to content origin, pre-scroll) to char column.
fn column_from_x(font: &ResidentFont, line_text: &str, x: f32, px: f32) -> u32 {
    let mut acc = 0.0_f32;
    let mut column = 0_u32;
    for ch in line_text.chars() {
        let advance = char_advance(font, ch, px);
        if acc + advance * 0.5 >= x {
            break;
        }
        acc += advance;
        column += 1;
    }
    column
}

/// Rebuild a color with a multiplied alpha (theme colors carry their own
/// alpha; glyph opacity from the visual style is folded in here).
fn rgba(color: [f32; 4], a: f32) -> [f32; 4] {
    [color[0], color[1], color[2], color[3] * a]
}

fn completion_kind_color(kind: &str, theme: &EditorTheme) -> [f32; 4] {
    match kind {
        "Keyword" => theme.token("Keyword", 1.0),
        "NodeKind" => theme.token("NodeKind", 1.0),
        "Attribute" => theme.token("Attribute", 1.0),
        "Input" | "InputKind" => theme.token("InputRef", 1.0),
        // LSP-sourced values (TS/Rust/C++): neutral identifier color.
        "Value" => theme.token("Ident", 1.0),
        _ => theme.token("Ident", 1.0),
    }
}

fn kind_prefix(kind: &str) -> &'static str {
    match kind {
        "Keyword" => "kw",
        "NodeKind" => "nd",
        "Attribute" => "at",
        "Input" => "in",
        "InputKind" => "ik",
        "Value" => "vl",
        _ => "?",
    }
}

/// Ordered selection endpoints from `anchor` + `caret`.
fn ordered_selection(anchor: EditorPosition, caret: EditorPosition) -> (EditorPosition, EditorPosition) {
    if anchor <= caret {
        (anchor, caret)
    } else {
        (caret, anchor)
    }
}

fn selected_range_for_row(
    anchor: EditorPosition,
    caret: EditorPosition,
    row: u32,
    line_char_len: u32,
) -> Option<(u32, u32)> {
    let (start, end) = ordered_selection(anchor, caret);
    if row < start.line || row > end.line {
        return None;
    }
    let from = if row == start.line { start.column } else { 0 };
    let to = if row == end.line {
        end.column
    } else {
        line_char_len
    };
    if to <= from {
        return None;
    }
    Some((from, to))
}

/// Line text of a mirrored editor row.
fn line_of(state: &EditorRuntimeState, row: u32) -> &str {
    state
        .lines
        .get(row as usize)
        .map(String::as_str)
        .unwrap_or_default()
}

/// Token class name at (row, char column), from the mirrored spans.
fn class_at(state: &EditorRuntimeState, row: u32, column: u32) -> Option<&str> {
    let spans = state.token_spans.get(row as usize)?;
    spans
        .iter()
        .find(|span| column >= span.start && column < span.start + span.len)
        .map(|span| span.class.as_str())
}

impl super::UiWgpuRenderer {
    /// Layouts every code editor in the current plan into glyph + rect
    /// instances. Reads presentation mirrors only; editing semantics run in
    /// the ui-runtime, never here, so a draw pass cannot alter state.
    pub(super) fn layout_editors(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        time_seconds: f32,
    ) -> EditorLayoutOutput {
        let mut output = EditorLayoutOutput::default();
        let Some(font) = self.resident_font.as_mut() else {
            return output;
        };
        let editors = &mut self.editors;
        let sampled = &self.sampled;
        let plan = &self.plan;
        let plan_index = &self.plan_index;
        let viewport_logical_size = self.viewport_logical_size;
        let paths: Vec<String> = editors.keys().cloned().collect();
        for path in &paths {
            let Some(index) = plan_index.get(path).copied() else {
                continue;
            };
            let visual = &sampled[index];
            // Code editors are screen-UI presentation; projected world panels
            // are not yet supported for editor chrome.
            if visual.world_depth.is_some() {
                continue;
            }
            // The lowered node keeps its Panel kind; only an editor mirror
            // makes it an editor. Exiting/removed nodes are skipped.
            if plan[index].instance_index.is_none() {
                continue;
            }
            let state = editors.get_mut(path).expect("path from editors keys");
            let declaration = &state.declaration;
            let theme = editor_theme_from(declaration);
            // Whole-editor text material: the code_editor node declares a
            // `text_material` clause like any text node, the effect lands in
            // node_text_materials under the full fragment/node path, and this
            // layout routes every code glyph through the package pass while
            // keeping the line-number gutter in the ordinary text pass.
            let editor_material = self.node_text_materials.get(path).cloned();
            let mut editor_material_instances: Vec<UiTextInstance> = Vec::new();
            // Per-token-class shader routing: each entry is (package_id,
            // instances). Instances are pre-expanded by the declaring class
            // material's overflow, mirroring the whole-node text material.
            let mut token_material_batches: Vec<(String, Vec<UiTextInstance>)> = Vec::new();
            // Transient edit fx (type-in / delete fragment) batches by
            // package; the ui-runtime owns fx lifecycle, the renderer skips
            // expired entries defensively.
            let mut fx_batches: HashMap<String, Vec<UiTextInstance>> = HashMap::new();
            let raster_px = editor_px(declaration, state.font_scale);
            let row_height = editor_row_height(font, declaration, state.font_scale);
            if row_height <= 0.0 || visual.bounds.width <= 0.0 || visual.bounds.height <= 0.0 {
                continue;
            }
            let line_count = state.lines.len() as u32;
            let gutter_width = editor_gutter_width(declaration, line_count, state.font_scale);
            state.content_rect = UiBounds {
                x: visual.bounds.x,
                y: visual.bounds.y - state.scroll_y,
                width: visual.bounds.width,
                height: (line_count as f32 * row_height).max(visual.bounds.height),
            };
            let content_x = visual.bounds.x + gutter_width;
            let content_width = (visual.bounds.width - gutter_width).max(1.0);
            let clip = [
                visual.clip.x,
                visual.clip.y,
                visual.clip.x + visual.clip.width,
                visual.clip.y + visual.clip.height,
            ];
            let opacity = visual.style.opacity;
            let depth = color_pass_depth(visual.world_depth);
            let paint_group_id = visual.paint_group_id;
            let base_track = [0.0_f32; 4];

            let first_row = (state.scroll_y / row_height).floor().max(0.0) as u32;
            let visible_rows = (visual.bounds.height / row_height).ceil() as u32 + 1;
            let last_row = first_row.saturating_add(visible_rows).min(line_count);

            // 1) Current-line highlight (focused editors only).
            if state.focus {
                let row_y =
                    visual.bounds.y + state.caret.line as f32 * row_height - state.scroll_y;
                if row_y + row_height >= visual.bounds.y
                    && row_y <= visual.bounds.y + visual.bounds.height
                {
                    output.editor_rects.push(overlay_instance(
                        UiBounds {
                            x: visual.bounds.x,
                            y: row_y,
                            width: visual.bounds.width,
                            height: row_height,
                        },
                        visual.clip,
                        theme.current_line,
                    ));
                }
            }

            // 2) Selection rectangles.
            if let Some(anchor) = state.selection_anchor {
                for row in first_row..last_row {
                    let line_text = line_of(state, row);
                    let Some((from, to)) = selected_range_for_row(
                        anchor,
                        state.caret,
                        row,
                        line_text.chars().count() as u32,
                    ) else {
                        continue;
                    };
                    let row_y = visual.bounds.y + row as f32 * row_height - state.scroll_y;
                    if row_y + row_height < visual.bounds.y
                        || row_y > visual.bounds.y + visual.bounds.height
                    {
                        continue;
                    }
                    let x_from =
                        content_x + line_prefix_advance(font, line_text, from, raster_px)
                            - state.scroll_x;
                    let x_to = content_x + line_prefix_advance(font, line_text, to, raster_px)
                        - state.scroll_x;
                    let rect_x = x_from.min(x_to);
                    let rect_width = (x_to - x_from).abs().max(1.0);
                    output.editor_rects.push(overlay_instance(
                        UiBounds {
                            x: rect_x,
                            y: row_y,
                            width: rect_width,
                            height: row_height,
                        },
                        visual.clip,
                        theme.selection,
                    ));
                }
            }

            // 3) Visible rows: line numbers + token-colored text.
            for row in first_row..last_row {
                let line_text = line_of(state, row);
                let row_y = visual.bounds.y + row as f32 * row_height - state.scroll_y;
                if row_y + row_height < visual.bounds.y
                    || row_y > visual.bounds.y + visual.bounds.height
                {
                    continue;
                }
                let baseline =
                    (row_y + editor_line_metrics(font, declaration, state.font_scale).ascent)
                        .floor();

                // Line-number gutter.
                if declaration.line_numbers {
                    let number_text = (row + 1).to_string();
                    let number_width = line_full_advance(font, &number_text, raster_px);
                    let mut x = visual.bounds.x + (gutter_width - 10.0) - number_width;
                    let is_current = state.focus && row == state.caret.line;
                    let color = if is_current {
                        rgba(theme.line_number_current, opacity)
                    } else {
                        rgba(theme.line_number, opacity * 0.9)
                    };
                    for ch in number_text.chars() {
                        let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
                            continue;
                        };
                        output.editor_texts.push(UiTextInstance {
                            rect: [
                                (x + glyph.xmin).floor(),
                                baseline + glyph.plane_min_y.floor(),
                                glyph.width,
                                glyph.height,
                            ],
                            color,
                            clip,
                            uv: glyph.uv,
                            depth,
                            paint_group_id,
                            animation: base_track,
                            transform_from: [0.0, 0.0, 1.0, 1.0],
                            transform_to: [0.0, 0.0, 1.0, 1.0],
                            rotation_pivot: [0.0; 4],
                            overflow: [0.0; 4],
                        });
                        x += glyph.advance;
                    }
                }

                // Token-colored code text. Whitespace between spans renders in
                // the default color so proportional fonts keep spacing.
                let mut column = 0u32;
                let mut x = content_x - state.scroll_x;
                for ch in line_text.chars() {
                    let color = class_at(state, row, column)
                        .map_or_else(
                            || rgba(theme.text, opacity),
                            |class| theme.token(class, opacity),
                        );
                    let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
                        continue;
                    };
                    // Transient edit fx: inserted glyphs in the fx column
                    // range ride the type-in package; a delete fx snapshots
                    // its ghost at the pre-delete estimate position.
                    // The mirror can be rebuilt by a co-drawn render pass
                    // (behind-ui / world-lab) between presentations, which
                    // would drop a just-published fx; re-read the external
                    // presentations slot directly so transient effects never
                    // vanish from the layout pass.
                    let fx_owned;
                    let fx_source = if !state.edit_fx.is_empty() {
                        &state.edit_fx
                    } else if let Some(slot) = &self.editor_external_presentations
                        && let Ok(external) = slot.lock()
                    {
                        fx_owned = external
                            .iter()
                            .filter(|p| p.node_key == state.declaration.node_key)
                            .flat_map(|p| p.edit_fx.iter().cloned())
                            .collect::<Vec<_>>();
                        &fx_owned
                    } else {
                        &state.edit_fx
                    };

                    for fx in fx_source {
                        let alive =
                            time_seconds - fx.started_seconds < fx.duration_ms as f32 / 1000.0;
                        if !alive {
                            continue;
                        }
                        let fx_len = fx.text.chars().count() as u32;
                        if fx.kind == "insert"
                            && fx.row == row
                            && column >= fx.col
                            && column < fx.col + fx_len
                        {
                            let mut inst = UiTextInstance {
                                rect: [
                                    (x + glyph.xmin).floor(),
                                    baseline + glyph.plane_min_y.floor(),
                                    glyph.width,
                                    glyph.height,
                                ],
                                color,
                                clip,
                                uv: glyph.uv,
                                depth,
                                paint_group_id,
                                animation: base_track,
                                transform_from: [0.0, 0.0, 1.0, 1.0],
                                transform_to: [0.0, 0.0, 1.0, 1.0],
                                rotation_pivot: [0.0; 4],
                                overflow: [0.0; 4],
                            };
                            inst.rect[0] -= 6.0;
                            inst.rect[1] -= 6.0;
                            inst.rect[2] += 12.0;
                            inst.rect[3] += 12.0;
                            inst.clip[0] -= 6.0;
                            inst.clip[1] -= 6.0;
                            inst.clip[2] += 12.0;
                            inst.clip[3] += 12.0;
                            inst.overflow = [6.0, 6.0, 6.0, 6.0];
                            fx_batches
                                .entry(fx.package_id.clone())
                                .or_default()
                                .push(inst);
                        } else if fx.kind == "delete" && fx.row == row && column == fx.col {
                            let mut ghost = UiTextInstance {
                                rect: [
                                    (x + glyph.xmin).floor(),
                                    baseline + glyph.plane_min_y.floor(),
                                    glyph.width,
                                    glyph.height,
                                ],
                                color,
                                clip,
                                uv: glyph.uv,
                                depth,
                                paint_group_id,
                                animation: base_track,
                                transform_from: [0.0, 0.0, 1.0, 1.0],
                                transform_to: [0.0, 0.0, 1.0, 1.0],
                                rotation_pivot: [0.0; 4],
                                overflow: [0.0; 4],
                            };
                            ghost.rect[0] -= 8.0;
                            ghost.rect[1] -= 8.0;
                            ghost.rect[2] += 16.0;
                            ghost.rect[3] += 16.0;
                            ghost.clip[0] -= 8.0;
                            ghost.clip[1] -= 8.0;
                            ghost.clip[2] += 16.0;
                            ghost.clip[3] += 16.0;
                            ghost.overflow = [8.0, 8.0, 8.0, 8.0];
                            fx_batches
                                .entry(fx.package_id.clone())
                                .or_default()
                                .push(ghost);
                        }
                    }
                    let mut instance = UiTextInstance {
                        rect: [
                            (x + glyph.xmin).floor(),
                            baseline + glyph.plane_min_y.floor(),
                            glyph.width,
                            glyph.height,
                        ],
                        color,
                        clip,
                        uv: glyph.uv,
                        depth,
                        paint_group_id,
                        animation: base_track,
                        transform_from: [0.0, 0.0, 1.0, 1.0],
                        transform_to: [0.0, 0.0, 1.0, 1.0],
                        rotation_pivot: [0.0; 4],
                        overflow: [0.0; 4],
                    };
                    // Token-class shader takes precedence; the whole-node
                    // text material is the fallback for classes without a
                    // dedicated entry. Selected spans (if the editor declares
                    // `selection_shader`) get the glow material in preference
                    // to the class shader.
                    let class_material = {
                        let base = class_at(state, row, column)
                            .and_then(|class| declaration.token_materials.get(class));
                        let in_selection = match state.selection_anchor {
                            Some(anchor) => {
                                let (s, e) = ordered_selection(anchor, state.caret);
                                row >= s.line
                                    && row <= e.line
                                    && column >= if row == s.line { s.column } else { 0 }
                                    && column < if row == e.line { e.column } else { u32::MAX }
                            }
                            None => false,
                        };
                        if in_selection {
                            state
                                .declaration
                                .selection_material
                                .as_ref()
                                .or(base)
                        } else {
                            base
                        }
                    };
                    if let Some(text_material) = class_material {
                        instance.rect[0] -= text_material.overflow[0];
                        instance.rect[1] -= text_material.overflow[1];
                        instance.rect[2] +=
                            text_material.overflow[0] + text_material.overflow[2];
                        instance.rect[3] +=
                            text_material.overflow[1] + text_material.overflow[3];
                        instance.clip[0] -= text_material.overflow[0];
                        instance.clip[1] -= text_material.overflow[1];
                        instance.clip[2] +=
                            text_material.overflow[0] + text_material.overflow[2];
                        instance.clip[3] +=
                            text_material.overflow[1] + text_material.overflow[3];
                        instance.overflow = text_material.overflow;
                        match token_material_batches
                            .iter_mut()
                            .find(|(package, _)| *package == text_material.package_id)
                        {
                            Some((_, instances)) => instances.push(instance),
                            None => token_material_batches.push((
                                text_material.package_id.clone(),
                                vec![instance],
                            )),
                        }
                    } else if let Some(text_material) = &editor_material {
                        instance.rect[0] -= text_material.overflow[0];
                        instance.rect[1] -= text_material.overflow[1];
                        instance.rect[2] +=
                            text_material.overflow[0] + text_material.overflow[2];
                        instance.rect[3] +=
                            text_material.overflow[1] + text_material.overflow[3];
                        instance.clip[0] -= text_material.overflow[0];
                        instance.clip[1] -= text_material.overflow[1];
                        instance.clip[2] +=
                            text_material.overflow[0] + text_material.overflow[2];
                        instance.clip[3] +=
                            text_material.overflow[1] + text_material.overflow[3];
                        instance.overflow = text_material.overflow;
                        editor_material_instances.push(instance);
                    } else {
                        output.editor_texts.push(instance);
                    }
                    x += glyph.advance;
                    column += 1;
                }

                // IME preedit text renders at the caret position with a dim
                // color.
                if state.focus && row == state.caret.line && !state.preedit.is_empty() {
                    let preedit_x = content_x
                        + line_prefix_advance(font, line_text, state.caret.column, raster_px)
                        - state.scroll_x;
                    let mut px = preedit_x;
                    for ch in state.preedit.chars() {
                        let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
                            continue;
                        };
                        output.editor_texts.push(UiTextInstance {
                            rect: [
                                (px + glyph.xmin).floor(),
                                baseline + glyph.plane_min_y.floor(),
                                glyph.width,
                                glyph.height,
                            ],
                            color: [0.62, 0.72, 0.90, opacity],
                            clip,
                            uv: glyph.uv,
                            depth,
                            paint_group_id,
                            animation: base_track,
                            transform_from: [0.0, 0.0, 1.0, 1.0],
                            transform_to: [0.0, 0.0, 1.0, 1.0],
                            rotation_pivot: [0.0; 4],
                            overflow: [0.0; 4],
                        });
                        px += glyph.advance;
                    }
                }
            }

            // Flush transient fx batches, then the token-class batches and
            // the whole-node material (line numbers are intentionally
            // excluded above; only token-colored code glyphs route to
            // shaders).
            output
                .editor_text_materials
                .extend(fx_batches.into_iter());
            output
                .editor_text_materials
                .append(&mut token_material_batches);
            if !editor_material_instances.is_empty()
                && let Some(text_material) = &editor_material
            {
                output.editor_text_materials.push((
                    text_material.package_id.clone(),
                    editor_material_instances,
                ));
            }

            // 4) Caret (focused editors; blink unless recently edited or a
            //    completion popup is open).
            if state.focus {
                let just_edited =
                    time_seconds - state.last_edit_seconds < CARET_SOLID_AFTER_EDIT_SECONDS;
                let visible = state.completion.is_some()
                    || just_edited
                    || (time_seconds / CARET_BLINK_SECONDS).fract() < 0.5;
                if visible {
                    let caret_line = line_of(state, state.caret.line);
                    let caret_x = content_x
                        + line_prefix_advance(font, caret_line, state.caret.column, raster_px)
                        - state.scroll_x;
                    let caret_y = visual.bounds.y + state.caret.line as f32 * row_height
                        - state.scroll_y
                        + 1.0;
                    output.editor_popup_rects.push(overlay_instance(
                        UiBounds {
                            x: caret_x,
                            y: caret_y,
                            width: 2.0,
                            height: (row_height - 2.0).max(2.0),
                        },
                        visual.clip,
                        theme.caret,
                    ));
                }
            }

            // 5) Completion popup (top layer).
            if let Some(completion) = &state.completion {
                if !completion.items.is_empty() {
                    let caret_line = line_of(state, state.caret.line);
                    let caret_x = content_x
                        + line_prefix_advance(font, caret_line, state.caret.column, raster_px)
                        - state.scroll_x;
                    let caret_y = visual.bounds.y + state.caret.line as f32 * row_height
                        - state.scroll_y
                        + row_height;
                    let mut popup_width = 0.0_f32;
                    for item in &completion.items {
                        let label_width = line_full_advance(font, &item.label, raster_px)
                            + line_full_advance(font, &item.detail, raster_px) * 0.8;
                        popup_width = popup_width.max(label_width);
                    }
                    popup_width = (popup_width + COMPLETION_PAD * 2.0 + 14.0)
                        .clamp(120.0, content_width - 8.0);
                    let item_height = row_height * 0.92;
                    let popup_height = item_height
                        * completion.items.len().min(COMPLETION_VISIBLE_ITEMS) as f32
                        + COMPLETION_PAD;
                    let popup_x = caret_x.clamp(
                        visual.bounds.x + 2.0,
                        (visual.bounds.x + visual.bounds.width - popup_width)
                            .max(visual.bounds.x + 2.0),
                    );
                    let mut popup_y = caret_y;
                    if popup_y + popup_height > visual.bounds.y + visual.bounds.height {
                        popup_y = (caret_y - row_height - popup_height).max(visual.bounds.y + 2.0);
                    }
                    let popup_bounds = UiBounds {
                        x: popup_x,
                        y: popup_y,
                        width: popup_width,
                        height: popup_height,
                    };
                    let popup_clip = UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: viewport_logical_size[0],
                        height: viewport_logical_size[1],
                    };
                    output.editor_popup_rects.push(overlay_instance(
                        popup_bounds,
                        popup_clip,
                        theme.popup_background,
                    ));
                    let text_start_y = popup_y + COMPLETION_PAD * 0.5;
                    for (item_index, item) in completion.items.iter().enumerate() {
                        if item_index >= COMPLETION_VISIBLE_ITEMS {
                            break;
                        }
                        let item_y = text_start_y + item_index as f32 * item_height;
                        if item_index == completion.selected {
                            output.editor_popup_rects.push(overlay_instance(
                                UiBounds {
                                    x: popup_x + 2.0,
                                    y: item_y,
                                    width: popup_width - 4.0,
                                    height: item_height,
                                },
                                popup_clip,
                                theme.popup_selection,
                            ));
                        }
                        let mut ix = popup_x + COMPLETION_PAD;
                        let baseline = (item_y
                            + editor_line_metrics(font, declaration, state.font_scale).ascent)
                            .floor();
                        for ch in kind_prefix(&item.kind).chars() {
                            let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px)
                            else {
                                continue;
                            };
                            output.editor_popup_texts.push(UiTextInstance {
                                rect: [
                                    (ix + glyph.xmin).floor(),
                                    baseline + glyph.plane_min_y.floor(),
                                    glyph.width,
                                    glyph.height,
                                ],
                                color: completion_kind_color(&item.kind, &theme),
                                clip: [
                                    popup_clip.x,
                                    popup_clip.y,
                                    popup_clip.x + popup_clip.width,
                                    popup_clip.y + popup_clip.height,
                                ],
                                uv: glyph.uv,
                                depth,
                                paint_group_id,
                                animation: base_track,
                                transform_from: [0.0, 0.0, 1.0, 1.0],
                                transform_to: [0.0, 0.0, 1.0, 1.0],
                                rotation_pivot: [0.0; 4],
                                overflow: [0.0; 4],
                            });
                            ix += glyph.advance;
                        }
                        ix += 6.0;
                        for ch in item.label.chars() {
                            let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px)
                            else {
                                continue;
                            };
                            output.editor_popup_texts.push(UiTextInstance {
                                rect: [
                                    (ix + glyph.xmin).floor(),
                                    baseline + glyph.plane_min_y.floor(),
                                    glyph.width,
                                    glyph.height,
                                ],
                                color: [0.86, 0.90, 0.95, 1.0],
                                clip: [
                                    popup_clip.x,
                                    popup_clip.y,
                                    popup_clip.x + popup_clip.width,
                                    popup_clip.y + popup_clip.height,
                                ],
                                uv: glyph.uv,
                                depth,
                                paint_group_id,
                                animation: base_track,
                                transform_from: [0.0, 0.0, 1.0, 1.0],
                                transform_to: [0.0, 0.0, 1.0, 1.0],
                                rotation_pivot: [0.0; 4],
                                overflow: [0.0; 4],
                            });
                            ix += glyph.advance;
                        }
                    }
                }
            }
        }
        for state in editors.values_mut() {
            state.layout_dirty = false;
        }
        output
    }
}

impl super::UiWgpuRenderer {
    /// Reconciles renderer-local presentation mirrors with the submitted
    /// fragments: `CodeEditorDeclaration` effects provide the declaration
    /// (theme / materials / font), `CodeEditorPresentation` effects provide
    /// the per-frame editing snapshot. A mirror rebuilds only when the
    /// presentation revision changes.
    pub(crate) fn reconcile_editors(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) {
        let mut desired: HashMap<String, (UiCodeEditorDeclaration, UiCodeEditorPresentation)> =
            HashMap::new();
        for fragment in fragments.values() {
            let mut kinds = HashMap::new();
            collect_node_kinds(&fragment.root, &mut kinds);
            let mut pres_map: HashMap<String, UiCodeEditorPresentation> = HashMap::new();
            let mut declarations: HashMap<String, UiCodeEditorDeclaration> = HashMap::new();
            for effect in &fragment.effects {
                match effect {
                    neon_ui_schema::UiEffect::CodeEditorPresentation { presentations } => {
                        for (key, presentation) in presentations {
                            pres_map.insert(key.clone(), presentation.clone());
                        }
                    }
                    neon_ui_schema::UiEffect::CodeEditorDeclaration { node_key, declaration } => {
                        declarations.insert(node_key.clone(), declaration.clone());
                    }
                    _ => {}
                }
            }
            // The desired set comes from declarations (a lowered code_editor
            // always carries the declaration; the presentation may ride the
            // fragment or arrive via the external slot, filled below).
            for (node_key, declaration) in declarations {
                // Only accept declarations whose lowered node is still a
                // Panel in this fragment (the renderer needs a draw target).
                if kinds.get(&node_key) != Some(&UiNodeKind::Panel) {
                    continue;
                }
                let path = format!("{}/{}", fragment.fragment_id.0, node_key);
                let presentation = pres_map.remove(&node_key).unwrap_or_default();
                desired.insert(path, (declaration, presentation));
            }
        }
        // Merge externally-published presentations (the host's ui-runtime
        // editor component publishes fresh snapshots after handling input;
        // they key by node_key like the fragment effects do).
        if let Some(slot) = &self.editor_external_presentations
            && let Ok(external) = slot.lock()
        {
            for presentation in external.iter() {
                let path = desired
                    .keys()
                    .find(|path| path.ends_with(&format!("/{}", presentation.node_key)))
                    .cloned();
                if let Some(path) = path
                    && let Some((_, existing)) = desired.get_mut(&path)
                {
                    *existing = presentation.clone();
                }
            }
        }
        // Destroy mirrors whose presentations disappeared.
        self.editors.retain(|path, _| desired.contains_key(path));
        // Create / update mirrors.
        for (path, (declaration, presentation)) in desired {
            if let Some(state) = self.editors.get_mut(&path) {
                if state.presentation_revision != presentation.revision {
                    *state = EditorRuntimeState::from_presentation(declaration, presentation);
                }
            } else {
                self.editors.insert(
                    path,
                    EditorRuntimeState::from_presentation(declaration, presentation),
                );
            }
        }
    }

    /// Attaches the ui-runtime input sink (host bridge).
    pub(crate) fn set_editor_input_sink(
        &mut self,
        sink: Box<
            dyn FnMut(neon_ui_schema::UiEditorInputEvent, f32)
                -> Vec<EditorCommit>
                + Send,
        >,
    ) {
        self.editor_input_sink = Some(sink);
    }

    /// Attaches the shared presentations slot (host bridge).
    pub(crate) fn set_editor_external_presentations(
        &mut self,
        slot: std::sync::Arc<std::sync::Mutex<Vec<neon_ui_schema::UiCodeEditorPresentation>>>,
    ) {
        self.editor_external_presentations = Some(slot);
    }

    /// Whether any code editor currently owns keyboard focus.
    pub(crate) fn editor_focused(&self) -> bool {
        self.editors.values().any(|state| state.focus)
    }

    /// Path of the focused editor mirror, if any.
    fn focused_editor_path(&self) -> Option<String> {
        self.editors
            .iter()
            .find(|(_, state)| state.focus)
            .map(|(path, _)| path.clone())
    }

    /// IME caret rect of the focused editor (for the platform IME window).
    pub(crate) fn editor_ime_rect(&self) -> Option<UiBounds> {
        let path = self.focused_editor_path()?;
        let index = self.plan_index_of(&path)?;
        let state = self.editors.get(&path)?;
        let visual = &self.sampled[index];
        let font = self.resident_font.as_ref()?;
        let raster_px = editor_px(&state.declaration, state.font_scale);
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter =
            editor_gutter_width(&state.declaration, state.lines.len() as u32, state.font_scale);
        let line_text = line_of(state, state.caret.line);
        let x = visual.bounds.x
            + gutter
            + line_prefix_advance(font, line_text, state.caret.column, raster_px)
            - state.scroll_x;
        let y = visual.bounds.y + state.caret.line as f32 * row_height - state.scroll_y;
        Some(UiBounds {
            x,
            y,
            width: 2.0,
            height: row_height,
        })
    }

    /// Topmost code editor containing `pointer`, if any.
    pub(crate) fn editor_at_pointer(&self, pointer: [f32; 2]) -> Option<String> {
        for index in (0..self.plan.len()).rev() {
            let path = &self.plan[index].id;
            if !self.editors.contains_key(path) {
                continue;
            }
            let visual = &self.sampled[index];
            if visual.world_depth.is_some() {
                continue;
            }
            if self.plan[index].instance_index.is_none() {
                continue;
            }
            let in_panel = contains(visual.bounds, pointer);
            let in_content = self
                .editors
                .get(path)
                .is_some_and(|st| contains(st.content_rect, pointer));
            if in_panel || in_content {
                return Some(path.clone());
            }
        }
        None
    }

    /// Maps a pointer position inside an editor to (line, column) using the
    /// resident font metrics (pure geometry; the semantics run elsewhere).
    fn pointer_to_position(
        &self,
        path: &str,
        index: usize,
        pointer: [f32; 2],
    ) -> Option<(u32, u32)> {
        let font = self.resident_font.as_ref()?;
        let state = self.editors.get(path)?;
        let visual = &self.sampled[index];
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter =
            editor_gutter_width(&state.declaration, state.lines.len() as u32, state.font_scale);
        let line_count = state.lines.len() as u32;
        let line = if row_height > 0.0 {
            ((pointer[1] - visual.bounds.y + state.scroll_y) / row_height)
                .floor()
                .max(0.0) as u32
        } else {
            0
        };
        let line = line.min(line_count.saturating_sub(1));
        let line_text = line_of(state, line);
        let x = pointer[0] - (visual.bounds.x + gutter - state.scroll_x);
        let column = column_from_x(font, line_text, x, editor_px(&state.declaration, state.font_scale));
        Some((line, column.min(line_text.chars().count() as u32)))
    }

    /// Renderer metrics for a mirrored editor (font-derived), attached to
    /// forwarded key/zoom events so the ui-runtime can scroll without fonts.
    fn editor_metrics(
        &self,
        path: &str,
        index: usize,
    ) -> Option<(f32, f32, f32, f32)> {
        let font = self.resident_font.as_ref()?;
        let state = self.editors.get(path)?;
        let visual = &self.sampled[index];
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter =
            editor_gutter_width(&state.declaration, state.lines.len() as u32, state.font_scale);
        Some((
            visual.bounds.height,
            visual.bounds.width,
            row_height,
            gutter,
        ))
    }

    /// Forwards an input event to the ui-runtime editor component. Returns
    /// whether the event was accepted (a sink is attached). `now` is the
    /// renderer animation clock so fx lifetimes stay on one timeline.
    fn forward_editor_input(&mut self, event: UiEditorInputEvent, now: f32) -> bool {
        let Some(sink) = self.editor_input_sink.as_mut() else {
            return false;
        };
        let commits = sink(event, now);
        self.editor_pending_commits.extend(commits);
        true
    }

    /// Pointer press inside a code editor: maps to (line, column) and
    /// forwards the semantic press. Returns whether consumed.
    pub(crate) fn editor_pointer_press(&mut self, pointer: [f32; 2]) -> bool {
        let Some(path) = self.editor_at_pointer(pointer) else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        self.editor_selection_drag = true;
        let Some((line, column)) = self.pointer_to_position(&path, index, pointer) else {
            return true;
        };
        self.forward_editor_input(
            UiEditorInputEvent::PointerPress { path, line, column },
            self.animation_clock_seconds,
        )
    }

    /// Extends the editor selection while the pointer is held down.
    pub(crate) fn editor_pointer_drag(&mut self, pointer: [f32; 2]) {
        if !self.editor_selection_drag {
            return;
        }
        let Some(path) = self.editor_at_pointer(pointer) else {
            return;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return;
        };
        let Some((line, column)) = self.pointer_to_position(&path, index, pointer) else {
            return;
        };
        self.forward_editor_input(
            UiEditorInputEvent::PointerDrag { path, line, column },
            self.animation_clock_seconds,
        );
    }

    /// Ends a selection drag.
    pub(crate) fn editor_pointer_release(&mut self) {
        self.editor_selection_drag = false;
        self.forward_editor_input(
            UiEditorInputEvent::PointerRelease,
            self.animation_clock_seconds,
        );
    }

    /// Mouse wheel over a code editor scrolls it. Returns whether consumed.
    pub(crate) fn editor_scroll_at_pointer(&mut self, delta: [f32; 2]) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(path) = self.editor_at_pointer(pointer) else {
            return false;
        };
        self.forward_editor_input(
            UiEditorInputEvent::Scroll { path, delta },
            self.animation_clock_seconds,
        )
    }

    /// Ctrl + wheel over a code editor zooms it (lossless font rescale).
    pub(crate) fn editor_zoom_at_pointer(&mut self, wheel_y: f32) -> bool {
        if wheel_y == 0.0 {
            return false;
        }
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(path) = self.editor_at_pointer(pointer) else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        let Some((viewport_height, viewport_width, row_height, gutter_width)) =
            self.editor_metrics(&path, index)
        else {
            return false;
        };
        let factor = if wheel_y > 0.0 { 1.12 } else { 1.0 / 1.12 };
        self.forward_editor_input(
            UiEditorInputEvent::Zoom {
                path,
                factor,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
            },
            self.animation_clock_seconds,
        )
    }

    /// Routes one pressed key to the focused editor. Returns whether consumed.
    pub(crate) fn editor_handle_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
    ) -> bool {
        let Some(path) = self.focused_editor_path() else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        let Some((viewport_height, viewport_width, row_height, gutter_width)) =
            self.editor_metrics(&path, index)
        else {
            return false;
        };
        let kind = match key {
            Key::Character(character) => UiEditorKeyKind::Character(character.to_string()),
            Key::Named(named) => UiEditorKeyKind::Named(named_key_name(*named).to_string()),
            _ => return false,
        };
        self.forward_editor_input(
            UiEditorInputEvent::Key {
                path,
                kind,
                text: text.map(str::to_string),
                shift,
                ctrl,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
            },
            self.animation_clock_seconds,
        )
    }

    /// IME preedit for the focused editor (forwarded to the ui-runtime).
    pub(crate) fn editor_ime_preedit(&mut self, value: &str) {
        let Some(path) = self.focused_editor_path() else {
            return;
        };
        self.forward_editor_input(
            UiEditorInputEvent::ImePreedit {
                path,
                value: value.to_string(),
            },
            self.animation_clock_seconds,
        );
    }

    /// IME commit for the focused editor (forwarded to the ui-runtime).
    pub(crate) fn editor_ime_commit(&mut self, value: &str) -> bool {
        let Some(path) = self.focused_editor_path() else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        let Some((viewport_height, viewport_width, row_height, gutter_width)) =
            self.editor_metrics(&path, index)
        else {
            return false;
        };
        self.forward_editor_input(
            UiEditorInputEvent::ImeCommit {
                path,
                value: value.to_string(),
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
            },
            self.animation_clock_seconds,
        )
    }

    /// Blurs the focused editor: forwards an Escape (commit + blur) semantic
    /// to the ui-runtime.
    pub(crate) fn blur_editor(&mut self) {
        let Some(path) = self.focused_editor_path() else {
            return;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return;
        };
        let Some((viewport_height, viewport_width, row_height, gutter_width)) =
            self.editor_metrics(&path, index)
        else {
            self.editor_selection_drag = false;
            return;
        };
        self.forward_editor_input(
            UiEditorInputEvent::Key {
                path,
                kind: UiEditorKeyKind::Named("Escape".to_string()),
                text: None,
                shift: false,
                ctrl: false,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
            },
            self.animation_clock_seconds,
        );
        self.editor_selection_drag = false;
    }

    /// Takes queued editor commits (one per blurred/explicit-save editor).
    pub(crate) fn take_editor_commits(&mut self) -> Vec<EditorCommit> {
        std::mem::take(&mut self.editor_pending_commits)
    }
}

/// Stable name for a winit `NamedKey` (matches the ui-runtime's named-key
/// dispatch table).
fn named_key_name(named: NamedKey) -> &'static str {
    match named {
        NamedKey::ArrowDown => "ArrowDown",
        NamedKey::ArrowLeft => "ArrowLeft",
        NamedKey::ArrowRight => "ArrowRight",
        NamedKey::ArrowUp => "ArrowUp",
        NamedKey::Backspace => "Backspace",
        NamedKey::Delete => "Delete",
        NamedKey::End => "End",
        NamedKey::Enter => "Enter",
        NamedKey::Escape => "Escape",
        NamedKey::Home => "Home",
        NamedKey::PageDown => "PageDown",
        NamedKey::PageUp => "PageUp",
        NamedKey::Space => "Space",
        NamedKey::Tab => "Tab",
        _ => "Unidentified",
    }
}

fn collect_node_kinds(node: &neon_ui_schema::UiNode, out: &mut HashMap<String, UiNodeKind>) {
    out.insert(node.node_id.0.clone(), node.kind.clone());
    for child in &node.children {
        collect_node_kinds(child, out);
    }
}
