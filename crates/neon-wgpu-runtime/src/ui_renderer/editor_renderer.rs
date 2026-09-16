//! Renderer-local code editor: state, layout, and interaction.
//!
//! Per `docs/nui-flow-code-editor.md` (M4/M5): the unified WGPU renderer owns
//! the frame-rate local editor presentation (line numbers, token-colored text,
//! current-line highlight, selection, blinking caret, scroll, completion
//! popup) while `neon-editor-core` owns the editable buffer, incremental
//! highlight cache, completion engine, and undo/redo. The host receives only
//! commit ChangeSets through the declared semantic event; every local
//! keystroke is Layer 1 presentation and never becomes per-frame RPC.
//!
//! This module is a child of `ui_renderer.rs` so it shares the renderer's
//! private instance/font plumbing without widening any public API.

use std::collections::HashMap;

use super::editor_theme::{EditorTheme, editor_theme_from};
use neon_editor::grammar::nui_flow_default;
use neon_editor::{
    CompletionItem, CompletionKind, EditEventKind, EditorCore, Position, TokenClass,
};
use neon_ui_schema::{TextRef, UiCodeEditorDeclaration, UiEditorLanguage, UiNodeKind};
use winit::keyboard::{Key, NamedKey};

use super::{
    ResidentFont, UiBounds, UiFragment, UiInstance, UiTextInstance, color_pass_depth, contains,
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
/// the declared `event` action (from `event <dotted.intent>` on the
/// `code_editor` node) and the full current document text.
#[derive(Clone, Debug)]
pub(crate) struct EditorCommit {
    pub node_path: String,
    pub event_action: Option<String>,
    pub document: String,
}

/// Open completion popup state. The item list is snapshotted at Ctrl+Space
/// time; typing or explicit dismissal closes it.
#[derive(Clone, Debug)]
pub(super) struct EditorCompletionState {
    pub items: Vec<CompletionItem>,
    pub selected: usize,
}

/// Transient shader packages applied to character-level edits. Inserted
/// glyphs get a quick type-in (fade + lift); deleted glyphs keep a snapshot
/// fragment that bursts away. The renderer owns the lifecycle: an fx lives
/// for `duration_ms` after the edit event landed, then disappears, mirroring
/// how one-shot node text materials expire.
const EDIT_FX_INSERT_PACKAGE: &str = "text-type-in";
const EDIT_FX_DELETE_PACKAGE: &str = "text-delete-fragment";
const EDIT_FX_INSERT_DURATION_MS: u32 = 420;
const EDIT_FX_DELETE_DURATION_MS: u32 = 620;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EditFxKind {
    Insert,
    Delete,
}

/// One transient edit effect. `instances` is populated during layout:
/// Insert fx resolve glyphs from the current layout every frame (so they
/// follow scroll), Delete fx snapshot their ghost glyphs once (`generated`)
/// at the pre-delete estimate position.
pub(super) struct EditorEditFx {
    pub kind: EditFxKind,
    pub package_id: String,
    pub row: u32,
    pub col: u32,
    pub len: usize,
    pub instances: Vec<UiTextInstance>,
    pub generated: bool,
    pub started_seconds: f32,
    pub duration_ms: u32,
}

/// Renderer-local state for one code-editor component.
pub(super) struct EditorRuntimeState {
    pub declaration: UiCodeEditorDeclaration,
    pub core: EditorCore,
    /// Declared semantic event action (`event` attribute on the node).
    pub event_action: Option<String>,
    /// Last source text adopted from the fragment. Local edits never rewrite
    /// it; an external fragment update only re-adopts when this differs and
    /// the editor is not focused (no host write while the user types).
    pub adopted_source: String,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub caret: Position,
    pub selection_anchor: Option<Position>,
    pub focus: bool,
    pub completion: Option<EditorCompletionState>,
    pub preedit: String,
    pub last_edit_seconds: f32,
    pub pending_edits: bool,
    /// Active transient edit effects (type-in / delete fragment). Bounded:
    /// every layout retires entries older than their duration.
    pub edit_fx: Vec<EditorEditFx>,
    /// Runtime zoom factor for lossless font rescaling (Ctrl + wheel).
    /// Base size comes from `declaration.font_size`; every layout metric is
    /// multiplied by this so glyphs re-rasterize at the new size instead of
    /// being stretched.
    pub font_scale: f32,
    /// Set by Ctrl+wheel zoom so the plan-reuse early return cannot skip the
    /// layout pass; cleared at the end of each layout_editors run.
    pub layout_dirty: bool,
}

impl EditorRuntimeState {
    fn new(declaration: UiCodeEditorDeclaration, source: &str) -> Self {
        let grammar = match declaration.language {
            UiEditorLanguage::NuiFlow => nui_flow_default(),
        };
        Self {
            declaration,
            core: EditorCore::new(source, grammar),
            event_action: None,
            adopted_source: source.to_string(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            caret: Position::START,
            selection_anchor: None,
            focus: false,
            completion: None,
            preedit: String::new(),
            last_edit_seconds: 0.0,
            pending_edits: false,
            edit_fx: Vec::new(),
            font_scale: 1.0,
            layout_dirty: false,
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
    pub editor_rects: Vec<UiInstance>,
    /// Caret + completion popup background/selected-item highlight (drawn
    /// above glyphs, in the popup instance pass).
    pub editor_popup_rects: Vec<UiInstance>,
    /// Code-glyph instances routed to the per-package text-material pass when
    /// the code_editor node declares a `text_material`. Each entry is
    /// `(package_id, instances)`; glyph rects/clips are pre-expanded by the
    /// material overflow, mirroring how text nodes route into
    /// `text_material_batches`. Line numbers stay in `editor_texts`.
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

/// Type-ahead completion: after a character edit, if the caret sits right
/// after an identifier character, ask editor-core for candidates (it already
/// filters by the typed prefix and NUI context) and open or refresh the popup.
/// A non-identifier context (space, punctuation, empty) dismisses it. This is
/// component-internal Layer 1: no RPC, the popup render/navigation is shared
/// with Ctrl+Space.
fn auto_complete(state: &mut EditorRuntimeState) {
    let line = state
        .core
        .buffer()
        .line(state.caret.line)
        .unwrap_or_default();
    let before = line
        .chars()
        .take(state.caret.column as usize)
        .collect::<String>();
    let ident_typed = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .count()
        > 0;
    // NUI line-leading: the caret after leading whitespace is where node
    // kinds / attributes complete, so a whitespace-only prefix also triggers
    // (editor-core decides whether the context yields candidates).
    let line_leading = !before.is_empty() && before.chars().all(|c| c.is_whitespace());
    if !ident_typed && !line_leading {
        state.completion = None;
        return;
    }
    let items = state.core.completions(state.caret);
    if items.is_empty() {
        state.completion = None;
        return;
    }
    let selected = state
        .completion
        .as_ref()
        .map_or(0, |c| c.selected.min(items.len().saturating_sub(1)));
    state.completion = Some(EditorCompletionState { items, selected });
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

fn completion_kind_color(kind: CompletionKind, theme: &EditorTheme) -> [f32; 4] {
    match kind {
        CompletionKind::Keyword => theme.token(TokenClass::Keyword, 1.0),
        CompletionKind::NodeKind => theme.token(TokenClass::NodeKind, 1.0),
        CompletionKind::Attribute => theme.token(TokenClass::Attribute, 1.0),
        CompletionKind::Input | CompletionKind::InputKind => {
            theme.token(TokenClass::InputRef, 1.0)
        }
        // LSP-sourced values (TS/Rust/C++): neutral identifier color.
        CompletionKind::Value => theme.token(TokenClass::Ident, 1.0),
    }
}

fn kind_prefix(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Keyword => "kw",
        CompletionKind::NodeKind => "nd",
        CompletionKind::Attribute => "at",
        CompletionKind::Input => "in",
        CompletionKind::InputKind => "ik",
        CompletionKind::Value => "vl",
    }
}

/// Ordered selection endpoints from `anchor` + `caret`.
fn ordered_selection(anchor: Position, caret: Position) -> (Position, Position) {
    if anchor <= caret {
        (anchor, caret)
    } else {
        (caret, anchor)
    }
}

/// Clamp scroll offsets into the document's content bounds.
fn clamp_scroll(
    state: &mut EditorRuntimeState,
    content_width: f32,
    content_height: f32,
    viewport_width: f32,
    viewport_height: f32,
) {
    let max_x = (content_width - viewport_width).max(0.0);
    let max_y = (content_height - viewport_height).max(0.0);
    state.scroll_x = state.scroll_x.clamp(0.0, max_x);
    state.scroll_y = state.scroll_y.clamp(0.0, max_y);
}

/// Keep the caret visible after a move/edit.
fn scroll_caret_into_view(
    state: &mut EditorRuntimeState,
    font: &ResidentFont,
    editor_bounds: UiBounds,
    gutter_width: f32,
) {
    let raster_px = editor_px(&state.declaration, state.font_scale);
    let row_height = editor_row_height(font, &state.declaration, state.font_scale);
    let viewport_width = (editor_bounds.width - gutter_width).max(1.0);
    let line_text = state
        .core
        .buffer()
        .line(state.caret.line)
        .unwrap_or_default();
    let caret_x = line_prefix_advance(font, line_text, state.caret.column, raster_px);
    if caret_x < state.scroll_x {
        state.scroll_x = caret_x;
    } else if caret_x > state.scroll_x + viewport_width - 8.0 {
        state.scroll_x = caret_x - viewport_width + 16.0;
    }
    let caret_y = state.caret.line as f32 * row_height;
    if caret_y < state.scroll_y {
        state.scroll_y = caret_y;
    } else if caret_y + row_height > state.scroll_y + editor_bounds.height {
        state.scroll_y = caret_y + row_height - editor_bounds.height;
    }
    let content_width = line_full_advance(font, line_text, raster_px).max(viewport_width);
    let content_height = state.core.buffer().line_count() as f32 * row_height;
    clamp_scroll(
        state,
        content_width,
        content_height,
        viewport_width,
        editor_bounds.height,
    );
}

fn selected_range_for_row(
    anchor: Position,
    caret: Position,
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

impl super::UiWgpuRenderer {
    /// Layouts every code editor in the current plan into glyph + rect
    /// instances. Reads editor state only; scrolling/caret mutations happen
    /// in the interaction handlers, never here, so a draw pass cannot alter
    /// state.
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
            // Code editors are screen-UI presentation; projected world panels are
            // not yet supported for editor chrome.
            if visual.world_depth.is_some() {
                continue;
            }
            // The lowered node keeps its Panel kind; only an editor state makes it
            // an editor. Exiting/removed nodes are skipped (instance_index None).
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
            // Drain character-level edits into transient fx (type-in for
            // inserts, fragment burst for deletes). Instances are resolved
            // during the visible-rows loop below.
            for event in state.core.take_edit_events() {
                let (kind, package_id, duration_ms) = match event.kind {
                    EditEventKind::Insert => (
                        EditFxKind::Insert,
                        EDIT_FX_INSERT_PACKAGE,
                        EDIT_FX_INSERT_DURATION_MS,
                    ),
                    EditEventKind::Delete => (
                        EditFxKind::Delete,
                        EDIT_FX_DELETE_PACKAGE,
                        EDIT_FX_DELETE_DURATION_MS,
                    ),
                };
                state.edit_fx.push(EditorEditFx {
                    kind,
                    package_id: package_id.into(),
                    row: event.row,
                    col: event.column,
                    len: event.text.chars().count(),
                    instances: Vec::new(),
                    generated: false,
                    started_seconds: time_seconds,
                    duration_ms,
                });
            }
            let raster_px = editor_px(declaration, state.font_scale);
            let row_height = editor_row_height(font, declaration, state.font_scale);
            if row_height <= 0.0 || visual.bounds.width <= 0.0 || visual.bounds.height <= 0.0 {
                continue;
            }
            let line_count = state.core.buffer().line_count();
            let gutter_width = editor_gutter_width(declaration, line_count, state.font_scale);
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
                let row_y = visual.bounds.y + state.caret.line as f32 * row_height - state.scroll_y;
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
                    let line_text = state.core.buffer().line(row).unwrap_or_default();
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
                    let x_from = content_x + line_prefix_advance(font, line_text, from, raster_px)
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
                let line_text = state.core.buffer().line(row).unwrap_or_default();
                let row_y = visual.bounds.y + row as f32 * row_height - state.scroll_y;
                if row_y + row_height < visual.bounds.y
                    || row_y > visual.bounds.y + visual.bounds.height
                {
                    continue;
                }
                let baseline = (row_y + editor_line_metrics(font, declaration, state.font_scale).ascent).floor();

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

                // Token-colored code text. Whitespace between spans renders in the
                // default color so proportional fonts keep their natural spacing.
                let highlight = state.core.line_spans(row);
                let mut column = 0u32;
                let mut x = content_x - state.scroll_x;
                for ch in line_text.chars() {
                    let color = highlight
                        .and_then(|tokens| tokens.class_at(column))
                        .map_or_else(
                            || rgba(theme.text, opacity),
                            |class| theme.token(class, opacity),
                        );
                    let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
                        continue;
                    };
                    // Transient edit fx: inserted glyphs in the fx column
                    // range ride the type-in package; a delete fx snapshots
                    // its ghost at the pre-delete estimate position once.
                    for fx in state.edit_fx.iter_mut() {
                        if fx.kind == EditFxKind::Insert
                            && fx.row == row
                            && column >= fx.col
                            && column < fx.col + fx.len as u32
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
                            fx.instances.push(inst);
                        } else if fx.kind == EditFxKind::Delete
                            && fx.row == row
                            && !fx.generated
                            && column == fx.col
                        {
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
                            fx.instances.push(ghost);
                            fx.generated = true;
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
                    // dedicated entry. Glyph routing mirrors the text-node
                    // path: expand rect/clip by the material overflow so a
                    // glow can paint outside the glyph box.
                    // Selected spans (if the editor declares `selection_shader`)
                    // get the glow material in preference to the class shader;
                    // without a selection material the class/fallback stands.
                    let class_material = {
                        let base = highlight
                            .and_then(|tokens| tokens.class_at(column))
                            .and_then(|class| declaration.token_materials.get(class.name()));
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

                // IME preedit text renders at the caret position with a dim color.
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

            // Retire expired fx and flush their material batches alongside the
            // static token/whole-node materials.
            state
                .edit_fx
                .retain(|fx| time_seconds - fx.started_seconds < fx.duration_ms as f32 / 1000.0);
            for fx in &state.edit_fx {
                if !fx.instances.is_empty() {
                    output
                        .editor_text_materials
                        .push((fx.package_id.clone(), fx.instances.clone()));
                }
            }
            // Flush token-class batches, then the whole-node material (line
            // numbers are intentionally excluded above; only token-colored
            // code glyphs route to shaders).
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
                    let caret_line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
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
                    let caret_line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
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
                        let baseline =
                            (item_y + editor_line_metrics(font, declaration, state.font_scale).ascent).floor();
                        for ch in kind_prefix(item.kind).chars() {
                            let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
                                continue;
                            };
                            output.editor_popup_texts.push(UiTextInstance {
                                rect: [
                                    (ix + glyph.xmin).floor(),
                                    baseline + glyph.plane_min_y.floor(),
                                    glyph.width,
                                    glyph.height,
                                ],
                                color: completion_kind_color(item.kind, &theme),
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
                            let Ok(glyph) = ensure_glyph(device, queue, font, ch, raster_px) else {
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

fn collect_node_kinds(node: &neon_ui_schema::UiNode, out: &mut HashMap<String, UiNodeKind>) {
    out.insert(node.node_id.0.clone(), node.kind.clone());
    for child in &node.children {
        collect_node_kinds(child, out);
    }
}

fn collect_node_literal_text(node: &neon_ui_schema::UiNode, out: &mut HashMap<String, String>) {
    if let Some(TextRef::Literal { value }) = &node.text {
        out.insert(node.node_id.0.clone(), value.clone());
    }
    for child in &node.children {
        collect_node_literal_text(child, out);
    }
}

impl super::UiWgpuRenderer {
    /// Reconciles renderer-local editor states with the submitted fragments.
    /// Called after `refresh_plan`; creates/destroys editor mirrors and adopts
    /// external document updates only while the editor is not focused.
    pub(crate) fn reconcile_editors(
        &mut self,
        fragments: &HashMap<neon_ui_schema::UiFragmentId, UiFragment>,
    ) {
        let mut desired: HashMap<String, (UiCodeEditorDeclaration, Option<String>, String)> =
            HashMap::new();
        for fragment in fragments.values() {
            let mut kinds = HashMap::new();
            collect_node_kinds(&fragment.root, &mut kinds);
            let mut sources = HashMap::new();
            collect_node_literal_text(&fragment.root, &mut sources);
            let mut events = HashMap::new();
            for effect in &fragment.effects {
                if let neon_ui_schema::UiEffect::BoundSemanticIntent { node_id, intent } = effect
                    && let neon_ui_schema::UiIntent::Invoke { action, .. } = intent
                {
                    events.insert(node_id.0.clone(), action.clone());
                }
            }
            for effect in &fragment.effects {
                let neon_ui_schema::UiEffect::CodeEditorDeclaration {
                    node_key,
                    declaration,
                } = effect
                else {
                    continue;
                };
                // Only accept declarations whose lowered node is still a Panel
                // in this fragment (the renderer requires a real draw target).
                if kinds.get(node_key) != Some(&UiNodeKind::Panel) {
                    continue;
                }
                let path = format!("{}/{}", fragment.fragment_id.0, node_key);
                let source = sources.get(node_key).cloned().unwrap_or_default();
                desired.insert(
                    path,
                    (declaration.clone(), events.get(node_key).cloned(), source),
                );
            }
        }
        // Destroy editors whose declarations disappeared.
        self.editors.retain(|path, _| desired.contains_key(path));
        if let Some(focused) = self.focused_editor.clone() {
            if !self.editors.contains_key(&focused) {
                self.focused_editor = None;
                self.editor_selection_drag = false;
            }
        }
        // Create / update.
        for (path, (declaration, event_action, source)) in desired {
            if let Some(state) = self.editors.get_mut(&path) {
                let needs_rebuild = state.declaration.language != declaration.language
                    || (state.adopted_source != source && !state.focus);
                state.declaration = declaration;
                state.event_action = event_action;
                if needs_rebuild {
                    state.core = EditorCore::new(&source, nui_flow_default());
                    state.adopted_source = source;
                    state.caret = Position::START;
                    state.selection_anchor = None;
                    state.completion = None;
                    state.scroll_x = 0.0;
                    state.scroll_y = 0.0;
                    state.preedit.clear();
                }
            } else {
                let mut state = EditorRuntimeState::new(declaration, &source);
                state.event_action = event_action;
                self.editors.insert(path, state);
            }
        }
    }

    /// Whether any code editor currently owns keyboard focus.
    pub(crate) fn editor_focused(&self) -> bool {
        self.focused_editor.is_some()
    }

    /// IME caret rect of the focused editor (for the platform IME window).
    pub(crate) fn editor_ime_rect(&self) -> Option<UiBounds> {
        let path = self.focused_editor.as_ref()?;
        let index = self.plan_index_of(path)?;
        let state = self.editors.get(path)?;
        let visual = &self.sampled[index];
        let font = self.resident_font.as_ref()?;
        let raster_px = editor_px(&state.declaration, state.font_scale);
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter = editor_gutter_width(&state.declaration, state.core.buffer().line_count(), state.font_scale);
        let line_text = state
            .core
            .buffer()
            .line(state.caret.line)
            .unwrap_or_default();
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
            if contains(visual.bounds, pointer) {
                return Some(path.clone());
            }
        }
        None
    }

    /// Pointer press inside a code editor: focus, place the caret, start a
    /// selection drag. Blurs (and commits) the previously focused editor.
    /// Returns whether the press was consumed by an editor.
    pub(crate) fn editor_pointer_press(&mut self, pointer: [f32; 2]) -> bool {
        let Some(path) = self.editor_at_pointer(pointer) else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        if self.focused_editor.as_deref() != Some(path.as_str()) {
            self.commit_focused_editor();
        }
        self.focused_editor = Some(path.clone());
        self.editor_selection_drag = true;
        let Some(state) = self.editors.get_mut(&path) else {
            return false;
        };
        state.focus = true;
        state.completion = None;
        let Some(font) = self.resident_font.as_ref() else {
            return true;
        };
        let visual = &self.sampled[index];
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter = editor_gutter_width(&state.declaration, state.core.buffer().line_count(), state.font_scale);
        let line_count = state.core.buffer().line_count();
        let line = if row_height > 0.0 {
            ((pointer[1] - visual.bounds.y + state.scroll_y) / row_height)
                .floor()
                .max(0.0) as u32
        } else {
            0
        };
        let line = line.min(line_count.saturating_sub(1));
        let line_text = state.core.buffer().line(line).unwrap_or_default();
        let x = pointer[0] - (visual.bounds.x + gutter - state.scroll_x);
        let column = column_from_x(font, line_text, x, editor_px(&state.declaration, state.font_scale));
        state.caret = Position::new(line, column.min(line_text.chars().count() as u32));
        state.selection_anchor = Some(state.caret);
        true
    }

    /// Extends the editor selection while the pointer is held down.
    pub(crate) fn editor_pointer_drag(&mut self, pointer: [f32; 2]) {
        if !self.editor_selection_drag {
            return;
        }
        let Some(path) = self.focused_editor.clone() else {
            return;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return;
        };
        let Some(font) = self.resident_font.as_ref() else {
            return;
        };
        let visual = &self.sampled[index];
        let Some(state) = self.editors.get_mut(&path) else {
            return;
        };
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        if row_height <= 0.0 {
            return;
        }
        // Clamp the pointer into the editor bounds so a drag beyond the edge
        // selects to the border instead of jumping.
        let clamped = [
            pointer[0].clamp(visual.bounds.x, visual.bounds.x + visual.bounds.width),
            pointer[1].clamp(visual.bounds.y, visual.bounds.y + visual.bounds.height),
        ];
        let line_count = state.core.buffer().line_count();
        let line = ((clamped[1] - visual.bounds.y + state.scroll_y) / row_height)
            .floor()
            .max(0.0) as u32;
        let line = line.min(line_count.saturating_sub(1));
        let gutter = editor_gutter_width(&state.declaration, line_count, state.font_scale);
        let line_text = state.core.buffer().line(line).unwrap_or_default();
        let x = clamped[0] - (visual.bounds.x + gutter - state.scroll_x);
        let column = column_from_x(font, line_text, x, editor_px(&state.declaration, state.font_scale))
            .min(line_text.chars().count() as u32);
        state.caret = Position::new(line, column);
    }

    /// Ends a selection drag.
    pub(crate) fn editor_pointer_release(&mut self) {
        self.editor_selection_drag = false;
    }

    /// Mouse wheel over a code editor scrolls it. Returns whether consumed.
    pub(crate) fn editor_scroll_at_pointer(&mut self, delta: [f32; 2]) -> bool {
        let Some(pointer) = self.pointer_position else {
            return false;
        };
        let Some(path) = self.editor_at_pointer(pointer) else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        let Some(font) = self.resident_font.as_ref() else {
            return false;
        };
        let visual = &self.sampled[index];
        let Some(state) = self.editors.get_mut(&path) else {
            return false;
        };
        let raster_px = editor_px(&state.declaration, state.font_scale);
        let row_height = editor_row_height(font, &state.declaration, state.font_scale);
        let gutter = editor_gutter_width(&state.declaration, state.core.buffer().line_count(), state.font_scale);
        state.scroll_y += delta[1];
        state.scroll_x += delta[0];
        let content_width = state
            .core
            .buffer()
            .lines()
            .map(|line| line_full_advance(font, line, raster_px))
            .fold(0.0_f32, f32::max)
            .max(visual.bounds.width - gutter);
        let content_height = state.core.buffer().line_count() as f32 * row_height;
        clamp_scroll(
            state,
            content_width,
            content_height,
            (visual.bounds.width - gutter).max(1.0),
            visual.bounds.height,
        );
        true
    }

    /// Ctrl + wheel over a code editor scales its font losslessly (glyphs
    /// re-rasterize at the new size). Returns whether consumed.
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
        let Some(state) = self.editors.get_mut(&path) else {
            return false;
        };
        let factor = if wheel_y > 0.0 { 1.12 } else { 1.0 / 1.12 };
        let next = (state.font_scale * factor).clamp(0.5, 2.5);
        if (next - state.font_scale).abs() < 0.001 {
            return false;
        }
        state.font_scale = next;
        state.layout_dirty = true;
        true
    }

    /// Routes one pressed key to the focused editor. Returns whether consumed.
    pub(crate) fn editor_handle_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
    ) -> bool {
        let Some(path) = self.focused_editor.clone() else {
            return false;
        };
        let Some(index) = self.plan_index_of(&path) else {
            return false;
        };
        let now = self.animation_clock_seconds;
        let editor_bounds = self.sampled[index].bounds;
        let editor_height = editor_bounds.height;
        let (Some(state), Some(font)) = (self.editors.get_mut(&path), self.resident_font.as_ref())
        else {
            return false;
        };

        let mark_edit = |state: &mut EditorRuntimeState, now: f32| {
            state.pending_edits = true;
            state.last_edit_seconds = now;
        };
        let insert_text = |state: &mut EditorRuntimeState, value: &str, now: f32| {
            if let Some(anchor) = state.selection_anchor {
                let (start, end) = ordered_selection(anchor, state.caret);
                let deleted = state.core.delete(start, end);
                state.selection_anchor = None;
                state.caret = state.core.insert(deleted, value);
            } else {
                state.caret = state.core.insert(state.caret, value);
            }
            state.completion = None;
            mark_edit(state, now);
        };
        let delete_backward = |state: &mut EditorRuntimeState, now: f32| {
            if let Some(anchor) = state.selection_anchor {
                let (start, end) = ordered_selection(anchor, state.caret);
                state.caret = state.core.delete(start, end);
                state.selection_anchor = None;
                mark_edit(state, now);
                return;
            }
            if state.caret.column > 0 {
                let position = Position::new(state.caret.line, state.caret.column - 1);
                state.caret = state.core.delete(position, state.caret);
            } else if state.caret.line > 0 {
                let previous_len = state
                    .core
                    .buffer()
                    .line(state.caret.line - 1)
                    .map_or(0, |line| line.chars().count() as u32);
                let start = Position::new(state.caret.line - 1, previous_len);
                state.caret = state.core.delete(start, state.caret);
            } else {
                return;
            }
            state.completion = None;
            mark_edit(state, now);
        };
        let delete_forward = |state: &mut EditorRuntimeState, now: f32| {
            if let Some(anchor) = state.selection_anchor {
                let (start, end) = ordered_selection(anchor, state.caret);
                state.caret = state.core.delete(start, end);
                state.selection_anchor = None;
                mark_edit(state, now);
                return;
            }
            let line_len = state
                .core
                .buffer()
                .line(state.caret.line)
                .map_or(0, |line| line.chars().count() as u32);
            if state.caret.column < line_len {
                let end = Position::new(state.caret.line, state.caret.column + 1);
                state.caret = state.core.delete(state.caret, end);
            } else if state.caret.line + 1 < state.core.buffer().line_count() {
                let end = Position::new(state.caret.line + 1, 0);
                state.caret = state.core.delete(state.caret, end);
            } else {
                return;
            }
            state.completion = None;
            mark_edit(state, now);
        };
        let move_caret = |state: &mut EditorRuntimeState, to: Position, extend: bool| {
            let to = state.core.buffer().clamp_position(to);
            if extend {
                state.selection_anchor.get_or_insert(state.caret);
            } else {
                state.selection_anchor = None;
            }
            state.caret = to;
        };
        // Commit pending edits inline (disjoint field writes only; the state
        // borrow is alive so no `self` method may run here).
        let commit_now =
            |state: &mut EditorRuntimeState, path: &str, pending: &mut Vec<EditorCommit>| {
                if state.pending_edits {
                    state.pending_edits = false;
                    let document = state.core.buffer().text();
                    state.core.commit();
                    state.completion = None;
                    state.preedit.clear();
                    pending.push(EditorCommit {
                        node_path: path.to_string(),
                        event_action: state.event_action.clone(),
                        document,
                    });
                }
            };
        let open_completion = |state: &mut EditorRuntimeState| {
            let items = state.core.completions(state.caret);
            state.completion = Some(EditorCompletionState { items, selected: 0 });
        };

        let mut consumed = true;

        // Character input (including Ctrl shortcuts — winit reports Ctrl+A as
        // `Key::Character("a")`, so shortcuts live here, not in the named
        // branch).
        if let Key::Character(value) = key {
            if ctrl {
                let lower = value.to_ascii_lowercase();
                match lower.as_str() {
                    " " => {
                        open_completion(state);
                    }
                    "a" => {
                        let last = state.core.buffer().line_count().saturating_sub(1);
                        let len = state
                            .core
                            .buffer()
                            .line(last)
                            .map_or(0, |line| line.chars().count() as u32);
                        state.selection_anchor = Some(Position::START);
                        state.caret = Position::new(last, len);
                    }
                    "c" => {
                        if let Some(anchor) = state.selection_anchor {
                            let (start, end) = ordered_selection(anchor, state.caret);
                            self.editor_clipboard = extract_range(state, start, end);
                        }
                    }
                    "x" => {
                        if let Some(anchor) = state.selection_anchor {
                            let (start, end) = ordered_selection(anchor, state.caret);
                            self.editor_clipboard = extract_range(state, start, end);
                            state.caret = state.core.delete(start, end);
                            state.selection_anchor = None;
                            mark_edit(state, now);
                        }
                    }
                    "v" => {
                        if !self.editor_clipboard.is_empty() {
                            insert_text(state, &self.editor_clipboard, now);
                        }
                    }
                    "z" => {
                        if shift {
                            state.core.redo();
                        } else {
                            state.core.undo();
                        }
                        state.caret = state.core.buffer().clamp_position(state.caret);
                        state.selection_anchor = None;
                        state.completion = None;
                        mark_edit(state, now);
                    }
                    "y" => {
                        state.core.redo();
                        state.caret = state.core.buffer().clamp_position(state.caret);
                        state.selection_anchor = None;
                        state.completion = None;
                        mark_edit(state, now);
                    }
                    "s" => {
                        commit_now(state, &path, &mut self.editor_pending_commits);
                    }
                    _ => {
                        consumed = false;
                    }
                }
            } else if let Some(text) = text {
                insert_text(state, text, now);
                auto_complete(state);
            } else {
                insert_text(state, value, now);
                auto_complete(state);
            }
            if consumed {
                let gutter =
                    editor_gutter_width(&state.declaration, state.core.buffer().line_count(), state.font_scale);
                scroll_caret_into_view(state, font, editor_bounds, gutter);
            }
            return consumed;
        }

        let named = match key {
            Key::Named(named) => *named,
            _ => return false,
        };

        // Completion popup interaction takes precedence.
        if state.completion.is_some() {
            match named {
                NamedKey::Escape => {
                    state.completion = None;
                    return true;
                }
                NamedKey::ArrowDown => {
                    let count = state.completion.as_ref().map_or(0, |c| c.items.len());
                    if count > 0 {
                        let selected = state.completion.as_mut().unwrap().selected;
                        state.completion.as_mut().unwrap().selected = (selected + 1).min(count - 1);
                    }
                    return true;
                }
                NamedKey::ArrowUp => {
                    let selected = state.completion.as_ref().map_or(0, |c| c.selected);
                    if selected > 0 {
                        state.completion.as_mut().unwrap().selected = selected - 1;
                    }
                    return true;
                }
                NamedKey::Enter | NamedKey::Tab => {
                    let item = state
                        .completion
                        .as_ref()
                        .and_then(|c| c.items.get(c.selected).map(|item| item.clone()));
                    if let Some(item) = item {
                        state.caret = state.core.apply_completion(&item);
                        state.selection_anchor = None;
                        state.completion = None;
                        mark_edit(state, now);
                        let gutter = editor_gutter_width(
                            &state.declaration,
                            state.core.buffer().line_count(),
                            state.font_scale,
                        );
                        scroll_caret_into_view(state, font, editor_bounds, gutter);
                        return true;
                    }
                    state.completion = None;
                }
                _ => {}
            }
        }

        match named {
            NamedKey::Escape => {
                // Blur + commit; focus loss is handled by the caller
                // (the runtime clears `focused_editor` after this returns).
                commit_now(state, &path, &mut self.editor_pending_commits);
                state.focus = false;
                self.focused_editor = None;
                self.editor_selection_drag = false;
                true
            }
            NamedKey::Space => {
                // winit reports a bare space as NamedKey::Space (only
                // Ctrl+Space arrives as Character(" ")), so without this
                // branch the key falls through unhandled and is lost.
                insert_text(state, " ", now);
                true
            }
            NamedKey::Enter => {
                let line_text = state
                    .core
                    .buffer()
                    .line(state.caret.line)
                    .unwrap_or_default();
                let indent: String = line_text
                    .chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .collect();
                insert_text(state, &format!("\n{indent}"), now);
                true
            }
            NamedKey::Tab => {
                let spaces = " ".repeat(state.declaration.tab_size as usize);
                insert_text(state, &spaces, now);
                true
            }
            NamedKey::Backspace => {
                if ctrl {
                    let line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = state.caret.column as usize;
                    while column > 0 && chars.get(column - 1).is_some_and(|c| c.is_whitespace()) {
                        column -= 1;
                    }
                    while column > 0 && chars.get(column - 1).is_some_and(|c| !c.is_whitespace()) {
                        column -= 1;
                    }
                    let start = Position::new(state.caret.line, column as u32);
                    state.caret = state.core.delete(start, state.caret);
                    state.completion = None;
                    mark_edit(state, now);
                } else {
                    delete_backward(state, now);
                }
                auto_complete(state);
                true
            }
            NamedKey::Delete => {
                if ctrl {
                    let line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = state.caret.column as usize;
                    let len = chars.len();
                    while column < len && chars.get(column).is_some_and(|c| !c.is_whitespace()) {
                        column += 1;
                    }
                    while column < len && chars.get(column).is_some_and(|c| c.is_whitespace()) {
                        column += 1;
                    }
                    let end = Position::new(state.caret.line, column as u32);
                    state.caret = state.core.delete(state.caret, end);
                    state.completion = None;
                    mark_edit(state, now);
                } else {
                    delete_forward(state, now);
                }
                auto_complete(state);
                true
            }
            NamedKey::ArrowLeft => {
                if ctrl {
                    let line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = state.caret.column as usize;
                    while column > 0 && chars.get(column - 1).is_some_and(|c| c.is_whitespace()) {
                        column -= 1;
                    }
                    while column > 0 && chars.get(column - 1).is_some_and(|c| !c.is_whitespace()) {
                        column -= 1;
                    }
                    move_caret(state, Position::new(state.caret.line, column as u32), shift);
                } else if state.caret.column > 0 {
                    move_caret(
                        state,
                        Position::new(state.caret.line, state.caret.column - 1),
                        shift,
                    );
                } else if state.caret.line > 0 {
                    let previous_len = state
                        .core
                        .buffer()
                        .line(state.caret.line - 1)
                        .map_or(0, |line| line.chars().count() as u32);
                    move_caret(
                        state,
                        Position::new(state.caret.line - 1, previous_len),
                        shift,
                    );
                }
                true
            }
            NamedKey::ArrowRight => {
                let line_len = state
                    .core
                    .buffer()
                    .line(state.caret.line)
                    .map_or(0, |line| line.chars().count() as u32);
                if ctrl {
                    let line = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = state.caret.column as usize;
                    let len = chars.len();
                    while column < len && chars.get(column).is_some_and(|c| !c.is_whitespace()) {
                        column += 1;
                    }
                    while column < len && chars.get(column).is_some_and(|c| c.is_whitespace()) {
                        column += 1;
                    }
                    move_caret(state, Position::new(state.caret.line, column as u32), shift);
                } else if state.caret.column < line_len {
                    move_caret(
                        state,
                        Position::new(state.caret.line, state.caret.column + 1),
                        shift,
                    );
                } else if state.caret.line + 1 < state.core.buffer().line_count() {
                    move_caret(state, Position::new(state.caret.line + 1, 0), shift);
                }
                true
            }
            NamedKey::ArrowUp => {
                let previous_len = state
                    .core
                    .buffer()
                    .line(state.caret.line.saturating_sub(1))
                    .map_or(0, |line| line.chars().count() as u32);
                let line = state.caret.line.saturating_sub(1);
                move_caret(
                    state,
                    Position::new(line, state.caret.column.min(previous_len)),
                    shift,
                );
                true
            }
            NamedKey::ArrowDown => {
                let next_len = state
                    .core
                    .buffer()
                    .line(state.caret.line.saturating_add(1))
                    .map_or(0, |line| line.chars().count() as u32);
                let line = state
                    .caret
                    .line
                    .saturating_add(1)
                    .min(state.core.buffer().line_count().saturating_sub(1));
                move_caret(
                    state,
                    Position::new(line, state.caret.column.min(next_len)),
                    shift,
                );
                true
            }
            NamedKey::Home => {
                let to = if ctrl {
                    Position::START
                } else {
                    Position::new(state.caret.line, 0)
                };
                move_caret(state, to, shift);
                true
            }
            NamedKey::End => {
                let to = if ctrl {
                    let last = state.core.buffer().line_count().saturating_sub(1);
                    let len = state
                        .core
                        .buffer()
                        .line(last)
                        .map_or(0, |line| line.chars().count() as u32);
                    Position::new(last, len)
                } else {
                    let len = state
                        .core
                        .buffer()
                        .line(state.caret.line)
                        .map_or(0, |line| line.chars().count() as u32);
                    Position::new(state.caret.line, len)
                };
                move_caret(state, to, shift);
                true
            }
            NamedKey::PageUp => {
                let rows = (editor_height / editor_row_height(font, &state.declaration, state.font_scale).max(1.0))
                    .floor() as u32;
                let line = state.caret.line.saturating_sub(rows.max(1));
                move_caret(state, Position::new(line, state.caret.column), shift);
                true
            }
            NamedKey::PageDown => {
                let rows = (editor_height / editor_row_height(font, &state.declaration, state.font_scale).max(1.0))
                    .floor() as u32;
                let line = state
                    .caret
                    .line
                    .saturating_add(rows.max(1))
                    .min(state.core.buffer().line_count().saturating_sub(1));
                move_caret(state, Position::new(line, state.caret.column), shift);
                true
            }
            NamedKey::Space if ctrl => {
                open_completion(state);
                true
            }
            _ => false,
        };

        if consumed {
            let gutter = editor_gutter_width(&state.declaration, state.core.buffer().line_count(), state.font_scale);
            scroll_caret_into_view(state, font, editor_bounds, gutter);
        }
        consumed
    }

    /// IME preedit for the focused editor (local presentation at the caret).
    pub(crate) fn editor_ime_preedit(&mut self, value: &str) {
        if let Some(path) = self.focused_editor.clone()
            && let Some(state) = self.editors.get_mut(&path)
        {
            state.preedit = value.to_string();
        }
    }

    /// IME commit for the focused editor. Returns whether consumed.
    pub(crate) fn editor_ime_commit(&mut self, value: &str) -> bool {
        let Some(path) = self.focused_editor.clone() else {
            return false;
        };
        let Some(state) = self.editors.get_mut(&path) else {
            return false;
        };
        state.preedit.clear();
        if value.is_empty() {
            return true;
        }
        if let Some(anchor) = state.selection_anchor {
            let (start, end) = ordered_selection(anchor, state.caret);
            let deleted = state.core.delete(start, end);
            state.selection_anchor = None;
            state.caret = state.core.insert(deleted, value);
        } else {
            state.caret = state.core.insert(state.caret, value);
        }
        state.completion = None;
        state.pending_edits = true;
        state.last_edit_seconds = self.animation_clock_seconds;
        true
    }

    /// Blurs the focused editor, committing any pending edits.
    pub(crate) fn blur_editor(&mut self) {
        self.commit_focused_editor();
        if let Some(path) = self.focused_editor.clone()
            && let Some(state) = self.editors.get_mut(&path)
        {
            state.focus = false;
            state.completion = None;
            state.preedit.clear();
        }
        self.focused_editor = None;
        self.editor_selection_drag = false;
    }

    /// Takes queued editor commits (one per blurred/explicit-save editor).
    pub(crate) fn take_editor_commits(&mut self) -> Vec<EditorCommit> {
        std::mem::take(&mut self.editor_pending_commits)
    }

    fn commit_focused_editor(&mut self) {
        let Some(path) = self.focused_editor.clone() else {
            return;
        };
        let Some(state) = self.editors.get_mut(&path) else {
            return;
        };
        let pending = state.pending_edits;
        state.pending_edits = false;
        if !pending {
            return;
        }
        state.completion = None;
        state.preedit.clear();
        let document = state.core.buffer().text();
        // Move the edit-session baseline forward so the next ChangeSet is
        // incremental; the host is the final writer of the document frame.
        state.core.commit();
        self.editor_pending_commits.push(EditorCommit {
            node_path: path.clone(),
            event_action: state.event_action.clone(),
            document,
        });
    }
}

/// Extracts `start..end` text from the editor buffer (for clipboard ops).
fn extract_range(state: &EditorRuntimeState, start: Position, end: Position) -> String {
    let start = state.core.buffer().clamp_position(start);
    let end = state.core.buffer().clamp_position(end);
    if end <= start {
        return String::new();
    }
    let mut text = String::new();
    for line in start.line..=end.line {
        let line_text = state.core.buffer().line(line).unwrap_or_default();
        let chars: Vec<char> = line_text.chars().collect();
        let from = if line == start.line {
            start.column as usize
        } else {
            0
        };
        let to = if line == end.line {
            end.column as usize
        } else {
            chars.len()
        };
        if line > start.line {
            text.push('\n');
        }
        text.extend(chars[from.min(chars.len())..to.min(chars.len())].iter());
    }
    text
}
