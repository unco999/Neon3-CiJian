//! Generic local demo domain endpoint for accepted drag/drop revisions.
//!
//! It consumes declared semantic keys and returns a new fragment. It never
//! reaches into the renderer or uses renderer hit identifiers.

use std::net::SocketAddr;

use neon_ipc::{RpcServer, TransportError};
use neon_protocol::{Revision, RpcError, RpcRequest, RpcResponse, RpcStatus};
use neon_ui_schema::{
    TextRef, UiControlPresentation, UiDropPlacement, UiEffect, UiFragment, UiInputChange, UiInputFrame, UiInputKind,
    UiDataGridCell, UiDataGridFrame, UiDataGridWindowRequest, UiDataGridWindowRow,
    UiInputSchema, UiInputValue, UiNode, UiNodeId, UiProgram, UiProgramSemanticEvent,
    UiProgramSemanticEventKind, UiSemanticEvent, UiSemanticEventType, UiResolvedInputs, UiProgramRevision,
    UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
    UiProgramSemanticEventStatus, UiSemanticPayloadValue, UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
    UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
    UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME, UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
};
use serde_json::{json, Value};

use crate::{compile_nui_flow_program, parse_nui_flow, UiInputStore, UiInputStoreError,
    UiInputWriter, UiProgramSemanticEventRouter};

/// Generic controlled-input demo domain. It resolves the slot from the program
/// declaration, not from a renderer control kind or renderer identity.
pub struct DemoInputDomain {
    program: UiProgram,
    inputs: UiInputStore,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DemoInputDomainSnapshot {
    pub inputs: UiResolvedInputs,
    pub visible_status: std::collections::BTreeMap<String, String>,
}

impl DemoInputDomain {
    pub fn new(program: UiProgram, schema: UiInputSchema) -> Result<Self, UiInputStoreError> {
        let inputs = UiInputStore::activate(program.revision.clone(), schema)?;
        Ok(Self { program, inputs })
    }

    pub fn snapshot(&self) -> DemoInputDomainSnapshot {
        let inputs = self.inputs.snapshot();
        let visible_status = inputs.values.iter().map(|(key, value)| {
            let label = match key.as_str() {
                "list_choice" => "Selected mode",
                "drag_value" => "Current count",
                _ => key,
            };
            (format!("status-{key}"), format!("{label}: {}", display_value(&value.value)))
        }).collect();
        DemoInputDomainSnapshot { inputs, visible_status }
    }

    pub fn apply(&mut self, event: &UiProgramSemanticEvent) -> Result<DemoInputDomainSnapshot, &'static str> {
        let declaration = self.program.event_records.iter().find(|declaration| {
                declaration.node_key == event.source_node_key && declaration.intent == event.intent
        }).ok_or("semantic event is not declared by the source node")?;
        let key = declaration.bound_input_keys.iter().find(|key| {
                self.program.binding_records.iter().any(|binding| {
                binding.node_key == event.source_node_key && binding.input_key == **key
                    && matches!(binding.property, neon_ui_schema::UiBoundProperty::Active | neon_ui_schema::UiBoundProperty::Selected | neon_ui_schema::UiBoundProperty::NumericValue | neon_ui_schema::UiBoundProperty::StateToken)
                })
        }).ok_or("semantic event has no controlled input binding")?;
        let slot = self.inputs.schema().slots.iter().find(|slot| slot.key == *key)
            .ok_or("controlled input slot is missing")?;
        let current_inputs = self.inputs.snapshot();
        let current = current_inputs.values.get(key).ok_or("controlled input value is missing")?;
        let value = match &event.requested_value {
            Some(requested) => requested_input_value(&slot.kind, requested)
                .ok_or("requested control value does not match the bound input kind")?,
            None => advance_value(&slot.kind, &current.value)
                .ok_or("controlled input kind is not supported")?,
        };
        self.inputs.apply(UiInputWriter::External, UiInputFrame {
                    program_revision: self.program.revision.clone(),
                    expected_input_revision: current_inputs.input_revision,
                    request_id: event.request_id.clone(),
                    idempotency_key: event.idempotency_key.clone(),
            changes: vec![UiInputChange { key: key.clone(), value }],
        }).map_err(|_| "controlled input frame was rejected")?;
        Ok(self.snapshot())
    }

    /// Runs the controlled component-gallery domain over the public RPC boundary.
    /// The semantic gate remains the UI runtime's router; this endpoint owns only
    /// the controlled input state and visible status values.
    pub fn serve_component_gallery(endpoint: SocketAddr) -> Result<(), TransportError> {
        let (document, program) = component_gallery_program()
            .map_err(|error| TransportError::Io(std::io::Error::other(error)))?;
        let mut domain = Self::new(program.clone(), document.input_schema)
            .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?;
        let mut router = UiProgramSemanticEventRouter::new(program, domain.snapshot().inputs, 1);
        let server = RpcServer::bind(endpoint)?;
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let response = if shutdown {
                accepted(request, Some(domain.snapshot().inputs.input_revision), json!({"state": "accepted"}))
            } else if request.method != "ui.program.event" {
                rejected(request, Some(domain.snapshot().inputs.input_revision), "unsupported_method", "method is not supported")
            } else {
                let event = serde_json::from_value::<UiProgramSemanticEvent>(request.params.clone());
                match event {
                    Err(_) => rejected(request, Some(domain.snapshot().inputs.input_revision), "invalid_request", "a typed UI program semantic event is required"),
                    Ok(event) => {
                        let validation = router.validate(&event);
                        if validation.status != UiProgramSemanticEventStatus::Accepted {
                            rejected(request, Some(domain.snapshot().inputs.input_revision), validation.code.as_deref().unwrap_or("semantic_event_rejected"), &validation.message)
                        } else {
                            match domain.apply(&event) {
                                Ok(snapshot) => {
                                    router.replace_resolved_inputs(snapshot.inputs.clone());
                                    accepted(request, Some(snapshot.inputs.input_revision), json!({"validation": validation, "snapshot": snapshot}))
                                }
                                Err(message) => rejected(request, Some(domain.snapshot().inputs.input_revision), "domain_input_rejected", message),
                            }
                        }
                    }
                }
            };
            (response, !shutdown)
        })
    }
}

pub fn component_gallery_program() -> Result<(neon_ui_schema::NuiFlowDocument, neon_ui_schema::UiProgram), String> {
    let document = parse_nui_flow(include_str!("../../../tests/fixtures/ui/imgui-component-gallery.nui"))
    .map_err(|error| format!("component gallery fixture is invalid: {error:?}"))?;
    let revision = UiProgramRevision {
        program_id: "component-gallery.scenario".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
        ]
        .into_iter()
        .map(|name| UiProgramCapability {
            name: name.into(), version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        })
        .collect(),
    };
    let program = compile_nui_flow_program(&document, revision)
        .map_err(|error| format!("component gallery program did not compile: {error:?}"))?;
    Ok((document, program))
}

/// Applies a domain-produced display snapshot to declared status labels. Status
/// node keys name input slots, so the renderer remains unaware of control kinds.
pub fn apply_visible_status_to_fragment(
    fragment: &mut UiFragment,
    snapshot: &DemoInputDomainSnapshot,
) {
    fn visit(node: &mut UiNode, status: &std::collections::BTreeMap<String, String>) {
        if let Some(value) = status.get(&node.node_id.0) {
            node.text = Some(TextRef::Literal { value: value.clone() });
        }
        for child in &mut node.children {
            visit(child, status);
        }
    }
    visit(&mut fragment.root, &snapshot.visible_status);
    fragment.effects.retain(|effect| !matches!(effect, UiEffect::ControlPresentation { .. }));
    let presentation = |node_id: &str, state| UiEffect::ControlPresentation {
        node_id: UiNodeId(node_id.into()),
        state,
    };
    let values = &snapshot.inputs.values;
    if let Some(UiInputValue::Bool { value }) = values.get("feature_enabled").map(|value| &value.value) {
        fragment.effects.push(presentation("feature-toggle", UiControlPresentation::Toggle { selected: *value }));
    }
    if let Some(UiInputValue::Bool { value }) = values.get("radio_selected").map(|value| &value.value) {
        fragment.effects.push(presentation("mode-radio", UiControlPresentation::Toggle { selected: *value }));
    }
    if let Some(UiInputValue::F32 { value }) = values.get("slider_value").map(|value| &value.value) {
        fragment.effects.push(presentation("exposure-slider", UiControlPresentation::Numeric { value: *value, min: 0.0, max: 1.0 }));
    }
    if let Some(UiInputValue::I32 { value }) = values.get("drag_value").map(|value| &value.value) {
        fragment.effects.push(presentation("count-drag", UiControlPresentation::Numeric { value: *value as f32, min: 0.0, max: 24.0 }));
    }
    for (node_id, input_key) in [("mode-combo", "combo_choice"), ("mode-dropdown", "dropdown_choice"), ("item-list", "list_choice")] {
        if let Some(UiInputValue::Enum { value }) = values.get(input_key).map(|value| &value.value) {
            let selected = matches!(node_id, "mode-radio" | "item-selectable") && value == "alpha";
            fragment.effects.push(presentation(node_id, UiControlPresentation::Choice {
                    token: value.clone(),
                    options: vec!["alpha".into(), "beta".into(), "gamma".into()],
                    selected,
            }));
        }
    }
    if let Some(UiInputValue::Bool { value }) = values.get("item_selected").map(|value| &value.value) {
        fragment.effects.push(presentation("item-selectable", UiControlPresentation::Toggle { selected: *value }));
    }
    if let Some(UiInputValue::F32 { value }) = values.get("scroll_position").map(|value| &value.value) {
        fragment.effects.push(presentation("gallery-scroll", UiControlPresentation::Scroll { position: *value }));
    }
}

/// Maps a component-gallery control node to its program semantic event kind.
/// Intent names are unique per control, so the forwarder resolves the source
/// node from the program declaration instead of a renderer identity.
pub fn gallery_event_kind(node_key: &str) -> UiProgramSemanticEventKind {
    match node_key {
        "feature-toggle" | "mode-radio" | "mode-combo" | "mode-dropdown"
        | "item-selectable" | "item-list" => UiProgramSemanticEventKind::SelectionChanged,
        _ => UiProgramSemanticEventKind::ValueCommit,
    }
}

fn advance_value(kind: &UiInputKind, value: &UiInputValue) -> Option<UiInputValue> {
    Some(match (kind, value) {
        (UiInputKind::Bool, UiInputValue::Bool { value }) => UiInputValue::Bool { value: !value },
        (UiInputKind::I32, UiInputValue::I32 { value }) => UiInputValue::I32 { value: value.saturating_add(1) },
        (UiInputKind::U32, UiInputValue::U32 { value }) => UiInputValue::U32 { value: value.saturating_add(1) },
        (UiInputKind::F32, UiInputValue::F32 { value }) => UiInputValue::F32 { value: (value + 0.1).min(1.0) },
        (UiInputKind::Enum { variants }, UiInputValue::Enum { value }) => UiInputValue::Enum {
            value: variants.iter().cycle().skip_while(|variant| *variant != value).nth(1)?.clone(),
        },
        _ => return None,
    })
}

fn requested_input_value(kind: &UiInputKind, value: &UiSemanticPayloadValue) -> Option<UiInputValue> {
    match (kind, value) {
        (UiInputKind::Bool, UiSemanticPayloadValue::Bool { value }) => Some(UiInputValue::Bool { value: *value }),
        (UiInputKind::I32, UiSemanticPayloadValue::I32 { value }) => Some(UiInputValue::I32 { value: *value }),
        (UiInputKind::U32, UiSemanticPayloadValue::U32 { value }) => Some(UiInputValue::U32 { value: *value }),
        (UiInputKind::F32, UiSemanticPayloadValue::F32 { value }) => Some(UiInputValue::F32 { value: *value }),
        (UiInputKind::Enum { variants }, UiSemanticPayloadValue::Enum { value }) if variants.contains(value) => Some(UiInputValue::Enum { value: value.clone() }),
        _ => None,
    }
}

fn display_value(value: &UiInputValue) -> String {
    match value {
        UiInputValue::Bool { value } => value.to_string(),
        UiInputValue::I32 { value } => value.to_string(),
        UiInputValue::U32 { value } => value.to_string(),
        UiInputValue::F32 { value } => format!("{value:.2}"),
        UiInputValue::Enum { value } => value.clone(),
        _ => "unavailable".into(),
    }
}

pub struct DemoDragDropDomain {
    revision: Option<Revision>,
}

impl DemoDragDropDomain {
    pub fn new() -> Self {
        Self { revision: None }
    }

    pub fn serve(endpoint: SocketAddr) -> Result<(), TransportError> {
        let server = RpcServer::bind(endpoint)?;
        let mut domain = Self::new();
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            (domain.handle(request), !shutdown)
        })
    }

    pub fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        if request.method == "service.shutdown" {
            return accepted(request, self.revision, json!({"state": "accepted"}));
        }
        if request.method == "ui.data_grid.window.request" {
            return self.handle_data_grid_window_request(request);
        }
        if request.method != "ui.drag_drop.apply" {
            return rejected(
                request,
                self.revision,
                "unsupported_method",
                "method is not supported",
            );
        }
        let Some(event_value) = request.params.get("event") else {
            return rejected(
                request,
                self.revision,
                "invalid_request",
                "drag/drop event is required",
            );
        };
        let Some(fragment_value) = request.params.get("fragment") else {
            return rejected(
                request,
                self.revision,
                "invalid_request",
                "current fragment is required",
            );
        };
        let event: UiSemanticEvent =
            match serde_json::from_value::<UiSemanticEvent>(event_value.clone()) {
                Ok(event) if event.event == UiSemanticEventType::DragDrop => event,
                _ => {
                    return rejected(
                        request,
                        self.revision,
                        "invalid_drag_drop",
                        "a drag/drop semantic event is required",
                    );
                }
            };
        let mut fragment: UiFragment = match serde_json::from_value(fragment_value.clone()) {
            Ok(fragment) => fragment,
            Err(_) => {
                return rejected(
                    request,
                    self.revision,
                    "invalid_request",
                    "current fragment is invalid",
                );
            }
        };
        if request.expected_revision != Some(fragment.revision)
            || self
                .revision
                .is_some_and(|revision| revision != fragment.revision)
        {
            return rejected(
                request,
                self.revision,
                "revision_conflict",
                "drag/drop fragment revision is stale",
            );
        }
        let Some(drop) = event.drag_drop else {
            return rejected(
                request,
                self.revision,
                "invalid_drag_drop",
                "drag/drop payload is required",
            );
        };
        let Some(template_key) = drop.presentation_template_key else {
            return rejected(
                request,
                self.revision,
                "presentation_template_required",
                "accepted drops require a target-owned template",
            );
        };
        let source_drag_key = fragment.effects.iter().find_map(|effect| match effect {
            UiEffect::DragBinding { binding } if binding.source_node_id.0 == drop.source_key => {
                Some(binding.key.clone())
            }
            _ => None,
        });
        let declared = fragment.effects.iter().any(|effect| matches!(effect,
            UiEffect::DropBinding { binding }
                if binding.intent == event.intent
                    && binding.target_node_id.0 == drop.target_key
                    && source_drag_key.as_deref() == Some(binding.accepts_drag_key.as_str())
                    && binding.placement == drop.placement
                    && binding.presentation_template_key.as_deref() == Some(template_key.as_str())
        ));
        if !declared {
            return rejected(
                request,
                self.revision,
                "drag_drop_not_declared",
                "source, target, placement, or template is not declared",
            );
        }
        let Some(source) = find_node(&fragment.root, &drop.source_key).cloned() else {
            return rejected(
                request,
                self.revision,
                "source_not_found",
                "drag source is not present",
            );
        };
        let Some(target) = find_node(&fragment.root, &drop.target_key) else {
            return rejected(
                request,
                self.revision,
                "target_not_found",
                "drop target is not present",
            );
        };
        let Some(template) = target
            .children
            .iter()
            .find(|node| node.node_id.0 == template_key)
            .cloned()
        else {
            return rejected(
                request,
                self.revision,
                "template_not_owned",
                "presentation template is not owned by the drop target",
            );
        };
        if drop.placement != UiDropPlacement::Into
            && find_parent(&fragment.root, &drop.target_key).is_none()
        {
            return rejected(
                request,
                self.revision,
                "relative_target_has_no_parent",
                "before and after targets must have a parent",
            );
        }
        let label = first_literal(&source).unwrap_or_else(|| source.node_id.0.clone());
        let next_revision = Revision(fragment.revision.0 + 1);
        let mut representation = template;
        namespace_node(
            &mut representation,
            &format!("{}-{}-r{}", template_key, drop.source_key, next_revision.0),
        );
        set_first_literal(&mut representation, &label);
        remove_node(&mut fragment.root, &drop.source_key);
        fragment.effects.retain(|effect| match effect {
            UiEffect::DragBinding { binding } => binding.source_node_id.0 != drop.source_key,
            UiEffect::DropBinding { binding } => {
                source_drag_key.as_deref() != Some(binding.accepts_drag_key.as_str())
            }
            _ => true,
        });
        match drop.placement {
            UiDropPlacement::Into => find_node_mut(&mut fragment.root, &drop.target_key)
                .expect("validated target remains present")
                .children
                .push(representation),
            UiDropPlacement::Before | UiDropPlacement::After => {
                let parent = find_parent_mut(&mut fragment.root, &drop.target_key)
                    .expect("validated relative target retains its parent");
                let target_index = parent
                    .children
                    .iter()
                    .position(|child| child.node_id.0 == drop.target_key)
                    .expect("validated relative target remains present");
                let insertion_index = if drop.placement == UiDropPlacement::Before {
                    target_index
                } else {
                    target_index + 1
                };
                parent.children.insert(insertion_index, representation);
            }
        }
        fragment.revision = next_revision;
        if fragment.validate().is_err() {
            return rejected(
                request,
                self.revision,
                "invalid_fragment",
                "accepted domain revision is invalid",
            );
        }
        self.revision = Some(next_revision);
        accepted(
            request,
            self.revision,
            json!({"fragment": fragment, "state": "accepted"}),
        )
    }

    fn handle_data_grid_window_request(&self, request: RpcRequest) -> RpcResponse {
        let window_request: UiDataGridWindowRequest = match serde_json::from_value(request.params.clone()) {
            Ok(window_request) => window_request,
            Err(_) => return rejected(request, self.revision, "invalid_request", "a typed DataGrid window request is required"),
        };
        if window_request.data_grid_key != "virtual-list" || window_request.max_window_rows == 0 {
            return rejected(request, self.revision, "data_grid_not_found", "the requested DataGrid is not available");
        }
        if window_request.expected_list_revision != Revision(1) {
            return rejected(request, Some(Revision(1)), "revision_conflict", "the requested DataGrid list revision is stale");
        }
        const TOTAL_ROWS: u64 = 10_000;
        let first_row = window_request.requested_first_row.min(TOTAL_ROWS);
        let row_count = u64::from(window_request.max_window_rows.min(12)).min(TOTAL_ROWS - first_row);
        let window_rows = (first_row..first_row + row_count).map(virtual_list_row).collect();
        let frame = UiDataGridFrame {
            data_grid_key: window_request.data_grid_key,
            list_revision: Revision(1),
            total_rows: TOTAL_ROWS,
            first_row,
            window_rows,
            expected_program_revision: virtual_list_program_revision(),
        };
        accepted(request, Some(frame.list_revision), json!(frame))
    }
}

fn virtual_list_row(row_index: u64) -> UiDataGridWindowRow {
    let handle = |id| neon_ui_schema::UiTextHandle { id, generation: 1 };
    let base = 10_000 + row_index * 4;
    UiDataGridWindowRow {
        stable_row_key: format!("virtual-row-{row_index}"),
        cells: std::collections::BTreeMap::from([
            ("id".into(), UiDataGridCell { value: UiInputValue::I32 { value: row_index as i32 }, display: handle(base), presentation_override: None }),
            ("name".into(), UiDataGridCell { value: UiInputValue::TextHandle { value: handle(base + 1) }, display: handle(base + 1), presentation_override: None }),
            ("status".into(), UiDataGridCell { value: UiInputValue::TextHandle { value: handle(base + 2) }, display: handle(base + 2), presentation_override: None }),
            ("owner".into(), UiDataGridCell { value: UiInputValue::TextHandle { value: handle(base + 3) }, display: handle(base + 3), presentation_override: None }),
        ]),
    }
}

fn virtual_list_program_revision() -> UiProgramRevision {
    UiProgramRevision {
        program_id: "virtual-list-demo.demo".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
        ].into_iter().map(|name| UiProgramCapability {
            name: name.into(), version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        }).collect(),
    }
}

fn find_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    (node.node_id.0 == key)
        .then_some(node)
        .or_else(|| node.children.iter().find_map(|child| find_node(child, key)))
}

fn find_node_mut<'a>(node: &'a mut UiNode, key: &str) -> Option<&'a mut UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, key))
}

fn find_parent<'a>(node: &'a UiNode, child_key: &str) -> Option<&'a UiNode> {
    node.children
        .iter()
        .any(|child| child.node_id.0 == child_key)
        .then_some(node)
        .or_else(|| {
            node.children
                .iter()
                .find_map(|child| find_parent(child, child_key))
        })
}

fn find_parent_mut<'a>(node: &'a mut UiNode, child_key: &str) -> Option<&'a mut UiNode> {
    if node
        .children
        .iter()
        .any(|child| child.node_id.0 == child_key)
    {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_parent_mut(child, child_key))
}

fn remove_node(node: &mut UiNode, key: &str) -> bool {
    if let Some(index) = node
        .children
        .iter()
        .position(|child| child.node_id.0 == key)
    {
        node.children.remove(index);
        return true;
    }
    node.children
        .iter_mut()
        .any(|child| remove_node(child, key))
}

fn first_literal(node: &UiNode) -> Option<String> {
    match &node.text {
        Some(TextRef::Literal { value }) => Some(value.clone()),
        _ => node.children.iter().find_map(first_literal),
    }
}

fn set_first_literal(node: &mut UiNode, value: &str) -> bool {
    if matches!(node.text, Some(TextRef::Literal { .. })) {
        node.text = Some(TextRef::Literal {
            value: value.into(),
        });
        return true;
    }
    node.children
        .iter_mut()
        .any(|child| set_first_literal(child, value))
}

fn namespace_node(node: &mut UiNode, prefix: &str) {
    node.node_id = UiNodeId(format!("{prefix}-{}", node.node_id.0));
    for child in &mut node.children {
        namespace_node(child, prefix);
    }
}

fn accepted(request: RpcRequest, revision: Option<Revision>, result: Value) -> RpcResponse {
    RpcResponse {
        request_id: request.request_id,
        status: RpcStatus::Accepted,
        revision,
        result: Some(result),
        snapshot: None,
        error: None,
    }
}

fn rejected(
    request: RpcRequest,
    revision: Option<Revision>,
    code: &str,
    message: &str,
) -> RpcResponse {
    RpcResponse {
        request_id: request.request_id,
        status: RpcStatus::Rejected,
        revision,
        result: None,
        snapshot: None,
        error: Some(RpcError {
            code: code.into(),
            message: message.into(),
            current_revision: revision,
            object_id: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_nui_flow_program, lower_nui_flow_effects, parse_nui_flow, UiProgramSemanticEventRouter};
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId, ServiceName};
    use neon_ui_schema::UiFragmentId;

    fn fragment() -> UiFragment {
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/kanban-reparent-workbench.nui"
        ))
        .unwrap();
        let effects = lower_nui_flow_effects(&document);
        UiFragment {
            fragment_id: UiFragmentId("demo".into()),
            revision: Revision(1),
            root: document.ir.root,
            effects,
        }
    }
    fn request(
        fragment: UiFragment,
        drop_key: &str,
        source_key: &str,
        target_key: &str,
        placement: UiDropPlacement,
        template_key: &str,
    ) -> RpcRequest {
        let intent = fragment
            .effects
            .iter()
            .find_map(|effect| match effect {
                UiEffect::DropBinding { binding } if binding.key == drop_key => {
                    Some(binding.intent.clone())
                }
                _ => None,
            })
            .unwrap();
        let event = UiSemanticEvent {
            event: UiSemanticEventType::DragDrop,
            event_id: "drop-1".into(),
            renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: fragment.fragment_id.clone(),
                revision: fragment.revision,
            },
            intent,
            pointer: None,
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: Some(neon_ui_schema::UiDragDropPayload {
                source_key: source_key.into(),
                target_key: target_key.into(),
                placement,
                presentation_template_key: Some(template_key.into()),
            }),
        };
        RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("drop-1".into()),
            client: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "test".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName("demo-domain".into()),
            method: "ui.drag_drop.apply".into(),
            params: json!({"event": event, "fragment": fragment}),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("drop-1".into()),
        }
    }
    #[test]
    fn accepted_drop_returns_a_revisioned_target_template_representation() {
        let mut domain = DemoDragDropDomain { revision: None };
        let response = domain.handle(request(
            fragment(),
            "progress-drop",
            "backlog-card-01",
            "in-progress-panel",
            UiDropPlacement::Into,
            "progress-template",
        ));
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        let updated: UiFragment =
            serde_json::from_value(response.result.unwrap()["fragment"].clone()).unwrap();
        assert_eq!(updated.revision, Revision(2));
        assert!(find_node(&updated.root, "backlog-card-01").is_none());
        assert!(find_node(
                &updated.root,
                "progress-template-backlog-card-01-r2-progress-template"
            )
        .is_some());
    }

    #[test]
    fn accepted_relative_drops_insert_target_template_representations_as_siblings() {
        let mut domain = DemoDragDropDomain::new();
        let before = domain.handle(request(
            fragment(),
            "progress-audit-drop",
            "backlog-card-02",
            "in-progress-panel",
            UiDropPlacement::Before,
            "progress-template",
        ));
        assert_eq!(before.status, RpcStatus::Accepted, "{:?}", before.error);
        let before_fragment: UiFragment =
            serde_json::from_value(before.result.unwrap()["fragment"].clone()).unwrap();
        let board_columns = find_node(&before_fragment.root, "board-columns").unwrap();
        let progress_index = board_columns
            .children
            .iter()
            .position(|node| node.node_id.0 == "in-progress-panel")
            .unwrap();
        assert_eq!(
            board_columns.children[progress_index - 1].node_id.0,
            "progress-template-backlog-card-02-r2-progress-template"
        );

        let after = DemoDragDropDomain::new().handle(request(
            fragment(),
            "done-bindings-drop",
            "backlog-card-03",
            "done-panel",
            UiDropPlacement::After,
            "accepted-template",
        ));
        assert_eq!(after.status, RpcStatus::Accepted, "{:?}", after.error);
        let after_fragment: UiFragment =
            serde_json::from_value(after.result.unwrap()["fragment"].clone()).unwrap();
        let board_columns = find_node(&after_fragment.root, "board-columns").unwrap();
        let done_index = board_columns
            .children
            .iter()
            .position(|node| node.node_id.0 == "done-panel")
            .unwrap();
        assert_eq!(
            board_columns.children[done_index + 1].node_id.0,
            "accepted-template-backlog-card-03-r2-accepted-template"
        );
    }

    #[test]
    fn virtual_list_window_request_returns_a_bounded_generated_frame() {
        let request = UiDataGridWindowRequest {
            renderer_epoch: 1,
            composition_revision: Revision(7),
            fragment: neon_ui_schema::UiFragmentRevision { id: UiFragmentId("virtual-list-demo".into()), revision: Revision(3) },
            data_grid_key: "virtual-list".into(),
            expected_list_revision: Revision(1),
            requested_first_row: 9_996,
            max_window_rows: 99,
            sequence: 4,
        };
        let response = DemoDragDropDomain::new().handle(RpcRequest {
            protocol: "neon3.rpc".into(), version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("virtual-list-window".into()),
            client: ClientIdentity { kind: ClientKind::UiRuntime, instance_id: "test".into(), pid: 1, origin: "test".into() },
            target: ServiceName("demo-domain".into()), method: "ui.data_grid.window.request".into(),
            params: json!(request), expected_revision: Some(Revision(1)), idempotency_key: Some("virtual-list-window".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        let frame: UiDataGridFrame = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(frame.total_rows, 10_000);
        assert_eq!(frame.first_row, 9_996);
        assert_eq!(frame.window_rows.len(), 4);
        assert_eq!(frame.window_rows[0].stable_row_key, "virtual-row-9996");
    }

    #[test]
    fn component_gallery_headless_scenario_accepts_events_and_publishes_visible_status() {
        let document = parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        )).unwrap();
        let revision = program_revision();
        let program = compile_nui_flow_program(&document, revision.clone()).unwrap();
        let mut domain = DemoInputDomain::new(program.clone(), document.input_schema.clone()).unwrap();
        let mut router = UiProgramSemanticEventRouter::new(program.clone(), domain.snapshot().inputs, 7);
        for (index, node_key) in [
            "feature-toggle", "mode-radio", "exposure-slider", "count-drag", "mode-combo",
            "mode-dropdown", "item-selectable", "item-list", "gallery-scroll",
        ].iter().enumerate() {
            let snapshot = domain.snapshot();
            let declaration = program.event_records.iter().find(|event| event.node_key == *node_key).unwrap();
            let payload = declaration.bound_input_keys.iter().map(|key| {
                (key.clone(), match &snapshot.inputs.values[key].value {
                    UiInputValue::Bool { value } => neon_ui_schema::UiSemanticPayloadValue::Bool { value: *value },
                    UiInputValue::I32 { value } => neon_ui_schema::UiSemanticPayloadValue::I32 { value: *value },
                    UiInputValue::F32 { value } => neon_ui_schema::UiSemanticPayloadValue::F32 { value: *value },
                    UiInputValue::Enum { value } => neon_ui_schema::UiSemanticPayloadValue::Enum { value: value.clone() },
                            _ => unreachable!(),
                })
            }).collect();
            let event = UiProgramSemanticEvent {
                event_id: format!("gallery-scenario-{index}"),
                kind: if matches!(*node_key, "feature-toggle" | "mode-radio" | "mode-combo" | "item-selectable") { neon_ui_schema::UiProgramSemanticEventKind::SelectionChanged }
                    else { neon_ui_schema::UiProgramSemanticEventKind::ValueCommit },
                intent: declaration.intent.clone(), source_node_key: (*node_key).into(), payload,
                program_revision: revision.clone(), input_revision: snapshot.inputs.input_revision,
                request_id: format!("gallery-request-{index}"), idempotency_key: format!("gallery-key-{index}"), requested_value: None,
                interaction: neon_ui_schema::UiSemanticInteractionMetadata { interaction_id: format!("gallery-interaction-{index}"), sequence: index as u64 + 1, renderer_epoch: 7 },
            };
            assert_eq!(router.validate(&event).status, neon_ui_schema::UiProgramSemanticEventStatus::Accepted);
            let updated = domain.apply(&event).unwrap();
            assert_eq!(updated.inputs.input_revision, Revision(index as u64 + 1));
            assert!(updated.visible_status.values().any(|status| status != ""));
            router.replace_resolved_inputs(updated.inputs);
        }
        let snapshot = domain.snapshot();
        assert_eq!(snapshot.inputs.values["feature_enabled"].value, UiInputValue::Bool { value: false });
        assert_eq!(snapshot.inputs.values["combo_choice"].value, UiInputValue::Enum { value: "gamma".into() });
        assert_eq!(snapshot.inputs.values["dropdown_choice"].value, UiInputValue::Enum { value: "gamma".into() });
        assert_eq!(snapshot.inputs.values["list_choice"].value, UiInputValue::Enum { value: "gamma".into() });
        assert_eq!(snapshot.visible_status["status-dropdown_choice"], "dropdown_choice: gamma");
        let mut fragment = UiFragment {
            fragment_id: neon_ui_schema::UiFragmentId("gallery-scenario".into()),
            revision: Revision(2), root: document.ir.root.clone(), effects: Vec::new(),
        };
        apply_visible_status_to_fragment(&mut fragment, &snapshot);
        assert_eq!(first_literal(find_node(&fragment.root, "status-list_choice").unwrap()).as_deref(), Some("Selected mode: gamma"));
        assert!(fragment.effects.iter().any(|effect| matches!(
            effect,
            UiEffect::ControlPresentation {
                node_id,
                state: UiControlPresentation::Toggle { selected: false }
            } if node_id.0 == "feature-toggle"
        )));
    }

    fn program_revision() -> neon_ui_schema::UiProgramRevision {
        use neon_ui_schema::{UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus, UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME, UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME};
        neon_ui_schema::UiProgramRevision {
            program_id: "component-gallery-test".into(), revision: Revision(1), schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: [UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME].into_iter().map(|name| UiProgramCapability { name: name.into(), version: 1, owner: UiProgramCapabilityOwner::SharedContract, status: UiProgramCapabilityStatus::Supported }).collect(),
        }
    }
}
