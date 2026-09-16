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

use neon_editor_core::TokenClass;
use neon_ui_schema::UiCodeEditorDeclaration;

/// One token class's default One Dark color (RGB 0..1).
fn one_dark_token(class: TokenClass) -> [f32; 3] {
    match class {
        TokenClass::Keyword => [0.78, 0.55, 0.91],      // #C678DD
        TokenClass::NodeKind => [0.31, 0.76, 1.0],      // #4FC1FF
        TokenClass::NodeKey => [0.90, 0.75, 0.48],      // #E5C07B
        TokenClass::Attribute => [0.34, 0.71, 0.76],    // #56B6C2
        TokenClass::InputRef => [0.38, 0.69, 0.94],     // #61AFEF
        TokenClass::ColorLiteral => [0.82, 0.60, 0.40], // #D19A66
        TokenClass::NumericLiteral => [0.82, 0.60, 0.40],
        TokenClass::StringLiteral => [0.60, 0.76, 0.47], // #98C379
        TokenClass::Intent => [0.78, 0.47, 0.87],       // #C678DD
        TokenClass::Comment => [0.36, 0.39, 0.44],      // #5C6370
        TokenClass::Ident => [0.67, 0.70, 0.75],        // #ABB2BF
    }
}

/// Complete visual theme for a code editor.
///
/// Fields are RGBA 0..1. `syntax` holds per-class overrides keyed by
/// [`TokenClass::name`]; classes absent from the map fall back to the built-in
/// One Dark defaults via [`EditorTheme::token`].
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
    /// Per-token-class color overrides (`TokenClass::name()` -> RGBA).
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
    pub fn token(&self, class: TokenClass, opacity: f32) -> [f32; 4] {
        if let Some(c) = self.syntax.get(class.name()) {
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
