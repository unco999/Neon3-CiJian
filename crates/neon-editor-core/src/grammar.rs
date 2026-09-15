//! Grammar description consumed by the highlighter and completion engine.
//!
//! The kernel is grammar-agnostic: a [`FlowGrammar`] is plain data. The
//! built-in NUI Flow table below is provisional and duplicated on purpose for
//! now; the plan (`docs/nui-flow-code-editor.md` slice 3) replaces it with the
//! single table extracted into `neon-ui-schema` so the Flow parser,
//! formatter, and this kernel share one source of truth.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowGrammar {
    /// Top-level statement keywords (`input`, `surface`, `machine`, ...).
    pub keywords: Vec<&'static str>,
    /// Declarable node kinds (`panel`, `text`, `button`, ...).
    pub node_kinds: Vec<&'static str>,
    /// Attributes valid on every node (`w`, `h`, `fill`, ...).
    pub common_attributes: Vec<&'static str>,
    /// Per-node-kind attributes beyond `common_attributes`.
    pub node_attributes: BTreeMap<&'static str, Vec<&'static str>>,
    /// Input kinds accepted after `input <key>`.
    pub input_kinds: Vec<&'static str>,
}

impl FlowGrammar {
    pub fn is_keyword(&self, token: &str) -> bool {
        self.keywords.binary_search(&token).is_ok()
    }

    pub fn is_node_kind(&self, token: &str) -> bool {
        self.node_kinds.binary_search(&token).is_ok()
    }

    /// Attribute names valid for a node kind, sorted and deduplicated.
    pub fn attributes_for(&self, node_kind: &str) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.common_attributes.clone();
        if let Some(extra) = self.node_attributes.get(node_kind) {
            names.extend_from_slice(extra);
        }
        names.sort_unstable();
        names.dedup();
        names
    }

    pub fn is_attribute_of(&self, node_kind: &str, token: &str) -> bool {
        if self.common_attributes.binary_search(&token).is_ok() {
            return true;
        }
        self.node_attributes
            .get(node_kind)
            .is_some_and(|extra| extra.binary_search(&token).is_ok())
    }
}

/// Provisional NUI Flow grammar. Attribute lists are intentionally partial;
/// slice 3 swaps this for the extracted `neon-ui-schema` table.
pub fn nui_flow_default() -> FlowGrammar {
    let mut keywords = vec![
        "branch",
        "emitevent",
        "input",
        "keyframe",
        "machine",
        "motion",
        "on",
        "repeat",
        "resource",
        "state",
        "surface",
        "sync",
        "template",
        "text_style",
    ];
    keywords.sort_unstable();
    let mut node_kinds = vec![
        "button",
        "canvas",
        "checkbox",
        "code_editor",
        "combo",
        "data_grid",
        "drag_value",
        "dropdown",
        "image",
        "list_box",
        "panel",
        "progress_bar",
        "radio_button",
        "render",
        "scroll",
        "selectable",
        "slider",
        "tabs",
        "text",
        "world",
    ];
    node_kinds.sort_unstable();
    let mut common = vec![
        "align",
        "border_width",
        "clip",
        "enabled",
        "fill",
        "grow",
        "gap",
        "h",
        "line",
        "opacity",
        "pad",
        "radius",
        "visible",
        "w",
    ];
    common.sort_unstable();
    let mut node_attributes: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();
    node_attributes.insert("text", vec!["value", "rich"]);
    node_attributes.insert(
        "button",
        vec!["value", "event", "checked", "numeric", "state"],
    );
    node_attributes.insert(
        "data_grid",
        vec!["capacity", "columns", "overscan", "row_height", "source"],
    );
    node_attributes.insert(
        "code_editor",
        vec![
            "completions",
            "event",
            "font_size",
            "gutter_diagnostics",
            "language",
            "line_numbers",
            "read_only",
            "source",
            "tab_size",
            "wrap",
        ],
    );
    let input_kinds = vec![
        "asset_handle",
        "bool",
        "canvas_data",
        "color",
        "f32",
        "grid",
        "i32",
        "text",
        "u32",
    ];
    FlowGrammar {
        keywords,
        node_kinds,
        common_attributes: common,
        node_attributes,
        input_kinds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_lookup_merges_common_and_node_specific() {
        let grammar = nui_flow_default();
        let attributes = grammar.attributes_for("data_grid");
        assert!(attributes.contains(&"w"));
        assert!(attributes.contains(&"capacity"));
        assert!(!attributes.contains(&"value"));
    }

    #[test]
    fn tables_are_sorted_for_binary_search() {
        let grammar = nui_flow_default();
        let mut sorted = grammar.keywords.clone();
        sorted.sort_unstable();
        assert_eq!(grammar.keywords, sorted);
        let mut sorted = grammar.node_kinds.clone();
        sorted.sort_unstable();
        assert_eq!(grammar.node_kinds, sorted);
        let mut sorted = grammar.common_attributes.clone();
        sorted.sort_unstable();
        assert_eq!(grammar.common_attributes, sorted);
    }
}
