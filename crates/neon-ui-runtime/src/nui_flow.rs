//! Closed, line-oriented NUI Flow authoring notation.
//!
//! Flow is deliberately parsed into the canonical JSON IR. It has no evaluator,
//! expressions, callbacks, or source of domain truth.

use std::collections::{BTreeMap, HashSet};

use neon_protocol::Revision;
use neon_ui_schema::{
    NuiFlowDocument, NuiFlowParseDiagnostic, NuiSourceSpan, TextRef, UiAlignItems, UiBoundProperty,
    UiBounds, UiDiagnosticSeverity, UiInputKind, UiInputPacking, UiInputSchema, UiInputSlot,
    UiInputUpdateClass, UiInputValue, UiIrBinding, UiIrDocument, UiIrPatch, UiIrPatchOperation,
    UiIrPatchOperationKind, UiJustifyContent, UiLayout, UiLayoutMode, UiNode, UiNodeId, UiNodeKind,
    RenderSurfaceRef, UiProgramEventDeclaration, UiResourceBudget, UiSourceSpan, UiStyle,
    UiSurfaceId, UiProgram, UiProgramRevision,
};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NuiFlowError {
    pub diagnostics: Vec<NuiFlowParseDiagnostic>,
}

type FlowResult<T> = Result<T, NuiFlowError>;

pub fn parse_nui_flow(source: &str) -> FlowResult<NuiFlowDocument> {
    let mut header = Header::default();
    let mut stack: Vec<(usize, NodeBuild)> = Vec::new();
    let mut root: Option<NodeBuild> = None;
    let mut bindings = Vec::new();
    let mut events = Vec::new();
    let mut source_map = BTreeMap::new();
    let mut input_slots = Vec::new();
    let mut seen_inputs = HashSet::new();

    for (index, raw) in source.lines().enumerate() {
        let line = (index + 1) as u32;
        if raw.contains('\t') {
            return Err(error(
                "nui_flow_tabs_forbidden",
                "tabs are forbidden; use two-space indentation",
                line,
                1,
            ));
        }
        let without_comment = if raw.trim_start().starts_with('#') {
            ""
        } else {
            raw
        };
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment.len() - without_comment.trim_start().len();
        if indent % 2 != 0 {
            return Err(error(
                "nui_flow_mixed_indentation",
                "indentation must use whole two-space levels",
                line,
                1,
            ));
        }
        let content = without_comment.trim();
        reject_forbidden(content, line)?;
        if indent == 0 {
            if let Some(slot) = parse_input(content, line)? {
                if !seen_inputs.insert(slot.key.clone()) {
                    return Err(error(
                        "ui_program_duplicate_input_key",
                        "input keys must be unique",
                        line,
                        1,
                    ));
                }
                input_slots.push(slot);
                continue;
            }
            if parse_header(content, &mut header, line)? {
                continue;
            }
        }
        let node = parse_node(content, line)?;
        if let Some(previous) = stack.last() {
            if indent > previous.0 + 2 {
                return Err(error(
                    "nui_flow_invalid_indentation",
                    "a child may be indented by one level only",
                    line,
                    1,
                ));
            }
        }
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            let (_, complete) = stack.pop().expect("stack checked");
            attach(complete, &mut stack, &mut root, line)?;
        }
        if indent > 0 && stack.is_empty() {
            return Err(error(
                "nui_flow_orphan_node",
                "indented node has no parent",
                line,
                1,
            ));
        }
        if source_map
            .insert(node.node.node_id.0.clone(), span(line, (indent + 1) as u32, content))
            .is_some()
        {
            return Err(error(
                "ui_ir_duplicate_key",
                "node keys must be unique across the Flow document",
                line,
                1,
            ));
        }
        for (property, input_key) in &node.bindings {
            bindings.push(UiIrBinding {
                node_key: node.node.node_id.0.clone(),
                input_key: input_key.clone(),
                property: property.clone(),
            });
        }
        for intent in &node.intents {
            events.push(UiProgramEventDeclaration {
                node_key: node.node.node_id.0.clone(),
                intent: intent.clone(),
                allowed_payload_keys: Vec::new(),
            });
        }
        stack.push((indent, node));
    }
    while let Some((_, node)) = stack.pop() {
        attach(node, &mut stack, &mut root, 0)?;
    }
    let root = root.ok_or_else(|| {
        error(
            "nui_flow_missing_root",
            "Flow requires exactly one root surface",
            1,
            1,
        )
    })?;
    if header.surface_id.is_empty() {
        header.surface_id = format!("surface.{}", root.node.node_id.0);
    }
    let mut offset = 0;
    for slot in &mut input_slots {
        offset = align_up(offset, slot.packing.alignment);
        slot.packing.offset = offset;
        offset += u32::from(slot.packing.lanes) * 4;
    }
    let schema = UiInputSchema {
        schema_id: format!("{}.inputs", header.surface_id),
        version: 1,
        layout_hash: "nui-flow-v1".into(),
        slots: input_slots,
    };
    schema.validate().map_err(|_| {
        error(
            "ui_program_invalid_input_schema",
            "Flow input declaration is not a valid input schema",
            1,
            1,
        )
    })?;
    for binding in &bindings {
        let binding_span = source_map
            .get(&binding.node_key)
            .expect("Flow node keys populate the source map");
        let slot = schema.slots.iter().find(|slot| slot.key == binding.input_key).ok_or_else(|| {
            error_at(
                "ui_program_unknown_binding_target",
                "binding references an input that is not declared by this Flow document",
                binding_span,
            )
        })?;
        if !binding_accepts(&binding.property, &slot.kind) {
            return Err(error_at(
                "ui_program_input_type_mismatch",
                "binding property is incompatible with its declared input kind",
                binding_span,
            ));
        }
    }
    let ir = UiIrDocument {
        schema_version: 1,
        surface_id: UiSurfaceId(header.surface_id),
        revision: Revision(header.revision),
        root: root.node,
        bindings,
        events,
        resources: Vec::new(),
        resource_budget: header.budget,
    };
    ir.validate().map_err(|_| {
        error(
            "nui_flow_invalid_ir",
            "Flow lowering produced an invalid UI IR document",
            1,
            1,
        )
    })?;
    Ok(NuiFlowDocument {
        version: 1,
        source: source.into(),
        source_map,
        ir,
        input_schema: schema,
    })
}

pub fn lower_nui_flow(document: &NuiFlowDocument) -> UiIrDocument {
    document.ir.clone()
}

/// Compiles Flow through the same portable IR compiler and restores the exact
/// authoring spans on program/debug records. No Flow-specific evaluator exists.
pub fn compile_nui_flow_program(
    document: &NuiFlowDocument,
    revision: UiProgramRevision,
) -> Result<UiProgram, crate::UiProgramCompileError> {
    let mut program = crate::compile_ui_program(&document.ir, revision, &document.input_schema)?;
    for node in &mut program.nodes {
        if let Some(span) = document.source_map.get(&node.key) {
            let source_span = UiSourceSpan {
                source_id: "nui-flow".into(),
                line: span.line,
                column: span.column,
                end_line: span.end_line,
                end_column: span.end_column,
            };
            node.source_span = Some(source_span.clone());
            program
                .dependency_index
                .node_to_source_span
                .insert(node.key.clone(), Some(source_span));
        }
    }
    Ok(program)
}

/// Canonical formatter is intentionally conservative: it normalizes the
/// supported subset while preserving the source-free canonical IR semantics.
pub fn format_nui_flow(source: &str) -> FlowResult<String> {
    let parsed = parse_nui_flow(source)?;
    let mut lines = vec![
        "version 1".into(),
        format!(
            "surface {} revision {}",
            parsed.ir.surface_id.0, parsed.ir.revision.0
        ),
    ];
    for slot in &parsed.input_schema.slots {
        lines.push(format_input(slot));
    }
    format_node(
        &parsed.ir.root,
        0,
        &parsed.ir.bindings,
        &parsed.ir.events,
        &mut lines,
    );
    Ok(lines.join("\n") + "\n")
}

pub fn parse_nui_flow_patch(source: &str) -> FlowResult<UiIrPatch> {
    let mut expected = None;
    let mut operations = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let line = (index + 1) as u32;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        reject_forbidden(text, line)?;
        let parts = text.split_whitespace().collect::<Vec<_>>();
        match parts.first().copied() {
            Some("@") if parts.len() == 3 && parts[1] == "revision" => {
                expected = Some(parse_u64(parts[2], line, "revision")?)
            }
            Some("+") if parts.len() == 4 => operations.push(UiIrPatchOperation {
                kind: UiIrPatchOperationKind::Insert,
                target_path: parts[1].into(),
                expected_revision: Revision(expected.unwrap_or(0)),
                payload: Some(json!({"kind":parts[2], "key":parts[3]})),
                source_span: span(line, 1, text),
            }),
            Some("-") if parts.len() == 2 => operations.push(UiIrPatchOperation {
                kind: UiIrPatchOperationKind::Remove,
                target_path: parts[1].into(),
                expected_revision: Revision(expected.unwrap_or(0)),
                payload: None,
                source_span: span(line, 1, text),
            }),
            Some("~") if parts.len() >= 4 => operations.push(UiIrPatchOperation {
                kind: UiIrPatchOperationKind::Set,
                target_path: parts[1].into(),
                expected_revision: Revision(expected.unwrap_or(0)),
                payload: Some(json!({"property":parts[2], "value":parts[3..].join(" ")})),
                source_span: span(line, 1, text),
            }),
            Some(">") if parts.len() == 3 => operations.push(UiIrPatchOperation {
                kind: UiIrPatchOperationKind::Move,
                target_path: parts[1].into(),
                expected_revision: Revision(expected.unwrap_or(0)),
                payload: Some(json!({"parent":parts[2]})),
                source_span: span(line, 1, text),
            }),
            _ => {
                return Err(error(
                    "nui_flow_invalid_patch",
                    "expected @ revision, +, -, ~, or > patch operation",
                    line,
                    1,
                ))
            }
        }
    }
    let expected = expected.ok_or_else(|| {
        error(
            "nui_flow_missing_patch_revision",
            "patch requires '@ revision <number>'",
            1,
            1,
        )
    })?;
    for operation in &mut operations {
        operation.expected_revision = Revision(expected);
    }
    Ok(UiIrPatch {
        expected_revision: Revision(expected),
        operations,
    })
}

/// Applies only bounded canonical changes. This is suitable for dry-run callers;
/// any topology update still yields a new revisioned IR document.
pub fn apply_nui_ir_patch(document: &UiIrDocument, patch: &UiIrPatch) -> FlowResult<UiIrDocument> {
    if document.revision != patch.expected_revision {
        return Err(error(
            "nui_flow_stale_patch_revision",
            "patch revision does not match the document",
            1,
            1,
        ));
    }
    let mut result = document.clone();
    for operation in &patch.operations {
        let path = StablePath::parse(&operation.target_path, &operation.source_span)?;
        if path.segments().len() > 1 && !path_exists(&result.root, path.segments()) {
            return Err(error_at(
                "nui_flow_unknown_patch_target",
                "patch semantic path does not exist",
                &operation.source_span,
            ));
        }
        match operation.kind {
            UiIrPatchOperationKind::Set => set_node(
                &mut result.root,
                path.last(),
                operation.payload.as_ref(),
                &operation.source_span,
            )?,
            UiIrPatchOperationKind::Remove => {
                if path.matches_root(&result.root.node_id.0) {
                    return Err(error_at(
                        "nui_flow_invalid_patch",
                        "the root node cannot be removed",
                        &operation.source_span,
                    ));
                }
                if !remove_node_at_path(&mut result.root, path.segments()) {
                    return Err(error_at(
                        "nui_flow_unknown_patch_target",
                        "patch target does not exist",
                        &operation.source_span,
                    ));
                }
            }
            UiIrPatchOperationKind::Insert => insert_node(
                &mut result.root,
                path.last(),
                operation.payload.as_ref(),
                &operation.source_span,
            )?,
            UiIrPatchOperationKind::Move => move_node(
                &mut result.root,
                path.last(),
                operation.payload.as_ref(),
                &operation.source_span,
            )?,
        }
    }
    result.revision = Revision(result.revision.0 + 1);
    result.validate().map_err(|_| {
        error(
            "nui_flow_invalid_patch",
            "patch result fails canonical IR validation",
            1,
            1,
        )
    })?;
    Ok(result)
}

struct Header {
    surface_id: String,
    revision: u64,
    budget: UiResourceBudget,
}

/// A patch address is a semantic key or slash-separated semantic key path.
/// Numeric/index addressing is deliberately rejected so a reordered sibling list
/// cannot retarget a persisted patch.
struct StablePath(Vec<String>);

impl StablePath {
    fn parse(value: &str, source_span: &NuiSourceSpan) -> FlowResult<Self> {
        let segments = value
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if segments.is_empty()
            || segments.iter().any(|segment| !valid_key(segment) || segment.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(error_at(
                "nui_flow_invalid_patch_path",
                "patch targets use stable semantic keys or paths, never array indexes",
                source_span,
            ));
        }
        Ok(Self(segments))
    }

    fn last(&self) -> &str { self.0.last().expect("validated path") }
    fn segments(&self) -> &[String] { &self.0 }
    fn matches_root(&self, root: &str) -> bool { self.0.len() == 1 && self.0[0] == root }
}

fn path_exists(root: &UiNode, segments: &[String]) -> bool {
    let segments = if segments.first().is_some_and(|segment| segment == &root.node_id.0) {
        &segments[1..]
    } else {
        return false;
    };
    let mut node = root;
    for segment in segments {
        let Some(child) = node.children.iter().find(|child| child.node_id.0 == *segment) else {
            return false;
        };
        node = child;
    }
    true
}
impl Default for Header {
    fn default() -> Self {
        Self {
            surface_id: String::new(),
            revision: 1,
            budget: UiResourceBudget {
                max_nodes: 512,
                max_bindings: 512,
                max_instances: 512,
                max_text_records: 512,
                max_glyph_instances: 8192,
                max_events: 512,
                max_clips: 128,
            },
        }
    }
}
struct NodeBuild {
    node: UiNode,
    bindings: Vec<(UiBoundProperty, String)>,
    intents: Vec<String>,
}

fn parse_header(text: &str, header: &mut Header, line: u32) -> FlowResult<bool> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    match parts.first().copied() {
        Some("version") if parts.len() == 2 && parts[1] == "1" => Ok(true),
        Some("surface") if parts.len() == 4 && parts[2] == "revision" => {
            header.surface_id = parts[1].into();
            header.revision = parse_u64(parts[3], line, "revision")?;
            Ok(true)
        }
        Some("budget") => {
            for pair in &parts[1..] {
                let Some((key, value)) = pair.split_once('=') else {
                    return Err(error(
                        "nui_flow_invalid_budget",
                        "budget values use key=value",
                        line,
                        1,
                    ));
                };
                let value = parse_u64(value, line, key)? as u32;
                match key {
                    "nodes" => header.budget.max_nodes = value,
                    "bindings" => header.budget.max_bindings = value,
                    "instances" => header.budget.max_instances = value,
                    "text" => header.budget.max_text_records = value,
                    "glyphs" => header.budget.max_glyph_instances = value,
                    "events" => header.budget.max_events = value,
                    "clips" => header.budget.max_clips = value,
                    _ => {
                        return Err(error(
                            "nui_flow_unknown_budget",
                            "unknown budget field",
                            line,
                            1,
                        ))
                    }
                };
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_input(text: &str, line: u32) -> FlowResult<Option<UiInputSlot>> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.first() != Some(&"input") {
        return Ok(None);
    }
    if parts.len() != 5 || parts[3] != "default" {
        return Err(error(
            "nui_flow_invalid_input",
            "input syntax is: input <key> <kind> default <value>",
            line,
            1,
        ));
    }
    let kind = match parts[2] {
        "bool" => UiInputKind::Bool,
        "i32" => UiInputKind::I32,
        "u32" => UiInputKind::U32,
        "f32" => UiInputKind::F32,
        "text" => UiInputKind::TextHandle,
        _ => {
            return Err(error(
                "nui_flow_unknown_input_kind",
                "Flow supports bool, i32, u32, f32, and text inputs",
                line,
                1,
            ))
        }
    };
    let value = match (parts[2], parts[4]) {
        ("bool", "true") => UiInputValue::Bool { value: true },
        ("bool", "false") => UiInputValue::Bool { value: false },
        ("i32", value) => UiInputValue::I32 {
            value: value
                .parse()
                .map_err(|_| error("nui_flow_invalid_literal", "invalid i32 default", line, 1))?,
        },
        ("u32", value) => UiInputValue::U32 {
            value: value
                .parse()
                .map_err(|_| error("nui_flow_invalid_literal", "invalid u32 default", line, 1))?,
        },
        ("f32", value) => UiInputValue::F32 {
            value: value
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| {
                    error(
                        "nui_flow_invalid_literal",
                        "invalid finite f32 default",
                        line,
                        1,
                    )
                })?,
        },
        ("text", "text:empty") => UiInputValue::TextHandle {
            value: neon_ui_schema::UiTextHandle {
                id: 0,
                generation: 0,
            },
        },
        _ => {
            return Err(error(
                "nui_flow_invalid_literal",
                "input default does not match its type",
                line,
                1,
            ))
        }
    };
    let (alignment, lanes, representation) = kind.packing();
    Ok(Some(UiInputSlot {
        key: parts[1].into(),
        kind,
        default_value: value,
        update_class: UiInputUpdateClass::ReliableExternal,
        semantic_label: parts[1].replace('_', " "),
        packing: UiInputPacking {
            alignment,
            lanes,
            offset: 0,
            representation,
        },
    }))
}

fn parse_node(text: &str, line: u32) -> FlowResult<NodeBuild> {
    let values = tokenize(text, line)?;
    let parts = values.iter().map(String::as_str).collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err(error(
            "nui_flow_invalid_node",
            "node syntax is: <component> <stable-key> [tokens]",
            line,
            1,
        ));
    }
    let kind = match parts[0] {
        "surface" | "panel" | "scroll" | "overlay" | "branch" | "repeat" | "template" => {
            UiNodeKind::Panel
        }
        "text" => UiNodeKind::Label,
        "button" => UiNodeKind::Button,
        "input" => UiNodeKind::TextInput,
        "image" => UiNodeKind::Image,
        "render" => UiNodeKind::RenderSurface,
        _ => {
            return Err(error(
                "nui_flow_unknown_component",
                "component is outside the closed Flow vocabulary",
                line,
                1,
            ))
        }
    };
    if !valid_key(parts[1]) {
        return Err(error(
            "nui_flow_invalid_key",
            "node keys use letters, digits, '.', '_' and '-'",
            line,
            1,
        ));
    }
    let is_render_surface = kind == UiNodeKind::RenderSurface;
    let mut node = UiNode {
        node_id: UiNodeId(parts[1].into()),
        kind,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        },
        layout: Some(UiLayout::default()),
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        children: Vec::new(),
    };
    if is_render_surface {
        node.surface = Some(RenderSurfaceRef {
            target_id: format!("render.{}", node.node_id.0),
        });
    }
    let mut bindings = Vec::new();
    let mut intents = Vec::new();
    let mut used = HashSet::new();
    let mut index = 2;
    while index < parts.len() {
        let token = parts[index];
        if !used.insert(token) && !matches!(token, "row" | "column" | "overlay") {
            return Err(error(
                "nui_flow_duplicate_attribute",
                "attribute appears more than once",
                line,
                1,
            ));
        }
        match token {
            "row" => node.layout.as_mut().unwrap().mode = UiLayoutMode::Row,
            "column" => node.layout.as_mut().unwrap().mode = UiLayoutMode::Column,
            "overlay" => node.layout.as_mut().unwrap().mode = UiLayoutMode::Overlay,
            "w" | "h" | "minw" | "maxw" | "grow" | "shrink" | "basis" | "gap" | "pad" | "fill"
            | "line" | "ink" | "value" | "enabled" | "visible" | "event" | "token" | "align"
            | "justify" => {
                let value = *parts.get(index + 1).ok_or_else(|| {
                    error(
                        "nui_flow_missing_value",
                        "attribute requires a value",
                        line,
                        1,
                    )
                })?;
                index += 1;
                parse_attribute(&mut node, &mut bindings, &mut intents, token, value, line)?;
            }
            _ => {
                return Err(error(
                    "nui_flow_unknown_attribute",
                    "unknown Flow layout, style, binding, or event token",
                    line,
                    1,
                ))
            }
        }
        index += 1;
    }
    Ok(NodeBuild {
        node,
        bindings,
        intents,
    })
}

fn parse_attribute(
    node: &mut UiNode,
    bindings: &mut Vec<(UiBoundProperty, String)>,
    intents: &mut Vec<String>,
    key: &str,
    value: &str,
    line: u32,
) -> FlowResult<()> {
    let layout = node.layout.as_mut().expect("Flow nodes have layout");
    let mut direct_binding = |property| {
        value
            .strip_prefix('$')
            .map(|input| bindings.push((property, input.into())))
    };
    match key {
        "w" => node.bounds.width = number(value, line)?,
        "h" => node.bounds.height = number(value, line)?,
        "minw" => {
            layout.min_size = Some([number(value, line)?, layout.min_size.map_or(0.0, |v| v[1])])
        }
        "maxw" => {
            layout.max_size = Some([
                number(value, line)?,
                layout.max_size.map_or(f32::MAX, |v| v[1]),
            ])
        }
        "grow" => layout.flex_grow = number(value, line)?,
        "shrink" => layout.flex_shrink = number(value, line)?,
        "basis" => layout.flex_basis = Some(number(value, line)?),
        "gap" => layout.gap = number(value, line)?,
        "pad" => layout.padding = [number(value, line)?; 4],
        "fill" => {
            node.style.background_color = color(value, line)?;
        }
        "line" => {
            node.style.border_color = color(value, line)?;
        }
        "ink" => {
            if !value.starts_with("token:") {
                return Err(error(
                    "nui_flow_invalid_style",
                    "ink requires a token:<name> reference",
                    line,
                    1,
                ));
            }
        }
        "token" => {
            if !value.starts_with("token:") {
                return Err(error(
                    "nui_flow_invalid_style",
                    "token requires token:<name>",
                    line,
                    1,
                ));
            }
        }
        "align" => layout.align_items = alignment(value, line)?,
        "justify" => layout.justify_content = justify(value, line)?,
        "value" => {
            if let Some(input) = direct_binding(UiBoundProperty::TextValue) {
                input
            } else {
                node.text = Some(TextRef::Literal {
                    value: quoted(value, line)?,
                });
            }
        }
        "enabled" => {
            if direct_binding(UiBoundProperty::Enabled).is_none() {
                node.enabled = boolean(value, line)?;
            }
        }
        "visible" => {
            if direct_binding(UiBoundProperty::Visible).is_none() {
                node.visible = boolean(value, line)?;
            }
        }
        "event" => {
            if !valid_intent(value) {
                return Err(error(
                    "nui_flow_invalid_intent",
                    "event intent must be a dotted semantic name",
                    line,
                    1,
                ));
            }
            intents.push(value.into());
        }
        _ => unreachable!("parse_node limits attributes"),
    };
    Ok(())
}

fn attach(
    mut child: NodeBuild,
    stack: &mut Vec<(usize, NodeBuild)>,
    root: &mut Option<NodeBuild>,
    line: u32,
) -> FlowResult<()> {
    if let Some((_, parent)) = stack.last_mut() {
        parent.node.children.push(child.node);
        Ok(())
    } else if root.is_none() {
        *root = Some(child);
        Ok(())
    } else {
        Err(error(
            "nui_flow_multiple_roots",
            "Flow permits one root surface",
            line,
            1,
        ))
    }
}
fn reject_forbidden(text: &str, line: u32) -> FlowResult<()> {
    if text.chars().any(|character| matches!(character, '{' | '}' | '[' | ']'))
        || text.contains("=>")
        || text.contains("function")
        || text.contains("http:")
        || text.contains("https:")
        || text.contains("=") && !text.starts_with("budget") && !text.starts_with("@")
    {
        Err(error(
            "ui_program_forbidden_flow_feature",
            "Flow does not permit code, expressions, URLs, brackets, or assignment",
            line,
            1,
        ))
    } else {
        Ok(())
    }
}
fn span(line: u32, column: u32, text: &str) -> NuiSourceSpan {
    NuiSourceSpan {
        line,
        column,
        end_line: line,
        end_column: column + text.chars().count() as u32,
    }
}
fn error(code: &str, message: &str, line: u32, column: u32) -> NuiFlowError {
    NuiFlowError {
        diagnostics: vec![NuiFlowParseDiagnostic {
            code: code.into(),
            severity: UiDiagnosticSeverity::Error,
            message: message.into(),
            span: NuiSourceSpan {
                line,
                column,
                end_line: line,
                end_column: column,
            },
            suggestion: None,
        }],
    }
}
fn error_at(code: &str, message: &str, span: &NuiSourceSpan) -> NuiFlowError {
    NuiFlowError {
        diagnostics: vec![NuiFlowParseDiagnostic {
            code: code.into(),
            severity: UiDiagnosticSeverity::Error,
            message: message.into(),
            span: span.clone(),
            suggestion: None,
        }],
    }
}
fn parse_u64(value: &str, line: u32, what: &str) -> FlowResult<u64> {
    value.parse().map_err(|_| {
        error(
            "nui_flow_invalid_literal",
            &format!("invalid {what}"),
            line,
            1,
        )
    })
}
fn number(value: &str, line: u32) -> FlowResult<f32> {
    value
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| {
            error(
                "nui_flow_invalid_layout",
                "layout values must be finite numbers",
                line,
                1,
            )
        })
}
fn boolean(value: &str, line: u32) -> FlowResult<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(error(
            "nui_flow_invalid_literal",
            "expected true or false",
            line,
            1,
        )),
    }
}
fn quoted(value: &str, line: u32) -> FlowResult<String> {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Ok(value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\n", "\n"))
    } else {
        Err(error(
            "nui_flow_unquoted_text",
            "literal text must be a single quoted token",
            line,
            1,
        ))
    }
}
fn color(value: &str, line: u32) -> FlowResult<[f32; 4]> {
    let value = value.strip_prefix('#').ok_or_else(|| {
        error(
            "nui_flow_invalid_color",
            "color must use #RRGGBB or #RRGGBBAA",
            line,
            1,
        )
    })?;
    if value.len() != 6 && value.len() != 8 {
        return Err(error(
            "nui_flow_invalid_color",
            "color must use #RRGGBB or #RRGGBBAA",
            line,
            1,
        ));
    }
    let byte = |start| {
        u8::from_str_radix(&value[start..start + 2], 16)
            .map(|v| v as f32 / 255.0)
            .map_err(|_| {
                error(
                    "nui_flow_invalid_color",
                    "color has invalid hex digits",
                    line,
                    1,
                )
            })
    };
    Ok([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if value.len() == 8 { byte(6)? } else { 1.0 },
    ])
}
fn alignment(value: &str, line: u32) -> FlowResult<UiAlignItems> {
    match value {
        "start" => Ok(UiAlignItems::Start),
        "center" => Ok(UiAlignItems::Center),
        "end" => Ok(UiAlignItems::End),
        "stretch" => Ok(UiAlignItems::Stretch),
        _ => Err(error(
            "nui_flow_invalid_layout",
            "invalid align value",
            line,
            1,
        )),
    }
}
fn justify(value: &str, line: u32) -> FlowResult<UiJustifyContent> {
    match value {
        "start" => Ok(UiJustifyContent::Start),
        "center" => Ok(UiJustifyContent::Center),
        "end" => Ok(UiJustifyContent::End),
        "between" => Ok(UiJustifyContent::SpaceBetween),
        "around" => Ok(UiJustifyContent::SpaceAround),
        "evenly" => Ok(UiJustifyContent::SpaceEvenly),
        _ => Err(error(
            "nui_flow_invalid_layout",
            "invalid justify value",
            line,
            1,
        )),
    }
}
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn valid_intent(intent: &str) -> bool {
    intent.contains('.') && intent.split('.').all(valid_key)
}
fn binding_accepts(property: &UiBoundProperty, kind: &UiInputKind) -> bool {
    match property {
        UiBoundProperty::TextValue => matches!(kind, UiInputKind::TextHandle),
        UiBoundProperty::Enabled | UiBoundProperty::Visible => matches!(kind, UiInputKind::Bool),
        _ => false,
    }
}
fn align_up(value: u32, alignment: u32) -> u32 { (value + alignment - 1) / alignment * alignment }
fn tokenize(text: &str, line: u32) -> FlowResult<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            current.push(match character {
                'n' => '\n',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            current.push(character);
            continue;
        }
        if !quoted && character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quoted {
        return Err(error(
            "nui_flow_unterminated_string",
            "quoted text has an invalid escape or missing closing quote",
            line,
            1,
        ));
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}
fn format_input(slot: &UiInputSlot) -> String {
    let kind = match &slot.kind {
        UiInputKind::Bool => "bool",
        UiInputKind::I32 => "i32",
        UiInputKind::U32 => "u32",
        UiInputKind::F32 => "f32",
        UiInputKind::TextHandle => "text",
        _ => "unsupported",
    };
    let value = match &slot.default_value {
        UiInputValue::Bool { value } => value.to_string(),
        UiInputValue::I32 { value } => value.to_string(),
        UiInputValue::U32 { value } => value.to_string(),
        UiInputValue::F32 { value } => value.to_string(),
        UiInputValue::TextHandle { .. } => "text:empty".into(),
        _ => "unsupported".into(),
    };
    format!("input {} {} default {}", slot.key, kind, value)
}
fn format_node(
    node: &UiNode,
    indent: usize,
    bindings: &[UiIrBinding],
    events: &[UiProgramEventDeclaration],
    lines: &mut Vec<String>,
) {
    let kind = match &node.kind {
        UiNodeKind::Panel => "panel",
        UiNodeKind::Label => "text",
        UiNodeKind::Button => "button",
        UiNodeKind::TextInput => "input",
        UiNodeKind::Image => "image",
        UiNodeKind::RenderSurface => "render",
    };
    let mut line = format!("{}{} {}", " ".repeat(indent), kind, node.node_id.0);
    if let Some(layout) = node.layout {
        match layout.mode {
            UiLayoutMode::Row => line.push_str(" row"),
            UiLayoutMode::Column => line.push_str(" column"),
            UiLayoutMode::Overlay => line.push_str(" overlay"),
            UiLayoutMode::Absolute => {}
        }
        if layout.gap != 0.0 {
            line.push_str(&format!(" gap {}", layout.gap));
        }
    }
    if let Some(TextRef::Literal { value }) = &node.text {
        line.push_str(&format!(" value \"{}\"", value.replace('"', "\\\"")));
    }
    for binding in bindings
        .iter()
        .filter(|binding| binding.node_key == node.node_id.0)
    {
        let property = match &binding.property {
            UiBoundProperty::TextValue => "value",
            UiBoundProperty::Enabled => "enabled",
            UiBoundProperty::Visible => "visible",
            _ => continue,
        };
        line.push_str(&format!(" {} ${}", property, binding.input_key));
    }
    for event in events
        .iter()
        .filter(|event| event.node_key == node.node_id.0)
    {
        line.push_str(&format!(" event {}", event.intent));
    }
    lines.push(line);
    for child in &node.children {
        format_node(child, indent + 2, bindings, events, lines);
    }
}

fn find_node_mut<'a>(node: &'a mut UiNode, key: &str) -> Option<&'a mut UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_node_mut(child, key) {
            return Some(found);
        }
    }
    None
}
fn set_node(
    root: &mut UiNode,
    key: &str,
    payload: Option<&serde_json::Value>,
    span: &NuiSourceSpan,
) -> FlowResult<()> {
    let node = find_node_mut(root, key).ok_or_else(|| {
        error_at(
            "nui_flow_unknown_patch_target",
            "patch target does not exist",
            span,
        )
    })?;
    let payload = payload.ok_or_else(|| {
        error_at(
            "nui_flow_invalid_patch",
            "set operation requires a payload",
            span,
        )
    })?;
    let property = payload
        .get("property")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let value = payload.get("value").and_then(|v| v.as_str()).unwrap_or("");
    match property {
        "enabled" => node.enabled = boolean(value, span.line)?,
        "visible" => node.visible = boolean(value, span.line)?,
        "value" => {
            node.text = Some(TextRef::Literal {
                value: quoted(value, span.line)?,
            })
        }
        "w" => node.bounds.width = number(value, span.line)?,
        "h" => node.bounds.height = number(value, span.line)?,
        _ => {
            return Err(error_at(
                "nui_flow_invalid_patch",
                "set supports enabled, visible, value, w, and h",
                span,
            ))
        }
    }
    Ok(())
}
fn remove_node(parent: &mut UiNode, key: &str) -> bool {
    if let Some(index) = parent
        .children
        .iter()
        .position(|child| child.node_id.0 == key)
    {
        parent.children.remove(index);
        return true;
    }
    parent
        .children
        .iter_mut()
        .any(|child| remove_node(child, key))
}

fn remove_node_at_path(root: &mut UiNode, segments: &[String]) -> bool {
    let segments = if segments.first().is_some_and(|segment| segment == &root.node_id.0) {
        &segments[1..]
    } else {
        segments
    };
    match segments {
        [] => false,
        [key] => remove_node(root, key),
        [parent, rest @ ..] => {
            let Some(parent) = find_node_mut(root, parent) else {
                return false;
            };
            remove_node_at_path(parent, rest)
        }
    }
}
fn insert_node(
    root: &mut UiNode,
    parent_key: &str,
    payload: Option<&serde_json::Value>,
    span: &NuiSourceSpan,
) -> FlowResult<()> {
    let payload = payload.ok_or_else(|| {
        error_at(
            "nui_flow_invalid_patch",
            "insert requires a component and key",
            span,
        )
    })?;
    let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if !valid_key(key) || find_node_mut(root, key).is_some() {
        return Err(error_at(
            "nui_flow_invalid_patch",
            "insert key is invalid or already exists",
            span,
        ));
    }
    let kind = match kind {
        "panel" | "surface" | "scroll" | "overlay" => UiNodeKind::Panel,
        "text" => UiNodeKind::Label,
        "button" => UiNodeKind::Button,
        "input" => UiNodeKind::TextInput,
        "image" => UiNodeKind::Image,
        "render" => UiNodeKind::RenderSurface,
        _ => {
            return Err(error_at(
                "nui_flow_unknown_component",
                "component is outside the closed Flow vocabulary",
                span,
            ))
        }
    };
    let parent = find_node_mut(root, parent_key).ok_or_else(|| {
        error_at(
            "nui_flow_unknown_patch_target",
            "insert parent does not exist",
            span,
        )
    })?;
    parent.children.push(UiNode {
        node_id: UiNodeId(key.into()),
        kind,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        },
        layout: Some(UiLayout::default()),
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
    Ok(())
}
fn move_node(
    root: &mut UiNode,
    key: &str,
    payload: Option<&serde_json::Value>,
    span: &NuiSourceSpan,
) -> FlowResult<()> {
    let parent_path = payload
        .and_then(|p| p.get("parent"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_at(
                "nui_flow_invalid_patch",
                "move requires a destination parent",
                span,
            )
        })?;
    let parent_path = StablePath::parse(parent_path, span)?;
    let parent_key = parent_path.last().to_owned();
    if parent_path.segments().len() > 1 && !path_exists(root, parent_path.segments()) {
        return Err(error_at(
            "nui_flow_unknown_patch_target",
            "move destination semantic path does not exist",
            span,
        ));
    }
    if key == root.node_id.0 || key == parent_key {
        return Err(error_at(
            "nui_flow_invalid_patch",
            "root/self move is not permitted",
            span,
        ));
    }
    let mut taken = None;
    take_node(root, key, &mut taken);
    let node = taken.ok_or_else(|| {
        error_at(
            "nui_flow_unknown_patch_target",
            "move source does not exist",
            span,
        )
    })?;
    let target = find_node_mut(root, &parent_key).ok_or_else(|| {
        error_at(
            "nui_flow_unknown_patch_target",
            "move destination does not exist",
            span,
        )
    })?;
    target.children.push(node);
    Ok(())
}
fn take_node(parent: &mut UiNode, key: &str, taken: &mut Option<UiNode>) {
    if let Some(index) = parent
        .children
        .iter()
        .position(|child| child.node_id.0 == key)
    {
        *taken = Some(parent.children.remove(index));
        return;
    }
    for child in &mut parent.children {
        if taken.is_none() {
            take_node(child, key, taken);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKBENCH: &str = r#"
version 1
surface surface.editor.terrain revision 12
input can_commit bool default false
input terrain_name text default text:empty
panel workspace row gap 8
  panel rail column gap 4
    button water value "Water" enabled $can_commit event terrain.tool.select
  panel inspector column gap 6
    text title value $terrain_name
"#;

    #[test]
    fn lowers_workbench_with_stable_keys_and_bindings() {
        let document = parse_nui_flow(WORKBENCH).expect("valid Flow workbench");
        assert_eq!(document.ir.surface_id.0, "surface.editor.terrain");
        assert_eq!(document.source_map["water"].line, 8);
        assert_eq!(document.source_map["water"].column, 5);
        assert_eq!(document.ir.bindings.len(), 2);
        assert_eq!(document.input_schema.slots[1].packing.offset, 8);
    }

    #[test]
    fn rejects_unknown_and_incompatible_binding_inputs_at_lowering_boundary() {
        let unknown = parse_nui_flow("surface root\n  text title value $missing\n").unwrap_err();
        assert_eq!(unknown.diagnostics[0].code, "ui_program_unknown_binding_target");

        let incompatible = parse_nui_flow(
            "input can_commit bool default false\nsurface root\n  text title value $can_commit\n",
        )
        .unwrap_err();
        assert_eq!(incompatible.diagnostics[0].code, "ui_program_input_type_mismatch");
    }

    #[test]
    fn rejects_forbidden_script_syntax() {
        let error = parse_nui_flow("panel root { eval() }").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "ui_program_forbidden_flow_feature");
    }

    #[test]
    fn patch_targets_stable_key_and_advances_revision() {
        let document = parse_nui_flow(WORKBENCH).unwrap();
        let patch = parse_nui_flow_patch("@ revision 12\n~ water enabled false\n").unwrap();
        let patched = apply_nui_ir_patch(&document.ir, &patch).unwrap();
        assert_eq!(patched.revision, Revision(13));
    }

    #[test]
    fn patch_accepts_semantic_paths_but_rejects_indexes() {
        let document = parse_nui_flow(WORKBENCH).unwrap();
        let patch = parse_nui_flow_patch("@ revision 12\n- /workspace/rail/water\n").unwrap();
        let patched = apply_nui_ir_patch(&document.ir, &patch).unwrap();
        assert!(patched.root.children[0].children.is_empty());

        let indexed = parse_nui_flow_patch("@ revision 12\n- /workspace/0\n").unwrap();
        let error = apply_nui_ir_patch(&document.ir, &indexed).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "nui_flow_invalid_patch_path");

        let incorrect_parent = parse_nui_flow_patch("@ revision 12\n- /workspace/inspector/water\n").unwrap();
        let error = apply_nui_ir_patch(&document.ir, &incorrect_parent).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "nui_flow_unknown_patch_target");
    }

    #[test]
    fn formatter_and_quoted_text_are_deterministic() {
        let formatted = format_nui_flow(WORKBENCH).unwrap();
        assert_eq!(format_nui_flow(&formatted).unwrap(), formatted);
        let error = parse_nui_flow("text title value terrain name").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "nui_flow_unquoted_text");
    }

    #[test]
    fn render_declaration_lowers_to_a_nonempty_render_target() {
        let document = parse_nui_flow("surface workbench\n  render terrain_view\n").unwrap();
        assert_eq!(
            document.ir.root.children[0].surface.as_ref().unwrap().target_id,
            "render.terrain_view"
        );
    }
}
