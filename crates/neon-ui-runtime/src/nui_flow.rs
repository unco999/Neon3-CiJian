//! Closed, line-oriented NUI Flow authoring notation.
//!
//! Flow is deliberately parsed into the canonical JSON IR. It has no evaluator,
//! expressions, callbacks, or source of domain truth.

use std::collections::{BTreeMap, HashSet};

use neon_protocol::Revision;
use neon_ui_schema::{
    NuiFlowDocument, NuiFlowParseDiagnostic, NuiSourceSpan, TextRef, UiAlignItems, UiBoundProperty,
    UiBounds, UiDiagnosticSeverity, UiGridInputSlot, UiInputKind, UiInputPacking, UiInputSchema, UiInputSlot,
    UiInputUpdateClass, UiInputValue, UiIrBinding, UiIrDocument, UiIrPatch, UiIrPatchOperation,
    UiIrPatchOperationKind, UiJustifyContent, UiLayout, UiLayoutMode, UiNode, UiNodeId, UiNodeKind,
    RenderSurfaceRef, UiProgramEventDeclaration, UiResourceBudget, UiSourceSpan, UiStyle,
    UiSurfaceId, UiProgram, UiProgramRevision, UiBranchDeclaration, UiBranchPredicate,
    UiBranchLayoutParticipation, UiTemplateDeclaration, UiDataGridDeclaration, UiDataGridColumn,
    UiDataGridPresentation,
    NuiFlowStateMachine, NuiFlowStateTransition, NuiFlowStateTrigger,
    NuiFlowDragAxis, NuiFlowDragDeclaration, UiDragAxis, UiDragBinding, UiDragBoundary, UiDropBinding, UiEffect, UiIntent,
    NuiFlowDropDeclaration, UiClipPolicy, UiDropPlacement,
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
    let mut grid_slots = Vec::new();
    let mut seen_inputs = HashSet::new();
    let mut branches = Vec::new();
    let mut templates = Vec::new();
    let mut data_grids = Vec::new();
    let mut state_machines = Vec::new();
    let mut drags = Vec::new();
    let mut drops = Vec::new();

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
            if let Some(drop) = parse_drop_declaration(content, line)? {
                if drops.iter().any(|existing: &NuiFlowDropDeclaration| existing.key == drop.key) {
                    return Err(error("nui_flow_invalid_drop", "drop keys must be unique", line, 1));
                }
                drops.push(drop);
                continue;
            }
            if let Some(drag) = parse_drag_declaration(content, line)? {
                if drags.iter().any(|existing: &NuiFlowDragDeclaration| existing.key == drag.key) {
                    return Err(error("nui_flow_invalid_drag", "drag keys must be unique", line, 1));
                }
                drags.push(drag);
                continue;
            }
            if parse_state_machine_declaration(content, &mut state_machines, line)? {
                continue;
            }
            if let Some(input) = parse_input(content, line)? {
                let key = match &input { ParsedInput::Scalar(slot) => &slot.key, ParsedInput::Grid(slot) => &slot.key };
                if !seen_inputs.insert(key.clone()) {
                    return Err(error(
                        "ui_program_duplicate_input_key",
                        "input keys must be unique",
                        line,
                        1,
                    ));
                }
                match input { ParsedInput::Scalar(slot) => input_slots.push(slot), ParsedInput::Grid(slot) => grid_slots.push(slot) }
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
                literal_payload: std::collections::BTreeMap::new(),
                // A control event carries its declared controlled input through
                // the generic semantic boundary, never a renderer-local value.
                bound_input_keys: node.bindings.iter().filter_map(|(property, key)| {
                    matches!(property, UiBoundProperty::TextValue | UiBoundProperty::Active | UiBoundProperty::Selected | UiBoundProperty::NumericValue | UiBoundProperty::StateToken)
                        .then_some(key.clone())
                }).collect(),
            });
        }
        if let Some(predicate) = &node.branch_predicate {
            branches.push(UiBranchDeclaration { branch_key: node.node.node_id.0.clone(), root_node_key: node.node.node_id.0.clone(), predicate: predicate.clone(), layout_participation: UiBranchLayoutParticipation::HiddenSubtree });
        }
        if let Some(template) = &node.template {
            templates.push(UiTemplateDeclaration { template_key: node.node.node_id.0.clone(), root_node_key: node.node.node_id.0.clone(), max_instances: template.0, row_schema: template.1.clone(), instance_key_field: template.2.clone(), overflow_summary: template.3 });
        }
        if let Some(ref grid) = node.data_grid {
            data_grids.push(UiDataGridDeclaration { node_key: node.node.node_id.0.clone(), ..grid.clone() });
        }
        stack.push((indent, node));
    }
    while let Some((_, node)) = stack.pop() {
        attach(node, &mut stack, &mut root, 0)?;
    }
    let mut root = root.ok_or_else(|| {
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
        grid_slots,
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
    for branch in &branches {
        match &branch.predicate {
            UiBranchPredicate::MachineState { machine_key, state } => {
                let machine = state_machines.iter().find(|machine| &machine.key == machine_key).ok_or_else(|| error("nui_flow_invalid_state_machine", "branch references an undeclared machine", 1, 1))?;
                if !machine.states.iter().any(|candidate| candidate == state) {
                    return Err(error("nui_flow_invalid_state_machine", "branch references an undeclared machine state", 1, 1));
                }
            }
            UiBranchPredicate::Bool { input_key, .. } | UiBranchPredicate::EnumEquals { input_key, .. } => {
                let slot = schema.slots.iter().find(|slot| &slot.key == input_key).ok_or_else(|| error("ui_program_invalid_branch_template", "branch predicate input is not declared", 1, 1))?;
                match (&branch.predicate, &slot.kind) {
                    (UiBranchPredicate::Bool { .. }, UiInputKind::Bool) | (UiBranchPredicate::EnumEquals { .. }, UiInputKind::Enum { .. }) => {}
                    _ => return Err(error("ui_program_invalid_branch_template", "branch when requires a bool or enum input", 1, 1)),
                }
            }
        }
    }
    apply_boolean_binding_defaults(&mut root.node, &bindings, &schema);
    for grid in &data_grids {
        if !schema.grid_slots.iter().any(|slot| slot.key == grid.source_key) {
            return Err(error("ui_program_unknown_input_key", "DataGrid source must reference a declared grid input", 1, 1));
        }
    }
    validate_state_machines(&state_machines, &schema)?;
    for drag in &drags {
        if !source_map.contains_key(&drag.source_node_key) {
            return Err(error("nui_flow_invalid_drag", "drag source node is not declared", 1, 1));
        }
    }
    for drop in &drops {
        if !source_map.contains_key(&drop.target_node_key) {
            return Err(error("nui_flow_invalid_drop", "drop target node is not declared", 1, 1));
        }
        if !drags.iter().any(|drag| drag.key == drop.accepts_drag_key) {
            return Err(error("nui_flow_invalid_drop", "drop accepts an undeclared drag key", 1, 1));
        }
        if let Some(template_key) = &drop.presentation_template_key {
            let template = templates.iter().find(|template| &template.template_key == template_key)
                .ok_or_else(|| error("nui_flow_invalid_drop", "drop present references an undeclared bounded template", 1, 1))?;
            let target = find_node(&root.node, &drop.target_node_key)
                .expect("declared drop target exists in the Flow tree");
            if !target.children.iter().any(|child| child.node_id.0 == template.root_node_key) {
                return Err(error("nui_flow_invalid_drop", "drop present template must be directly owned by its target", 1, 1));
            }
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
        branches,
        templates,
        data_grids,
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
        state_machines,
        drags,
        drops,
    })
}

pub fn lower_nui_flow(document: &NuiFlowDocument) -> UiIrDocument {
    document.ir.clone()
}

/// Lowers Flow's declarative interaction vocabulary into the renderer-neutral
/// fragment effect contract. Pointer capture and preview remain WGPU-local.
pub fn lower_nui_flow_effects(document: &NuiFlowDocument) -> Vec<UiEffect> {
    let mut effects = document.ir.events.iter().map(|event| UiEffect::BoundSemanticIntent {
            node_id: UiNodeId(event.node_key.clone()),
        intent: UiIntent::Invoke { action: event.intent.clone(), params: json!({}) },
    }).collect::<Vec<_>>();
    effects.extend(document.drags.iter().map(|drag| UiEffect::DragBinding {
        binding: UiDragBinding {
            key: drag.key.clone(), source_node_id: UiNodeId(drag.source_node_key.clone()),
            axis: match drag.axis { NuiFlowDragAxis::Horizontal => UiDragAxis::Horizontal, NuiFlowDragAxis::Vertical => UiDragAxis::Vertical, NuiFlowDragAxis::Both => UiDragAxis::Both },
            snap: drag.snap, threshold: drag.threshold, boundary: drag.boundary,
        },
    }));
    effects.extend(document.drops.iter().map(|drop| UiEffect::DropBinding {
        binding: UiDropBinding {
            key: drop.key.clone(), target_node_id: UiNodeId(drop.target_node_key.clone()),
            accepts_drag_key: drop.accepts_drag_key.clone(),
            placement: drop.placement,
            presentation_template_key: drop.presentation_template_key.clone(),
            intent: UiIntent::Invoke { action: drop.emit_intent.clone(), params: json!({}) },
        },
    }));
    effects
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
    for slot in &parsed.input_schema.grid_slots {
        lines.push(format!("input {} grid default grid:empty", slot.key));
    }
    for slot in &parsed.input_schema.slots {
        lines.push(format_input(slot));
    }
    format_node(
        &parsed.ir.root,
        0,
        &parsed.ir.bindings,
        &parsed.ir.events, &parsed.ir.data_grids,
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
    branch_predicate: Option<UiBranchPredicate>,
    template: Option<(u32, BTreeMap<String, UiInputKind>, String, bool)>,
    data_grid: Option<UiDataGridDeclaration>,
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

fn parse_state_machine_declaration(
    text: &str,
    machines: &mut Vec<NuiFlowStateMachine>,
    line: u32,
) -> FlowResult<bool> {
    let parts = tokenize(text, line)?;
    let words = parts.iter().map(String::as_str).collect::<Vec<_>>();
    let invalid = |message| error("nui_flow_invalid_state_machine", message, line, 1);
    match words.first().copied() {
        Some("machine") => {
            if words.len() != 4 || words[2] != "initial" || !valid_key(words[1]) || !valid_key(words[3]) {
                return Err(invalid("machine syntax is: machine <key> initial <state>"));
            }
            if machines.iter().any(|machine| machine.key == words[1]) {
                return Err(invalid("machine keys must be unique"));
            }
            machines.push(NuiFlowStateMachine { key: words[1].into(), initial_state: words[3].into(), states: vec![words[3].into()], transitions: Vec::new() });
            Ok(true)
        }
        Some("state") => {
            if words.len() != 3 || !valid_key(words[1]) || !valid_key(words[2]) {
                return Err(invalid("state syntax is: state <machine> <state>"));
            }
            let machine = machines.iter_mut().find(|machine| machine.key == words[1]).ok_or_else(|| invalid("state references an undeclared machine"))?;
            if machine.states.iter().any(|state| state == words[2]) {
                return Err(invalid("state keys must be unique within a machine"));
            }
            machine.states.push(words[2].into());
            Ok(true)
        }
        Some("sync") => {
            if words.len() != 6 || words[2] != "when" || words[4] != "->" {
                return Err(invalid("sync syntax is: sync <machine> when <predicate> -> <state>"));
            }
            let predicate = parse_branch_predicate(words[3], line)?;
            let machine = machines.iter_mut().find(|machine| machine.key == words[1]).ok_or_else(|| invalid("sync references an undeclared machine"))?;
            machine.transitions.push(NuiFlowStateTransition { from_state: "*".into(), trigger: NuiFlowStateTrigger::Sync, predicate: Some(predicate), target_state: words[5].into(), emit_intent: None });
            Ok(true)
        }
        Some("on") => {
            let when_index = words.iter().position(|word| *word == "when");
            let arrow_index = words.iter().position(|word| *word == "->").ok_or_else(|| invalid("event transition requires -> <state>"))?;
            if words.len() < 5 || arrow_index + 1 >= words.len() || !valid_intent(words[2]) {
                return Err(invalid("on syntax is: on <machine> <intent> [when <predicate>] -> <state> [emit <intent>]"));
            }
            if let Some(index) = when_index {
                if index != 3 || arrow_index != 5 { return Err(invalid("event transition has an invalid when clause")); }
            } else if arrow_index != 3 { return Err(invalid("event transition has an invalid target")); }
            let trailing = &words[arrow_index + 2..];
            let emit_intent = match trailing {
                [] => None,
                ["emit", intent] if valid_intent(intent) => Some((*intent).into()),
                _ => return Err(invalid("emit accepts one dotted semantic intent")),
            };
            let predicate = when_index.map(|_| parse_branch_predicate(words[4], line)).transpose()?;
            let machine = machines.iter_mut().find(|machine| machine.key == words[1]).ok_or_else(|| invalid("event transition references an undeclared machine"))?;
            machine.transitions.push(NuiFlowStateTransition { from_state: "*".into(), trigger: NuiFlowStateTrigger::Intent { name: words[2].into() }, predicate, target_state: words[arrow_index + 1].into(), emit_intent });
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_drag_declaration(text: &str, line: u32) -> FlowResult<Option<NuiFlowDragDeclaration>> {
    let parts = tokenize(text, line)?;
    let words = parts.iter().map(String::as_str).collect::<Vec<_>>();
    if words.first().copied() != Some("drag") { return Ok(None); }
    if words.len() != 12 || words[2] != "source" || words[4] != "axis" || words[6] != "snap" || words[8] != "threshold" || words[10] != "within" || !valid_key(words[1]) || !valid_key(words[3]) {
        return Err(error("nui_flow_invalid_drag", "drag syntax is: drag <key> source <node> axis <horizontal|vertical|both> snap <px> threshold <px> within <parent|surface|free>", line, 1));
    }
    let axis = match words[5] {
        "horizontal" => NuiFlowDragAxis::Horizontal,
        "vertical" => NuiFlowDragAxis::Vertical,
        "both" => NuiFlowDragAxis::Both,
        _ => return Err(error("nui_flow_invalid_drag", "drag axis must be horizontal, vertical, or both", line, 1)),
    };
    let snap = number(words[7], line)?;
    let threshold = number(words[9], line)?;
    if snap < 0.0 || threshold < 0.0 { return Err(error("nui_flow_invalid_drag", "drag snap and threshold must be nonnegative", line, 1)); }
    let boundary = match words[11] {
        "parent" => UiDragBoundary::Parent,
        "surface" => UiDragBoundary::Surface,
        "free" => UiDragBoundary::Free,
        _ => return Err(error("nui_flow_invalid_drag", "drag within must be parent, surface, or free", line, 1)),
    };
    Ok(Some(NuiFlowDragDeclaration { key: words[1].into(), source_node_key: words[3].into(), axis, snap, threshold, boundary }))
}

fn parse_drop_declaration(text: &str, line: u32) -> FlowResult<Option<NuiFlowDropDeclaration>> {
    let parts = tokenize(text, line)?;
    let words = parts.iter().map(String::as_str).collect::<Vec<_>>();
    if words.first().copied() != Some("drop") { return Ok(None); }
    if words.len() < 8 || words[..6] != ["drop", words[1], "target", words[3], "accepts", words[5]] {
        return Err(error("nui_flow_invalid_drop", "drop syntax is: drop <key> target <node> accepts <drag> [placement <into|before|after>] [present <template-key>] emit <intent>", line, 1));
    }
    let mut cursor = 6;
    let mut placement = UiDropPlacement::Into;
    if words.get(cursor) == Some(&"placement") {
        placement = match words.get(cursor + 1).copied() {
            Some("into") => UiDropPlacement::Into,
            Some("before") => UiDropPlacement::Before,
            Some("after") => UiDropPlacement::After,
            _ => return Err(error("nui_flow_invalid_drop", "drop placement must be into, before, or after", line, 1)),
        };
        cursor += 2;
    }
    let presentation_template_key = if words.get(cursor) == Some(&"present") {
        let template_key = words.get(cursor + 1).ok_or_else(|| error("nui_flow_invalid_drop", "drop present requires a template key", line, 1))?;
        cursor += 2;
        Some((*template_key).to_string())
    } else {
        None
    };
    if words.get(cursor) != Some(&"emit") || cursor + 2 != words.len() {
        return Err(error("nui_flow_invalid_drop", "drop syntax is: drop <key> target <node> accepts <drag> [placement <into|before|after>] [present <template-key>] emit <intent>", line, 1));
    }
    if !valid_key(words[1]) || !valid_key(words[3]) || !valid_key(words[5]) || !presentation_template_key.as_ref().is_none_or(|key| valid_key(key)) || !valid_intent(words[cursor + 1]) {
        return Err(error("nui_flow_invalid_drop", "drop keys and intent are invalid", line, 1));
    }
    Ok(Some(NuiFlowDropDeclaration { key: words[1].into(), target_node_key: words[3].into(), accepts_drag_key: words[5].into(), placement, presentation_template_key, emit_intent: words[cursor + 1].into() }))
}

fn find_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, key))
}

fn validate_state_machines(machines: &[NuiFlowStateMachine], schema: &UiInputSchema) -> FlowResult<()> {
    for machine in machines {
        if !machine.states.iter().any(|state| state == &machine.initial_state) {
            return Err(error("nui_flow_invalid_state_machine", "initial state must be declared", 1, 1));
        }
        for transition in &machine.transitions {
            if !machine.states.iter().any(|state| state == &transition.target_state) {
                return Err(error("nui_flow_invalid_state_machine", "transition target state is not declared", 1, 1));
            }
            if let Some(predicate) = &transition.predicate {
                let input_key = match predicate { UiBranchPredicate::Bool { input_key, .. } | UiBranchPredicate::EnumEquals { input_key, .. } => input_key, UiBranchPredicate::MachineState { .. } => return Err(error("nui_flow_invalid_state_machine", "state transitions cannot predicate on another machine", 1, 1)) };
                let slot = schema.slots.iter().find(|slot| slot.key == *input_key).ok_or_else(|| error("nui_flow_invalid_state_machine", "transition predicate input is not declared", 1, 1))?;
                if !matches!((predicate, &slot.kind), (UiBranchPredicate::Bool { .. }, UiInputKind::Bool) | (UiBranchPredicate::EnumEquals { .. }, UiInputKind::Enum { .. })) {
                    return Err(error("nui_flow_invalid_state_machine", "transition predicate type is incompatible", 1, 1));
                }
            }
        }
    }
    Ok(())
}

enum ParsedInput { Scalar(UiInputSlot), Grid(UiGridInputSlot) }

fn parse_input(text: &str, line: u32) -> FlowResult<Option<ParsedInput>> {
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
    if parts[2] == "grid" {
        if parts[4] != "grid:empty" {
            return Err(error("nui_flow_invalid_literal", "grid inputs require default grid:empty", line, 1));
        }
        return Ok(Some(ParsedInput::Grid(UiGridInputSlot { key: parts[1].into() })));
    }
    let kind = match parts[2] {
        "bool" => UiInputKind::Bool,
        "i32" => UiInputKind::I32,
        "u32" => UiInputKind::U32,
        "f32" => UiInputKind::F32,
        "text" => UiInputKind::TextHandle,
        enum_spec if enum_spec.starts_with("enum:") => {
            let variants = enum_spec[5..]
                .split('|')
                .filter(|variant| !variant.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if variants.is_empty() || variants.iter().any(|variant| !valid_key(variant)) {
                return Err(error("nui_flow_invalid_input", "enum input variants use enum:one|two and stable variant keys", line, 1));
            }
            UiInputKind::Enum { variants }
        }
        _ => {
            return Err(error(
                "nui_flow_unknown_input_kind",
                "Flow supports bool, i32, u32, f32, text, and enum:one|two inputs",
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
        (enum_spec, value) if enum_spec.starts_with("enum:") => UiInputValue::Enum { value: value.into() },
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
    Ok(Some(ParsedInput::Scalar(UiInputSlot {
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
    })))
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
        "data_grid" => UiNodeKind::DataGrid,
        "tooltip" => UiNodeKind::Tooltip,
        "modal" => UiNodeKind::Modal,
        "dialog" => UiNodeKind::Dialog,
        "text" => UiNodeKind::Label,
        "button" => UiNodeKind::Button,
        "input" => UiNodeKind::TextInput,
        "checkbox" => UiNodeKind::Checkbox,
        "radio_button" => UiNodeKind::RadioButton,
        "slider" => UiNodeKind::Slider,
        "drag_value" => UiNodeKind::DragValue,
        "combo" => UiNodeKind::Combo,
        "dropdown" => UiNodeKind::Dropdown,
        "selectable" => UiNodeKind::Selectable,
        "list_box" => UiNodeKind::ListBox,
        "scrollbar" => UiNodeKind::Scrollbar,
        "progress_bar" => UiNodeKind::ProgressBar,
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
    if parts[0] == "scroll" {
        node.layout.as_mut().expect("Flow nodes have layout").clip = UiClipPolicy::Scroll;
    }
    let mut bindings = Vec::new();
    let mut intents = Vec::new();
    let mut branch_predicate = None;
    let mut template = None;
    let mut data_grid_capacity = None;
    let mut data_grid_row_height = None;
    let mut data_grid_overscan = None;
    let mut data_grid_columns = None;
    let mut data_grid_source = None;
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
            "x" | "y" | "w" | "h" | "minw" | "maxw" | "grow" | "shrink" | "basis" | "gap" | "pad" | "fill"
             | "line" | "ink" | "value" | "checked" | "selected" | "state" | "numeric" | "scroll" | "enabled" | "visible" | "event" | "token" | "align" | "clip"
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
            "when" if parts[0] == "branch" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "when requires a direct input predicate", line, 1))?;
                index += 1;
                branch_predicate = Some(parse_branch_predicate(value, line)?);
            }
            "in" if parts[0] == "branch" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "in requires machine.state", line, 1))?;
                index += 1;
                branch_predicate = Some(parse_machine_state_predicate(value, line)?);
            }
            "capacity" if matches!(parts[0], "repeat" | "template") => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "capacity requires a positive bound", line, 1))?;
                index += 1;
                let capacity = parse_u64(value, line, "capacity")? as u32;
                if capacity == 0 { return Err(error("ui_program_invalid_branch_template", "template capacity must be positive", line, 1)); }
                template = Some((capacity, BTreeMap::from([("row_key".into(), UiInputKind::U32)]), "row_key".into(), false));
            }
            "capacity" if parts[0] == "data_grid" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "capacity requires a positive bound", line, 1))?;
                index += 1;
                let capacity = u32::try_from(parse_u64(value, line, "capacity")?)
                    .map_err(|_| error("nui_flow_invalid_data_grid", "DataGrid capacity exceeds u32", line, 1))?;
                if capacity == 0 { return Err(error("nui_flow_invalid_data_grid", "DataGrid capacity must be positive", line, 1)); }
                data_grid_capacity = Some(capacity);
            }
            "row_height" if parts[0] == "data_grid" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "row_height requires a positive pixel value", line, 1))?;
                index += 1;
                let height = u32::try_from(parse_u64(value, line, "row_height")?)
                    .map_err(|_| error("nui_flow_invalid_data_grid", "DataGrid row_height exceeds u32", line, 1))?;
                if height == 0 { return Err(error("nui_flow_invalid_data_grid", "DataGrid row_height must be positive", line, 1)); }
                data_grid_row_height = Some(height);
            }
            "overscan" if parts[0] == "data_grid" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "overscan requires a nonnegative bound", line, 1))?;
                index += 1;
                data_grid_overscan = Some(u32::try_from(parse_u64(value, line, "overscan")?)
                    .map_err(|_| error("nui_flow_invalid_data_grid", "DataGrid overscan exceeds u32", line, 1))?);
            }
            "columns" if parts[0] == "data_grid" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "columns requires a quoted key:width list", line, 1))?;
                index += 1;
                data_grid_columns = Some(parse_data_grid_columns(&quoted(value, line)?, line)?);
            }
            "source" if parts[0] == "data_grid" => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "source requires a grid input binding", line, 1))?;
                index += 1;
                let Some(key) = value.strip_prefix('$') else { return Err(error("nui_flow_invalid_data_grid", "DataGrid source must use $grid_input", line, 1)); };
                if !valid_key(key) { return Err(error("nui_flow_invalid_data_grid", "DataGrid source must name a valid grid input", line, 1)); }
                data_grid_source = Some(key.into());
            }
            "key" if matches!(parts[0], "repeat" | "template") => {
                let value = *parts.get(index + 1).ok_or_else(|| error("nui_flow_missing_value", "key requires a row key field", line, 1))?;
                index += 1;
                let current = template.get_or_insert((1, BTreeMap::from([("row_key".into(), UiInputKind::U32)]), String::new(), false));
                let prior_key = current.2.clone();
                current.1.remove(&prior_key);
                current.1.insert(value.into(), UiInputKind::U32);
                current.2 = value.into();
            }
            "overflow_summary" if matches!(parts[0], "repeat" | "template") => {
                let current = template.get_or_insert((1, BTreeMap::from([("row_key".into(), UiInputKind::U32)]), "row_key".into(), false));
                current.3 = true;
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
    if parts[0] == "branch" && branch_predicate.is_none() { return Err(error("ui_program_invalid_branch_template", "branch requires `when` or `in machine.state`", line, 1)); }
    if matches!(parts[0], "repeat" | "template") {
        let Some(spec) = &template else { return Err(error("ui_program_invalid_branch_template", "repeat/template requires a finite capacity", line, 1)); };
        if spec.2.trim().is_empty() { return Err(error("ui_program_invalid_branch_template", "repeat/template requires a stable key field", line, 1)); }
    }
    let data_grid = if parts[0] == "data_grid" {
        let capacity = data_grid_capacity.ok_or_else(|| error("nui_flow_invalid_data_grid", "DataGrid requires a finite capacity", line, 1))?;
        let overscan = data_grid_overscan.unwrap_or(0);
        if overscan > capacity { return Err(error("nui_flow_invalid_data_grid", "DataGrid overscan cannot exceed capacity", line, 1)); }
        Some(UiDataGridDeclaration {
            node_key: String::new(),
            source_key: data_grid_source.ok_or_else(|| error("nui_flow_invalid_data_grid", "DataGrid requires source $grid_input", line, 1))?,
            max_window_rows: capacity,
            row_height: data_grid_row_height.unwrap_or(24),
            overscan,
            columns: data_grid_columns.unwrap_or_else(|| vec![UiDataGridColumn { key: "value".into(), label: "Value".into(), width: 1, presentation: UiDataGridPresentation::Text }]),
        })
    } else { None };
    Ok(NodeBuild { node, bindings, intents, branch_predicate, template, data_grid })
}

/// A lowered fragment may be submitted before an input frame is evaluated. Apply
/// boolean defaults that affect participation so hidden dialogs cannot become
/// active modal layers merely because their binding has not changed yet.
fn apply_boolean_binding_defaults(
    root: &mut UiNode,
    bindings: &[UiIrBinding],
    schema: &UiInputSchema,
) {
    fn visit(node: &mut UiNode, bindings: &[UiIrBinding], schema: &UiInputSchema) {
        for binding in bindings.iter().filter(|binding| binding.node_key == node.node_id.0) {
            let Some(UiInputValue::Bool { value }) = schema
                .slots
                .iter()
                .find(|slot| slot.key == binding.input_key)
                .map(|slot| &slot.default_value)
            else {
                continue;
            };
            match binding.property {
                UiBoundProperty::Visible => node.visible = *value,
                UiBoundProperty::Enabled => node.enabled = *value,
                _ => {}
            }
        }
        for child in &mut node.children {
            visit(child, bindings, schema);
        }
    }

    visit(root, bindings, schema);
}

fn parse_data_grid_columns(value: &str, line: u32) -> FlowResult<Vec<UiDataGridColumn>> {
    if value.is_empty() { return Err(error("nui_flow_invalid_data_grid", "DataGrid columns cannot be empty", line, 1)); }
    let mut columns = Vec::new();
    let mut keys = HashSet::new();
    for entry in value.split(',') {
        let parts = entry.split(':').collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err(error("nui_flow_invalid_data_grid", "DataGrid columns use key:width[:presentation]", line, 1));
        }
        let key = parts[0];
        let width = parts[1];
        if !valid_key(key) || !keys.insert(key) { return Err(error("nui_flow_invalid_data_grid", "DataGrid column keys must be valid and unique", line, 1)); }
        let width = u32::try_from(width.parse::<u64>().map_err(|_| error("nui_flow_invalid_data_grid", "DataGrid column width must be a positive integer", line, 1))?)
            .map_err(|_| error("nui_flow_invalid_data_grid", "DataGrid column width exceeds u32", line, 1))?;
        if width == 0 { return Err(error("nui_flow_invalid_data_grid", "DataGrid column width must be positive", line, 1)); }
        let presentation = match parts.get(2).copied() {
            None | Some("text") if parts.len() <= 3 => UiDataGridPresentation::Text,
            Some("select") if parts.len() == 4 => UiDataGridPresentation::Select { intent: parse_data_grid_token(parts[3], line, "select intent")? },
            Some("dropdown") if parts.len() == 5 => UiDataGridPresentation::Dropdown {
                options: parse_data_grid_options(parts[3], line)?,
                intent: parse_data_grid_token(parts[4], line, "dropdown intent")?,
            },
            Some("edit") if parts.len() == 5 => UiDataGridPresentation::Edit {
                max_chars: parse_data_grid_max_chars(parts[3], line)?,
                intent: parse_data_grid_token(parts[4], line, "edit intent")?,
            },
            _ => return Err(error("nui_flow_invalid_data_grid", "invalid DataGrid column presentation grammar", line, 1)),
        };
        columns.push(UiDataGridColumn { key: key.into(), label: key.replace('_', " "), width, presentation });
    }
    Ok(columns)
}

fn parse_data_grid_token(value: &str, line: u32, name: &str) -> FlowResult<String> {
    if value.is_empty() || value.contains('|') || !valid_key(value) {
        return Err(error("nui_flow_invalid_data_grid", &format!("DataGrid {name} must be a valid token"), line, 1));
    }
    Ok(value.into())
}

fn parse_data_grid_options(value: &str, line: u32) -> FlowResult<Vec<String>> {
    let options = value.split('|').map(str::to_owned).collect::<Vec<_>>();
    if options.is_empty() || options.iter().any(|option| option.trim().is_empty()) || options.iter().collect::<HashSet<_>>().len() != options.len() {
        return Err(error("nui_flow_invalid_data_grid", "DataGrid dropdown options must be nonempty and unique", line, 1));
    }
    Ok(options)
}

fn parse_data_grid_max_chars(value: &str, line: u32) -> FlowResult<u32> {
    let max_chars = value.parse::<u64>().ok().and_then(|value| u32::try_from(value).ok()).unwrap_or(0);
    if max_chars == 0 { return Err(error("nui_flow_invalid_data_grid", "DataGrid edit max_chars must be positive", line, 1)); }
    Ok(max_chars)
}

fn parse_branch_predicate(value: &str, line: u32) -> FlowResult<UiBranchPredicate> {
    let value = value.strip_prefix('$').ok_or_else(|| error("ui_program_invalid_branch_template", "branch predicate must reference one direct input", line, 1))?;
    if let Some(input_key) = value.strip_prefix('!') {
        return Ok(UiBranchPredicate::Bool { input_key: input_key.into(), expected: false });
    }
    if let Some((input_key, variant)) = value.split_once('=') {
        if input_key.is_empty() || variant.is_empty() { return Err(error("ui_program_invalid_branch_template", "enum branch predicate requires input and variant", line, 1)); }
        return Ok(UiBranchPredicate::EnumEquals { input_key: input_key.into(), variant: variant.into() });
        }
    Ok(UiBranchPredicate::Bool { input_key: value.into(), expected: true })
}

fn parse_machine_state_predicate(value: &str, line: u32) -> FlowResult<UiBranchPredicate> {
    let Some((machine_key, state)) = value.rsplit_once('.') else {
        return Err(error("nui_flow_invalid_state_machine", "branch in requires machine.state", line, 1));
    };
    if !valid_key(machine_key) || !valid_key(state) {
        return Err(error("nui_flow_invalid_state_machine", "machine and state keys are invalid", line, 1));
    }
    Ok(UiBranchPredicate::MachineState { machine_key: machine_key.into(), state: state.into() })
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
        "x" => node.bounds.x = number(value, line)?,
        "y" => node.bounds.y = number(value, line)?,
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
        "clip" => layout.clip = match value {
                "none" => UiClipPolicy::None,
                "bounds" => UiClipPolicy::Bounds,
                "rounded" => UiClipPolicy::Rounded,
                "scroll" => UiClipPolicy::Scroll,
            _ => return Err(error("nui_flow_invalid_layout", "clip must be none, bounds, rounded, or scroll", line, 1)),
        },
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
        "checked" => {
            if direct_binding(UiBoundProperty::Active).is_none() { return Err(error("nui_flow_invalid_control_binding", "checked requires a bool input binding", line, 1)); }
        }
        "selected" => {
            if direct_binding(UiBoundProperty::Selected).is_none() { return Err(error("nui_flow_invalid_control_binding", "selected requires a bool input binding", line, 1)); }
        }
        "state" => {
            if direct_binding(UiBoundProperty::StateToken).is_none() { return Err(error("nui_flow_invalid_control_binding", "state requires an enum input binding", line, 1)); }
        }
        "numeric" | "scroll" => {
            if direct_binding(UiBoundProperty::NumericValue).is_none() { return Err(error("nui_flow_invalid_control_binding", "numeric and scroll require a numeric input binding", line, 1)); }
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
        || text.contains("=") && !text.starts_with("budget") && !text.starts_with("@") && !text.contains("when $")
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
        UiBoundProperty::Enabled | UiBoundProperty::Visible | UiBoundProperty::Selected | UiBoundProperty::Active => matches!(kind, UiInputKind::Bool),
        UiBoundProperty::StateToken => matches!(kind, UiInputKind::Enum { .. }),
        UiBoundProperty::NumericValue => matches!(kind, UiInputKind::I32 | UiInputKind::U32 | UiInputKind::F32 | UiInputKind::I32Range { .. } | UiInputKind::U32Range { .. }),
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
    data_grids: &[UiDataGridDeclaration],
    lines: &mut Vec<String>,
) {
    let kind = match &node.kind {
        UiNodeKind::Panel => "panel",
        UiNodeKind::Label => "text",
        UiNodeKind::Button => "button",
        UiNodeKind::TextInput => "input",
        UiNodeKind::Checkbox => "checkbox",
        UiNodeKind::RadioButton => "radio_button",
        UiNodeKind::Slider => "slider",
        UiNodeKind::DragValue => "drag_value",
        UiNodeKind::Combo => "combo",
        UiNodeKind::Dropdown => "dropdown",
        UiNodeKind::Tooltip => "tooltip",
        UiNodeKind::Modal => "modal",
        UiNodeKind::Dialog => "dialog",
        UiNodeKind::Selectable => "selectable",
        UiNodeKind::ListBox => "list_box",
        UiNodeKind::Scrollbar => "scrollbar",
        UiNodeKind::ProgressBar => "progress_bar",
        UiNodeKind::DataGrid => "data_grid",
        UiNodeKind::Image => "image",
        UiNodeKind::RenderSurface => "render",
    };
    let mut line = format!("{}{} {}", " ".repeat(indent), kind, node.node_id.0);
    if node.bounds.x != 0.0 { line.push_str(&format!(" x {}", node.bounds.x)); }
    if node.bounds.y != 0.0 { line.push_str(&format!(" y {}", node.bounds.y)); }
    if node.kind == UiNodeKind::DataGrid {
        if let Some(grid) = data_grids.iter().find(|grid| grid.node_key == node.node_id.0) {
            line.push_str(&format!(" source ${} capacity {} row_height {} overscan {} columns \"{}\"",
                grid.source_key, grid.max_window_rows, grid.row_height, grid.overscan,
                grid.columns.iter().map(format_data_grid_column).collect::<Vec<_>>().join(",")));
        }
    }
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
        if layout.clip != UiClipPolicy::Bounds {
            let policy = match layout.clip {
                UiClipPolicy::None => "none",
                UiClipPolicy::Bounds => "bounds",
                UiClipPolicy::Rounded => "rounded",
                UiClipPolicy::Scroll => "scroll",
            };
            line.push_str(&format!(" clip {policy}"));
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
            UiBoundProperty::Active => "checked",
            UiBoundProperty::Selected => "selected",
            UiBoundProperty::StateToken => "state",
            UiBoundProperty::NumericValue => "numeric",
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
        format_node(child, indent + 2, bindings, events, data_grids, lines);
    }
}

fn format_data_grid_column(column: &UiDataGridColumn) -> String {
    match &column.presentation {
        UiDataGridPresentation::Text => format!("{}:{}", column.key, column.width),
        UiDataGridPresentation::Select { intent } => format!("{}:{}:select:{}", column.key, column.width, intent),
        UiDataGridPresentation::Dropdown { options, intent } => format!("{}:{}:dropdown:{}:{}", column.key, column.width, options.join("|"), intent),
        UiDataGridPresentation::Edit { max_chars, intent } => format!("{}:{}:edit:{}:{}", column.key, column.width, max_chars, intent),
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
        "tooltip" => UiNodeKind::Tooltip,
        "modal" => UiNodeKind::Modal,
        "dialog" => UiNodeKind::Dialog,
        "text" => UiNodeKind::Label,
        "button" => UiNodeKind::Button,
        "input" => UiNodeKind::TextInput,
        "checkbox" => UiNodeKind::Checkbox,
        "radio_button" => UiNodeKind::RadioButton,
        "slider" => UiNodeKind::Slider,
        "drag_value" => UiNodeKind::DragValue,
        "combo" => UiNodeKind::Combo,
        "dropdown" => UiNodeKind::Dropdown,
        "selectable" => UiNodeKind::Selectable,
        "list_box" => UiNodeKind::ListBox,
        "scrollbar" => UiNodeKind::Scrollbar,
        "progress_bar" => UiNodeKind::ProgressBar,
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
    fn parses_closed_enum_input_domains_for_direct_branch_predicates() {
        let document = parse_nui_flow(
            "input state enum:loading|ready|error default loading\nsurface root\n  branch ready when $state=ready\n    text label value \"Ready\"\n",
        ).unwrap();
        assert_eq!(document.input_schema.slots[0].kind, UiInputKind::Enum { variants: vec!["loading".into(), "ready".into(), "error".into()] });
        assert_eq!(document.ir.branches.len(), 1);
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
    fn data_grid_full_declaration_round_trips_typed_columns() {
        let source = "input asset_window grid default grid:empty\nsurface root\n  data_grid assets source $asset_window capacity 64 row_height 28 overscan 3 columns \"Name:240,Status:120\"\n";
        let document = parse_nui_flow(source).unwrap();
        let grid = &document.ir.data_grids[0];
        assert_eq!(grid.row_height, 28);
        assert_eq!(grid.overscan, 3);
        assert_eq!(grid.columns[0], UiDataGridColumn { key: "Name".into(), label: "Name".into(), width: 240, presentation: UiDataGridPresentation::Text });
        assert_eq!(grid.columns[1].key, "Status");
        let formatted = format_nui_flow(source).unwrap();
        assert_eq!(grid.source_key, "asset_window");
        assert!(formatted.contains("source $asset_window capacity 64 row_height 28 overscan 3 columns \"Name:240,Status:120\""));
    }

    #[test]
    fn data_grid_sources_require_declared_control_plane_grid_inputs() {
        let document = parse_nui_flow("input enabled bool default true\ninput asset_window grid default grid:empty\nsurface root\n  data_grid assets source $asset_window capacity 2\n").unwrap();
        assert_eq!(document.input_schema.slots.len(), 1);
        assert_eq!(document.input_schema.grid_slots[0].key, "asset_window");
        assert_eq!(document.input_schema.slots[0].packing.offset, 0);

        let error = parse_nui_flow("surface root\n  data_grid assets source $asset_window capacity 2\n").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "ui_program_unknown_input_key");
    }

    #[test]
    fn data_grid_columns_are_strict() {
        for columns in ["Name", "Name:0", "Name:20,Name:30", "Name:-1", "Name:20:select", "Name:20:dropdown::set", "Name:20:dropdown: |a:set", "Name:20:dropdown:a|a:set", "Name:20:edit:0:set", "Name:20:unknown"] {
            let error = parse_nui_flow(&format!("input asset_window grid default grid:empty\nsurface root\n  data_grid assets source $asset_window capacity 2 columns \"{columns}\"\n")).unwrap_err();
            assert_eq!(error.diagnostics[0].code, "nui_flow_invalid_data_grid");
        }
    }

    #[test]
    fn data_grid_columns_parse_presentations_and_format_deterministically() {
        let source = "input asset_window grid default grid:empty\nsurface root\n  data_grid assets source $asset_window capacity 2 columns \"Name:240:text,State:120:select:asset.state.select,Owner:160:dropdown:me|team:asset.owner.select,Notes:280:edit:128:asset.notes.edit\"\n";
        let document = parse_nui_flow(source).unwrap();
        assert_eq!(document.ir.data_grids[0].columns[1].presentation, UiDataGridPresentation::Select { intent: "asset.state.select".into() });
        assert_eq!(document.ir.data_grids[0].columns[2].presentation, UiDataGridPresentation::Dropdown { options: vec!["me".into(), "team".into()], intent: "asset.owner.select".into() });
        assert_eq!(document.ir.data_grids[0].columns[3].presentation, UiDataGridPresentation::Edit { max_chars: 128, intent: "asset.notes.edit".into() });
        let formatted = format_nui_flow(source).unwrap();
        assert!(formatted.contains("Name:240,State:120:select:asset.state.select,Owner:160:dropdown:me|team:asset.owner.select,Notes:280:edit:128:asset.notes.edit"));
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
    fn tooltip_and_modal_components_round_trip_as_declarative_node_kinds() {
        let source = "surface root\n  tooltip hint value \"More detail\"\n  modal confirm\n    dialog prompt\n      text body value \"Continue?\"\n";
        let document = parse_nui_flow(source).unwrap();
        assert_eq!(document.ir.root.children[0].kind, UiNodeKind::Tooltip);
        assert_eq!(document.ir.root.children[1].kind, UiNodeKind::Modal);
        assert_eq!(document.ir.root.children[1].children[0].kind, UiNodeKind::Dialog);
        assert!(format_nui_flow(source).unwrap().contains("tooltip hint"));
    }

    #[test]
    fn data_grid_declaration_lowers_with_a_finite_window_capacity() {
        let source = "input asset_window grid default grid:empty\nsurface root\n  data_grid assets source $asset_window capacity 64 h 400\n";
        let document = parse_nui_flow(source).unwrap();
        assert_eq!(document.ir.root.children[0].kind, UiNodeKind::DataGrid);
        assert_eq!(document.ir.data_grids[0].node_key, "assets");
        assert_eq!(document.ir.data_grids[0].max_window_rows, 64);
        assert!(format_nui_flow(source).unwrap().contains("data_grid assets source $asset_window capacity 64"));

        let error = parse_nui_flow("surface root\n  data_grid assets\n").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "nui_flow_invalid_data_grid");
    }

    #[test]
    fn render_declaration_lowers_to_a_nonempty_render_target() {
        let document = parse_nui_flow("surface workbench\n  render terrain_view\n").unwrap();
        assert_eq!(
            document.ir.root.children[0].surface.as_ref().unwrap().target_id,
            "render.terrain_view"
        );
    }

    #[test]
    fn complex_asset_review_fixture_uses_bounded_declarative_ui_features() {
        let source = include_str!("../../../tests/fixtures/ui/asset-review-workbench.nui");
        let document = parse_nui_flow(source).unwrap();

        assert_eq!(document.input_schema.slots.len(), 12);
        assert_eq!(document.state_machines.len(), 2);
        assert_eq!(document.drags.len(), 1);
        assert!(document.ir.events.iter().any(|event| event.intent == "asset.review.publish"));
        assert!(document.ir.root.children.iter().any(|node| node.node_id.0 == "command-bar"));
    }

    #[test]
    fn flow_drag_and_drop_lower_to_generic_effect_bindings() {
        let document = parse_nui_flow(include_str!("../../../tests/fixtures/ui/kanban-reparent-workbench.nui")).unwrap();
        let effects = lower_nui_flow_effects(&document);
        assert!(effects.iter().any(|effect| matches!(effect, UiEffect::DragBinding { binding } if binding.key == "backlog-card-drag" && binding.source_node_id.0 == "backlog-card-01" && binding.boundary == UiDragBoundary::Surface)));
        assert!(effects.iter().any(|effect| matches!(effect, UiEffect::DropBinding { binding } if binding.target_node_id.0 == "in-progress-panel" && binding.accepts_drag_key == "backlog-card-drag" && binding.presentation_template_key.as_deref() == Some("progress-template"))));
        assert!(effects.iter().any(|effect| matches!(effect, UiEffect::DropBinding { binding } if binding.target_node_id.0 == "done-panel" && binding.accepts_drag_key == "backlog-card-drag" && binding.presentation_template_key.as_deref() == Some("accepted-template"))));
    }

    #[test]
    fn drop_placement_defaults_to_into_and_accepts_relative_targets() {
        let source = "drag item-drag source item axis both snap 0 threshold 0 within free\ndrop default target target accepts item-drag emit workspace.item.move\ndrop before target target accepts item-drag placement before emit workspace.item.move\ndrop after target target accepts item-drag placement after emit workspace.item.move\nsurface root\n  panel item\n  panel target\n";
        let document = parse_nui_flow(source).unwrap();
        assert_eq!(document.drops.iter().map(|drop| drop.placement).collect::<Vec<_>>(), vec![UiDropPlacement::Into, UiDropPlacement::Before, UiDropPlacement::After]);
        assert!(lower_nui_flow_effects(&document).iter().any(|effect| matches!(effect, UiEffect::DropBinding { binding } if binding.key == "after" && binding.placement == UiDropPlacement::After)));
    }

    #[test]
    fn drop_present_lowers_a_target_owned_template_key() {
        let source = "drag item-drag source item axis both snap 0 threshold 0 within free\ndrop target-drop target target accepts item-drag present target-row emit workspace.item.move\nsurface root\n  panel item\n  panel target\n    template target-row h 32 capacity 4 key row_key overflow_summary\n      text row value \"Target row\"\n";
        let document = parse_nui_flow(source).unwrap();
        assert_eq!(document.drops[0].presentation_template_key.as_deref(), Some("target-row"));
        assert!(lower_nui_flow_effects(&document).iter().any(|effect| matches!(effect, UiEffect::DropBinding { binding } if binding.key == "target-drop" && binding.presentation_template_key.as_deref() == Some("target-row"))));
    }

    #[test]
    fn drop_present_requires_a_target_owned_template_for_every_placement() {
        let undeclared = parse_nui_flow("drag item-drag source item axis both snap 0 threshold 0 within free\ndrop target-drop target target accepts item-drag present missing-row emit workspace.item.move\nsurface root\n  panel item\n  panel target\n").unwrap_err();
        assert_eq!(undeclared.diagnostics[0].code, "nui_flow_invalid_drop");

        let relative = parse_nui_flow("drag item-drag source item axis both snap 0 threshold 0 within free\ndrop target-drop target target accepts item-drag placement before present target-row emit workspace.item.move\nsurface root\n  panel item\n  panel target\n    template target-row h 32 capacity 4 key row_key overflow_summary\n      text row value \"Target row\"\n").unwrap();
        assert_eq!(relative.drops[0].placement, UiDropPlacement::Before);
        assert_eq!(relative.drops[0].presentation_template_key.as_deref(), Some("target-row"));

        let unowned = parse_nui_flow("drag item-drag source item axis both snap 0 threshold 0 within free\ndrop target-drop target target accepts item-drag present other-row emit workspace.item.move\nsurface root\n  panel item\n  panel target\n  template other-row h 32 capacity 4 key row_key overflow_summary\n    text row value \"Other row\"\n").unwrap_err();
        assert_eq!(unowned.diagnostics[0].code, "nui_flow_invalid_drop");
    }

    #[test]
    fn panel_clip_defaults_to_bounds_and_flow_accepts_explicit_policies() {
        let document = parse_nui_flow("surface root clip none\n  panel bounds\n  panel rounded clip rounded\n  scroll scroller\n").unwrap();
        assert_eq!(document.ir.root.layout.unwrap().clip, UiClipPolicy::None);
        assert_eq!(document.ir.root.children[0].layout.unwrap().clip, UiClipPolicy::Bounds);
        assert_eq!(document.ir.root.children[1].layout.unwrap().clip, UiClipPolicy::Rounded);
        assert_eq!(document.ir.root.children[2].layout.unwrap().clip, UiClipPolicy::Scroll);
        assert!(format_nui_flow("surface root clip none\n  panel rounded clip rounded\n").unwrap().contains("clip rounded"));
    }

    #[test]
    fn drag_boundary_policy_accepts_all_declared_values() {
        for (within, boundary) in [
            ("parent", UiDragBoundary::Parent),
            ("surface", UiDragBoundary::Surface),
            ("free", UiDragBoundary::Free),
        ] {
            let source = format!("drag demo source panel axis both snap 0 threshold 0 within {within}\nsurface root\n  panel panel\n");
            assert_eq!(parse_nui_flow(&source).unwrap().drags[0].boundary, boundary);
        }
        let error = parse_nui_flow("drag demo source panel axis both snap 0 threshold 0 within column\nsurface root\n  panel panel\n").unwrap_err();
        assert_eq!(error.diagnostics[0].code, "nui_flow_invalid_drag");
    }
}
