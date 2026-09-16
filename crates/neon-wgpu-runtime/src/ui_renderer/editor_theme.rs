//! Editor visual theme: syntax defaults + editor chrome colors.
//!
//! This is the SDK-facing theming surface for the code editor. Construct
//! [`EditorTheme`] directly (e.g. [`EditorTheme::one_dark`]), or build it
//! from a NUI declaration with [`editor_theme_from`], which folds the
//! `syntax <class> <hex>` and `ui_color <name> <hex>` declaration overrides
//! on top of the defaults.
//!
//! Static syntax highlighting is always color-based (theme colors); text
//! materials (`token_shader`, `selection_shader`, whole-node `text_material`)
//! are reserved for transient / emphasis effects.

use neon_ui_schema::UiCodeEditorDeclaration;

/// Stable token-class names produced by the editor-core highlighter
/// (mirrors `neon_editor::TokenClass::name`).
const CLASS_KEYWORD: &str = "Keyword";
const CLASS_NODE_KIND: &str = "NodeKind";
const CLASS_NODE_KEY: &str = "NodeKey";
const CLASS_ATTRIBUTE: &str = "Attribute";
const CLASS_INPUT_REF: &str = "InputRef";
const CLASS_COLOR_LITERAL: &str = "ColorLiteral";
const CLASS_NUMERIC_LITERAL: &str = "NumericLiteral";
const CLASS_STRING_LITERAL: &str = "StringLiteral";
const CLASS_INTENT: &str = "Intent";
const CLASS_COMMENT: &str = "Comment";
const CLASS_IDENT: &str = "Ident";

/// One token class's default One Dark color (RGB 0..1).
fn one_dark_token(class: &str) -> [f32; 3] {
    match class {
        CLASS_KEYWORD => [0.78, 0.55, 0.91],        // #C678DD
        CLASS_NODE_KIND => [0.31, 0.76, 1.0],       // #4FC1FF
        CLASS_NODE_KEY => [0.90, 0.75, 0.48],       // #E5C07B
        CLASS_ATTRIBUTE => [0.34, 0.71, 0.76],      // #56B6C2
        CLASS_INPUT_REF => [0.38, 0.69, 0.94],      // #61AFEF
        CLASS_COLOR_LITERAL => [0.82, 0.60, 0.40],  // #D19A66
        CLASS_NUMERIC_LITERAL => [0.82, 0.60, 0.40],
        CLASS_STRING_LITERAL => [0.60, 0.76, 0.47], // #98C379
        CLASS_INTENT => [0.78, 0.47, 0.87],         // #C678DD
        CLASS_COMMENT => [0.36, 0.39, 0.44],        // #5C6370
        CLASS_IDENT => [0.67, 0.70, 0.75],          // #ABB2BF
        _ => [0.67, 0.70, 0.75],
    }
}

/// Complete visual theme for a code editor.
///
/// Fields are RGBA 0..1. `syntax` holds per-class overrides keyed by the
/// stable token-class name (`TokenClass::name`); classes absent from the map
/// fall back to the built-in One Dark defaults via [`EditorTheme::token`].
#[derive(Clone, Debug)]
pub struct EditorTheme {
    /// Ordinary code text (whitespace / unclassified glyphs).
    pub text: [f32; 4],
    /// Line-number gutter, non-current rows.
    pub line_number: [f32; 4],
    /// Line-number gutter, current row.
    pub line_number_current: [f32; 4],
    /// Selection fill.
    pub selection: [f32; 4],
    /// Current-line highlight fill.
    pub current_line: [f32; 4],
    /// Caret.
    pub caret: [f32; 4],
    /// Completion popup background.
    pub popup_background: [f32; 4],
    /// Completion popup selected-item highlight.
    pub popup_selection: [f32; 4],
    /// Per-token-class color overrides (class name -> RGBA).
    pub syntax: std::collections::BTreeMap<String, [f32; 4]>,
}

/// UI color override keys understood by [`EditorTheme::with_ui`] and the NUI
/// `ui_color <name> <hex>` clause.
pub const UI_COLOR_KEYS: &[&str] = &[
    "text",
    "line_number",
    "line_number_current",
    "selection",
    "current_line",
    "caret",
    "popup_background",
    "popup_selection",
];

impl EditorTheme {
    /// The default high-contrast dark theme (One Dark base palette).
    pub fn one_dark() -> Self {
        Self {
            text: [0.75, 0.78, 0.84, 1.0],
            line_number: [0.36, 0.39, 0.44, 1.0],
            line_number_current: [0.72, 0.76, 0.83, 1.0],
            selection: [0.15, 0.31, 0.47, 0.55],
            current_line: [1.0, 1.0, 1.0, 0.035],
            caret: [1.0, 1.0, 1.0, 1.0],
            popup_background: [0.118, 0.141, 0.188, 0.97],
            popup_selection: [0.20, 0.30, 0.44, 0.85],
            syntax: std::collections::BTreeMap::new(),
        }
    }

    /// Add a per-token-class color override (e.g. `"Keyword"`, `"StringLiteral"`).
    pub fn with_syntax(mut self, class: &str, color: [f32; 4]) -> Self {
        self.syntax.insert(class.to_string(), color);
        self
    }

    /// Override one chrome color by [`UI_COLOR_KEYS`] name. Unknown names are
    /// ignored so SDK callers can forward declaration keys verbatim.
    pub fn with_ui(mut self, name: &str, color: [f32; 4]) -> Self {
        match name {
            "text" => self.text = color,
            "line_number" => self.line_number = color,
            "line_number_current" => self.line_number_current = color,
            "selection" => self.selection = color,
            "current_line" => self.current_line = color,
            "caret" => self.caret = color,
            "popup_background" => self.popup_background = color,
            "popup_selection" => self.popup_selection = color,
            _ => {}
        }
        self
    }

    /// Color for a token class at a given opacity (override wins over the
    /// built-in One Dark default).
    pub fn token(&self, class: &str, opacity: f32) -> [f32; 4] {
        if let Some(c) = self.syntax.get(class) {
            return [c[0], c[1], c[2], c[3] * opacity];
        }
        let rgb = one_dark_token(class);
        [rgb[0], rgb[1], rgb[2], opacity]
    }
}

/// Build the effective theme for an editor declaration: One Dark defaults
/// folded with `syntax_colors` and `ui_colors` overrides.
pub fn editor_theme_from(declaration: &UiCodeEditorDeclaration) -> EditorTheme {
    let mut theme = EditorTheme::one_dark();
    for (class, color) in &declaration.syntax_colors {
        theme.syntax.insert(class.clone(), *color);
    }
    for (name, color) in &declaration.ui_colors {
        theme = theme.with_ui(name, *color);
    }
    theme
}
