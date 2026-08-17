//! Generic local demo domain endpoint for accepted drag/drop revisions.
//!
//! It consumes declared semantic keys and returns a new fragment. It never
//! reaches into the renderer or uses renderer hit identifiers.

use std::net::SocketAddr;

use neon_ipc::{RpcServer, TransportError};
use neon_protocol::{AssetRef, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus};
use neon_ui_schema::{
    TextRef, UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiControlPresentation, UiDataGridCell,
    UiDataGridFrame, UiDataGridInputFrame, UiDataGridWindowRequest, UiDataGridWindowRow,
    UiDropPlacement, UiEffect, UiFragment, UiHostInbound, UiHostPresentationUpdate,
    UiHostPublication, UiInputChange, UiInputFrame, UiInputKind, UiInputSchema, UiInputValue,
    UiNode, UiNodeId, UiProgram, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramRevision, UiProgramSemanticEvent,
    UiProgramSemanticEventKind, UiProgramSemanticEventStatus, UiResolvedInputs, UiSemanticEvent,
    UiSemanticEventType, UiSemanticPayloadValue,
};
use serde_json::{Value, json};

use crate::{
    UiInputStore, UiInputStoreError, UiInputWriter, UiProgramSemanticEventRouter,
    UiVariableEventPublisher, compile_nui_flow_program, host_adapter::UiHostAdapterConfig,
    instantiate_ui_template, parse_nui_flow,
};

/// Generic controlled-input demo domain. It resolves the slot from the program
/// declaration, not from a renderer control kind or renderer identity.
pub struct DemoInputDomain {
    program: UiProgram,
    inputs: UiInputStore,
    /// Optional eventd forwarder for `nui.variable.changed` observations.
    publisher: Option<UiVariableEventPublisher>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DemoInputDomainSnapshot {
    pub inputs: UiResolvedInputs,
    pub visible_status: std::collections::BTreeMap<String, String>,
}

impl DemoInputDomain {
    pub fn new(program: UiProgram, schema: UiInputSchema) -> Result<Self, UiInputStoreError> {
        let inputs = UiInputStore::activate(program.revision.clone(), schema)?;
        Ok(Self {
            program,
            inputs,
            publisher: None,
        })
    }

    /// Attaches an event publisher derived from the active input schema. Only
    /// variables that declared `emitevent` produce directed
    /// `flow.<flow_name>.<variable_key>` events; the flow name comes from the
    /// Flow `flow <name>` declaration.
    pub fn with_publisher(mut self, publisher: UiVariableEventPublisher) -> Self {
        self.publisher = Some(publisher);
        self
    }

    /// Builds a publisher from the active schema's `flow_name` and
    /// `emit_event_keys`, then attaches it.
    pub fn with_schema_publisher(
        mut self,
        endpoint: Option<std::net::SocketAddr>,
        client: neon_protocol::ClientIdentity,
        module: impl Into<String>,
        surface: impl Into<String>,
    ) -> Self {
        let schema = self.inputs.schema().clone();
        let publisher = UiVariableEventPublisher::new(
            endpoint,
            client,
            module,
            surface,
            schema.flow_name.clone(),
            schema.emit_event_keys.clone(),
        );
        self.publisher = Some(publisher);
        self
    }

    pub fn snapshot(&self) -> DemoInputDomainSnapshot {
        let inputs = self.inputs.snapshot();
        let visible_status = inputs
            .values
            .iter()
            .map(|(key, value)| {
                let label = match key.as_str() {
                    "list_choice" | "tabs_choice" => "Selected mode",
                    "drag_value" => "Current count",
                    _ => key,
                };
                (
                    format!("status-{key}"),
                    format!("{label}: {}", display_value(&value.value)),
                )
            })
            .collect();
        DemoInputDomainSnapshot {
            inputs,
            visible_status,
        }
    }

    pub fn apply(
        &mut self,
        event: &UiProgramSemanticEvent,
    ) -> Result<DemoInputDomainSnapshot, &'static str> {
        let declaration = self
            .program
            .event_records
            .iter()
            .find(|declaration| {
                declaration.node_key == event.source_node_key && declaration.intent == event.intent
            })
            .ok_or("semantic event is not declared by the source node")?;
        let key = declaration
            .bound_input_keys
            .iter()
            .find(|key| {
                self.program.binding_records.iter().any(|binding| {
                    binding.node_key == event.source_node_key
                        && binding.input_key == **key
                        && matches!(
                            binding.property,
                            neon_ui_schema::UiBoundProperty::Active
                                | neon_ui_schema::UiBoundProperty::Enabled
                                | neon_ui_schema::UiBoundProperty::Selected
                                | neon_ui_schema::UiBoundProperty::NumericValue
                                | neon_ui_schema::UiBoundProperty::StateToken
                                | neon_ui_schema::UiBoundProperty::TextValue
                        )
                })
            })
            .ok_or("semantic event has no controlled input binding")?;
        let slot = self
            .inputs
            .schema()
            .slots
            .iter()
            .find(|slot| slot.key == *key)
            .ok_or("controlled input slot is missing")?;
        let current_inputs = self.inputs.snapshot();
        let current = current_inputs
            .values
            .get(key)
            .ok_or("controlled input value is missing")?;
        let value = if event.intent == "gallery.button.activate" {
            advance_value(&slot.kind, &current.value)
                .ok_or("controlled input kind is not supported")?
        } else {
            match &event.requested_value {
                Some(requested) => requested_input_value(&slot.kind, requested)
                    .ok_or("requested control value does not match the bound input kind")?,
                None => advance_value(&slot.kind, &current.value)
                    .ok_or("controlled input kind is not supported")?,
            }
        };
        let result = self
            .inputs
            .apply(
                UiInputWriter::External,
                UiInputFrame {
                    program_revision: self.program.revision.clone(),
                    expected_input_revision: current_inputs.input_revision,
                    request_id: event.request_id.clone(),
                    idempotency_key: event.idempotency_key.clone(),
                    changes: vec![UiInputChange {
                        key: key.clone(),
                        value,
                    }],
                },
            )
            .map_err(|_| "controlled input frame was rejected")?;
        let snapshot = self.snapshot();
        if let Some(publisher) = &self.publisher {
            let _ = publisher.publish_variable_changes(&result.variable_changes);
        }
        Ok(snapshot)
    }

    /// Runs the component-gallery program through the generic UI host boundary.
    /// It owns only typed inputs and bounded DataGrid frames.
    pub fn serve_component_gallery(
        endpoint: SocketAddr,
        image_asset: AssetRef,
    ) -> Result<(), TransportError> {
        Self::serve_component_gallery_server(RpcServer::bind(endpoint)?, image_asset)
    }

    fn serve_component_gallery_server(
        server: RpcServer,
        image_asset: AssetRef,
    ) -> Result<(), TransportError> {
        let (document, program) = component_gallery_program(image_asset)
            .map_err(|error| TransportError::Io(std::io::Error::other(error)))?;
        let input_schema = document.input_schema.clone();
        let mut domain = Self::new(program.clone(), input_schema.clone())
            .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?;
        let mut router =
            UiProgramSemanticEventRouter::new(program.clone(), domain.snapshot().inputs, 1);
        let mut grid = DemoDragDropDomain::new();
        let grid_record = program
            .data_grid_records
            .iter()
            .find(|record| record.source_key == "asset_window")
            .cloned()
            .ok_or_else(|| {
                TransportError::Io(std::io::Error::other(
                    "component gallery DataGrid declaration is missing",
                ))
            })?;
        let grid_max_window_rows = grid_record.max_window_rows;
        let declared_grid_columns = grid_record
            .columns
            .iter()
            .map(|column| column.key.as_str())
            .collect::<Vec<_>>();
        let mut active_grid = Some(grid.virtual_list_window_frame(
            0,
            grid_max_window_rows,
            program.revision.clone(),
            &declared_grid_columns,
        ));
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let response = if shutdown {
                accepted(request, Some(domain.snapshot().inputs.input_revision), json!({"state": "accepted"}))
            } else if request.method == "ui.host.adapter.get" {
                accepted(request, Some(domain.snapshot().inputs.input_revision), json!(UiHostAdapterConfig {
                    program: program.clone(), input_schema: input_schema.clone(),
                }))
            } else if request.method == "ui.host.inbound" {
                let inbound = serde_json::from_value::<UiHostInbound>(request.params.clone());
                match inbound {
                    Ok(UiHostInbound::SemanticIntent { event }) => {
                        let validation = router.validate(&event);
                        if validation.status != UiProgramSemanticEventStatus::Accepted {
                            rejected(request, Some(domain.snapshot().inputs.input_revision), validation.code.as_deref().unwrap_or("semantic_event_rejected"), &validation.message)
                        } else {
                            let before = domain.snapshot().inputs.input_revision;
                            match domain.apply(&event) {
                                Ok(snapshot) => {
                                    router.replace_resolved_inputs(snapshot.inputs.clone());
                                    let changes = snapshot.inputs.changed_slots.iter().filter_map(|key| snapshot.inputs.values.get(key).map(|value| UiInputChange { key: key.clone(), value: value.value.clone() })).collect();
                                    let frame = grid.virtual_list_window_frame(0, grid_max_window_rows, program.revision.clone(), &["name", "status", "owner", "notes"]);
                                    active_grid = Some(frame.clone());
                                    accepted(request, Some(snapshot.inputs.input_revision), json!(UiHostPublication {
                                        scalar_frame: UiInputFrame { program_revision: program.revision.clone(), expected_input_revision: before, request_id: event.request_id, idempotency_key: event.idempotency_key, changes },
                                        grid_inputs: vec![UiDataGridInputFrame { source_key: "asset_window".into(), frame }],
                                        presentation_update: None,
                                    }))
                                }
                                Err(message) => rejected(request, Some(domain.snapshot().inputs.input_revision), "domain_input_rejected", message),
                            }
                        }
                    }
                    Ok(UiHostInbound::WindowRequest { request: neon_ui_schema::UiWindowRequest::DataGrid { request: window } }) => {
                        let before = domain.snapshot().inputs.input_revision;
                        let frame_response = grid.handle_data_grid_window_request(RpcRequest { params: json!(window), ..request.clone() });
                        match frame_response.result.and_then(|value| serde_json::from_value::<UiDataGridFrame>(value).ok()) {
                            Some(mut frame) => {
                                frame.expected_program_revision = program.revision.clone();
                                for row in &mut frame.window_rows {
                                    row.cells.retain(|key, _| matches!(key.as_str(), "name" | "status" | "owner" | "notes"));
                                }
                                let scalar_frame = UiInputFrame { program_revision: program.revision.clone(), expected_input_revision: before, request_id: request.request_id.0.clone(), idempotency_key: request.idempotency_key.clone().unwrap_or_default(), changes: Vec::new() };
                                if domain.inputs.apply(UiInputWriter::External, scalar_frame.clone()).is_err() {
                                    rejected(request, Some(before), "domain_input_rejected", "host input frame was rejected")
                                } else {
                                    router.replace_resolved_inputs(domain.snapshot().inputs);
                                    active_grid = Some(frame.clone());
                                    accepted(request, Some(frame.list_revision), json!(UiHostPublication { scalar_frame, grid_inputs: vec![UiDataGridInputFrame { source_key: "asset_window".into(), frame }], presentation_update: None }))
                                }
                            }
                            None => rejected(request, Some(grid.virtual_list_revision), "data_grid_window_rejected", "DataGrid window request was rejected"),
                        }
                    }
                    Ok(UiHostInbound::DataGridCell { event }) => {
                        let before = domain.snapshot().inputs.input_revision;
                        let Some(current_frame) = active_grid.clone() else {
                            return (rejected(request, Some(grid.virtual_list_revision), "ui_host_grid_unavailable", "DataGrid cell mutation has no active grid frame"), !shutdown);
                        };
                        let frame_response = grid.handle_virtual_list_cell_event(RpcRequest {
                            params: json!({ "data_grid_cell": event.data_grid_cell, "data_grid_frame": current_frame, "control_value": event.control_value, "text": event.text }),
                            method: match event.intent { neon_ui_schema::UiIntent::Invoke { action, .. } => action },
                            ..request.clone()
                        });
                        match frame_response.result.and_then(|value| serde_json::from_value::<UiDataGridFrame>(value).ok()) {
                            Some(frame) => {
                                let scalar_frame = UiInputFrame { program_revision: program.revision.clone(), expected_input_revision: before, request_id: request.request_id.0.clone(), idempotency_key: request.idempotency_key.clone().unwrap_or_default(), changes: Vec::new() };
                                if domain.inputs.apply(UiInputWriter::External, scalar_frame.clone()).is_err() {
                                    rejected(request, Some(before), "domain_input_rejected", "host input frame was rejected")
                                } else {
                                    router.replace_resolved_inputs(domain.snapshot().inputs);
                                    active_grid = Some(frame.clone());
                                    accepted(request, Some(frame.list_revision), json!(UiHostPublication { scalar_frame, grid_inputs: vec![UiDataGridInputFrame { source_key: "asset_window".into(), frame }], presentation_update: None }))
                                }
                            }
                            None => rejected(request, Some(grid.virtual_list_revision), "data_grid_cell_rejected", "DataGrid cell mutation was rejected"),
                        }
                    }
                    Ok(UiHostInbound::DragDrop { event, active_fragment }) => {
                        let before = domain.snapshot().inputs.input_revision;
                        let active_fragment = active_fragment.into_fragment();
                        let semantic_event = UiSemanticEvent {
                            event: UiSemanticEventType::DragDrop,
                            event_id: event.event_id.clone(),
                            renderer_epoch: event.interaction.renderer_epoch,
                            composition_revision: active_fragment.revision,
                            fragment: neon_ui_schema::UiFragmentRevision {
                                id: active_fragment.fragment_id.clone(),
                                revision: active_fragment.revision,
                            },
                            intent: neon_ui_schema::UiIntent::Invoke { action: event.intent.clone(), params: json!({}) },
                            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence: event.interaction.sequence }),
                            focus: None,
                            data_grid_cell: None,
                            text: None,
                            control_value: None,
                            drag_drop: Some(event.payload.clone()),
                        };
                        let applied = grid.handle(RpcRequest {
                            params: json!({"event": semantic_event, "fragment": active_fragment}),
                            method: "ui.drag_drop.apply".into(),
                            expected_revision: Some(active_fragment.revision),
                            ..request.clone()
                        });
                        let replacement = applied.result.and_then(|value| value.get("fragment").cloned())
                            .and_then(|value| serde_json::from_value::<UiFragment>(value).ok());
                        match replacement {
                            Some(mut replacement_fragment) => {
                                if let Some(frame) = active_grid.clone()
                                    && !replacement_fragment.effects.iter().any(|effect| matches!(effect, UiEffect::DataGridFrame { declaration, .. } if declaration.source_key == "asset_window"))
                                    && let Some(record) = program.data_grid_records.iter().find(|record| record.source_key == "asset_window") {
                                    replacement_fragment.effects.push(UiEffect::DataGridFrame {
                                        declaration: neon_ui_schema::UiDataGridDeclaration {
                                            node_key: record.node_key.clone(), source_key: record.source_key.clone(), max_window_rows: record.max_window_rows,
                                            row_height: record.row_height, overscan: record.overscan, columns: record.columns.clone(),
                                        },
                                        frame,
                                    });
                                }
                                accepted(request, Some(replacement_fragment.revision), json!(UiHostPublication {
                                    scalar_frame: UiInputFrame {
                                        program_revision: program.revision.clone(), expected_input_revision: before,
                                        request_id: event.request_id.clone(), idempotency_key: event.idempotency_key.clone(), changes: Vec::new(),
                                    },
                                    grid_inputs: Vec::new(),
                                    presentation_update: Some(UiHostPresentationUpdate {
                                        expected_fragment_revision: active_fragment.revision,
                                        replacement_fragment,
                                        replacement_program: program.clone(),
                                        replacement_input_schema: input_schema.clone(),
                                    }),
                                }))
                            }
                            None => {
                                let message = applied.error.as_ref().map(|error| error.message.as_str())
                                    .unwrap_or("declared inventory drop was rejected by the demo adapter");
                                rejected(request, Some(active_fragment.revision), "inventory_drop_rejected", message)
                            }
                        }
                    }
                    Err(_) => rejected(request, Some(domain.snapshot().inputs.input_revision), "invalid_request", "a typed UI host inbound request is required"),
                }
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

pub fn component_gallery_program(
    image_asset: AssetRef,
) -> Result<(neon_ui_schema::NuiFlowDocument, neon_ui_schema::UiProgram), String> {
    let mut document = parse_nui_flow(include_str!(
        "../../../tests/fixtures/ui/imgui-component-gallery.nui"
    ))
    .map_err(|error| format!("component gallery fixture is invalid: {error:?}"))?;
    let bindings = std::collections::HashMap::from([("gallery-image".into(), image_asset)]);
    crate::bind_nui_flow_resources(&mut document, &bindings)
        .map_err(|error| format!("component gallery resource binding failed: {error:?}"))?;
    let revision = UiProgramRevision {
        program_id: "surface.component-gallery.demo".into(),
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
            name: name.into(),
            version: 1,
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
            node.text = Some(TextRef::Literal {
                value: value.clone(),
            });
        }
        for child in &mut node.children {
            visit(child, status);
        }
    }
    visit(&mut fragment.root, &snapshot.visible_status);
    fragment
        .effects
        .retain(|effect| !matches!(effect, UiEffect::ControlPresentation { .. }));
    let presentation = |node_id: &str, state| UiEffect::ControlPresentation {
        node_id: UiNodeId(node_id.into()),
        state,
    };
    let values = &snapshot.inputs.values;
    if let Some(UiInputValue::Bool { value }) =
        values.get("feature_enabled").map(|value| &value.value)
    {
        fragment.effects.push(presentation(
            "feature-toggle",
            UiControlPresentation::Toggle { selected: *value },
        ));
    }
    if let Some(UiInputValue::Bool { value }) =
        values.get("radio_selected").map(|value| &value.value)
    {
        fragment.effects.push(presentation(
            "mode-radio",
            UiControlPresentation::Toggle { selected: *value },
        ));
    }
    if let Some(UiInputValue::F32 { value }) = values.get("slider_value").map(|value| &value.value)
    {
        fragment.effects.push(presentation(
            "exposure-slider",
            UiControlPresentation::Numeric {
                value: *value,
                min: 0.0,
                max: 1.0,
            },
        ));
    }
    if let Some(UiInputValue::I32 { value }) = values.get("drag_value").map(|value| &value.value) {
        fragment.effects.push(presentation(
            "count-drag",
            UiControlPresentation::Numeric {
                value: *value as f32,
                min: 0.0,
                max: 24.0,
            },
        ));
    }
    for (node_id, input_key) in [
        ("mode-combo", "combo_choice"),
        ("mode-dropdown", "dropdown_choice"),
        ("mode-tabs", "tabs_choice"),
        ("item-list", "list_choice"),
    ] {
        if let Some(UiInputValue::Enum { value }) = values.get(input_key).map(|value| &value.value)
        {
            let selected = matches!(node_id, "mode-radio" | "item-selectable") && value == "alpha";
            fragment.effects.push(presentation(
                node_id,
                UiControlPresentation::Choice {
                    token: value.clone(),
                    options: vec!["alpha".into(), "beta".into(), "gamma".into()],
                    selected,
                },
            ));
        }
    }
    if let Some(UiInputValue::Bool { value }) =
        values.get("item_selected").map(|value| &value.value)
    {
        fragment.effects.push(presentation(
            "item-selectable",
            UiControlPresentation::Toggle { selected: *value },
        ));
    }
    if let Some(UiInputValue::F32 { value }) =
        values.get("scroll_position").map(|value| &value.value)
    {
        fragment.effects.push(presentation(
            "gallery-scroll",
            UiControlPresentation::Scroll { position: *value },
        ));
    }
}

/// Maps a component-gallery control node to its program semantic event kind.
/// Intent names are unique per control, so the forwarder resolves the source
/// node from the program declaration instead of a renderer identity.
pub fn gallery_event_kind(node_key: &str) -> UiProgramSemanticEventKind {
    match node_key {
        "feature-toggle" | "mode-radio" | "mode-combo" | "mode-dropdown" | "mode-tabs"
        | "item-selectable" | "item-list" => UiProgramSemanticEventKind::SelectionChanged,
        "action-button" => UiProgramSemanticEventKind::Activate,
        "gallery-text" => UiProgramSemanticEventKind::TextEditCommit,
        _ => UiProgramSemanticEventKind::ValueCommit,
    }
}

fn advance_value(kind: &UiInputKind, value: &UiInputValue) -> Option<UiInputValue> {
    Some(match (kind, value) {
        (UiInputKind::Bool, UiInputValue::Bool { value }) => UiInputValue::Bool { value: !value },
        (UiInputKind::I32, UiInputValue::I32 { value }) => UiInputValue::I32 {
            value: value.saturating_add(1),
        },
        (UiInputKind::I32Range { maximum, .. }, UiInputValue::I32 { value }) => UiInputValue::I32 {
            value: value.saturating_add(1).min(*maximum),
        },
        (UiInputKind::U32, UiInputValue::U32 { value }) => UiInputValue::U32 {
            value: value.saturating_add(1),
        },
        (UiInputKind::U32Range { maximum, .. }, UiInputValue::U32 { value }) => UiInputValue::U32 {
            value: value.saturating_add(1).min(*maximum),
        },
        (UiInputKind::F32, UiInputValue::F32 { value }) => UiInputValue::F32 {
            value: (value + 0.1).min(1.0),
        },
        (UiInputKind::F32Range { maximum, .. }, UiInputValue::F32 { value }) => UiInputValue::F32 {
            value: (value + 0.1).min(*maximum),
        },
        (UiInputKind::Enum { variants }, UiInputValue::Enum { value }) => UiInputValue::Enum {
            value: variants
                .iter()
                .cycle()
                .skip_while(|variant| *variant != value)
                .nth(1)?
                .clone(),
        },
        (UiInputKind::TextHandle, UiInputValue::TextHandle { value }) => {
            UiInputValue::TextHandle { value: *value }
        }
        _ => return None,
    })
}

fn requested_input_value(
    kind: &UiInputKind,
    value: &UiSemanticPayloadValue,
) -> Option<UiInputValue> {
    match (kind, value) {
        (UiInputKind::Bool, UiSemanticPayloadValue::Bool { value }) => {
            Some(UiInputValue::Bool { value: *value })
        }
        (UiInputKind::I32, UiSemanticPayloadValue::I32 { value }) => {
            Some(UiInputValue::I32 { value: *value })
        }
        (UiInputKind::I32Range { minimum, maximum }, UiSemanticPayloadValue::I32 { value })
            if (*minimum..=*maximum).contains(value) =>
        {
            Some(UiInputValue::I32 { value: *value })
        }
        (UiInputKind::U32, UiSemanticPayloadValue::U32 { value }) => {
            Some(UiInputValue::U32 { value: *value })
        }
        (UiInputKind::U32Range { minimum, maximum }, UiSemanticPayloadValue::U32 { value })
            if (*minimum..=*maximum).contains(value) =>
        {
            Some(UiInputValue::U32 { value: *value })
        }
        (UiInputKind::F32, UiSemanticPayloadValue::F32 { value }) => {
            Some(UiInputValue::F32 { value: *value })
        }
        (UiInputKind::F32Range { minimum, maximum }, UiSemanticPayloadValue::F32 { value })
            if value.is_finite() && value >= minimum && value <= maximum =>
        {
            Some(UiInputValue::F32 { value: *value })
        }
        (UiInputKind::Enum { variants }, UiSemanticPayloadValue::Enum { value })
            if variants.contains(value) =>
        {
            Some(UiInputValue::Enum {
                value: value.clone(),
            })
        }
        (UiInputKind::TextHandle, UiSemanticPayloadValue::TextHandle { value }) => {
            Some(UiInputValue::TextHandle { value: *value })
        }
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
        UiInputValue::TextHandle { value } => format!("handle:{}:{}", value.id, value.generation),
        _ => "unavailable".into(),
    }
}

pub struct DemoDragDropDomain {
    revision: Option<Revision>,
    virtual_list_revision: Revision,
    virtual_list_rows: std::collections::BTreeMap<String, VirtualListRowState>,
}

#[derive(Clone, Debug, Default)]
struct VirtualListRowState {
    name: Option<String>,
    status: Option<String>,
    owner: Option<bool>,
    notes: Option<String>,
}

fn drag_drop_adapter_config() -> Result<UiHostAdapterConfig, &'static str> {
    let document = parse_nui_flow(include_str!(
        "../../../tests/fixtures/ui/kanban-reparent-workbench.nui"
    ))
    .map_err(|_| "drag/drop demo Flow is invalid")?;
    let program = compile_nui_flow_program(
        &document,
        UiProgramRevision {
            program_id: "surface.editor.kanban.demo".into(),
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
                name: name.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            })
            .collect(),
        },
    )
    .map_err(|_| "drag/drop demo program did not compile")?;
    Ok(UiHostAdapterConfig {
        program,
        input_schema: document.input_schema,
    })
}

impl DemoDragDropDomain {
    pub fn new() -> Self {
        Self {
            revision: None,
            virtual_list_revision: Revision(1),
            virtual_list_rows: std::collections::BTreeMap::new(),
        }
    }

    pub fn serve(endpoint: SocketAddr) -> Result<(), TransportError> {
        let server = RpcServer::bind(endpoint)?;
        let mut domain = Self::new();
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            (domain.handle(request), !shutdown)
        })
    }

    pub fn handle(&mut self, mut request: RpcRequest) -> RpcResponse {
        if request.method == "service.shutdown" {
            return accepted(request, self.revision, json!({"state": "accepted"}));
        }
        if request.method == "ui.host.adapter.get" {
            return match drag_drop_adapter_config() {
                Ok(config) => accepted(request, self.revision, json!(config)),
                Err(message) => {
                    rejected(request, self.revision, "ui_host_invalid_program", message)
                }
            };
        }
        if request.method == "ui.host.inbound" {
            let inbound: UiHostInbound = match serde_json::from_value(request.params.clone()) {
                Ok(inbound) => inbound,
                Err(_) => {
                    return rejected(
                        request,
                        self.revision,
                        "invalid_request",
                        "UI host inbound payload is invalid",
                    );
                }
            };
            let UiHostInbound::DragDrop {
                event,
                active_fragment,
            } = inbound
            else {
                return rejected(
                    request,
                    self.revision,
                    "unsupported_host_inbound",
                    "drag/drop demo accepts only drag/drop host input",
                );
            };
            let fragment = active_fragment.into_fragment();
            let active_revision = fragment.revision;
            let publication_request_id = event.request_id.clone();
            let publication_idempotency_key = event.idempotency_key.clone();
            let semantic_event = UiSemanticEvent {
                event: UiSemanticEventType::DragDrop,
                event_id: event.event_id,
                renderer_epoch: event.interaction.renderer_epoch,
                composition_revision: fragment.revision,
                fragment: neon_ui_schema::UiFragmentRevision {
                    id: fragment.fragment_id.clone(),
                    revision: fragment.revision,
                },
                intent: neon_ui_schema::UiIntent::Invoke {
                    action: event.intent,
                    params: json!({}),
                },
                pointer: Some(neon_ui_schema::UiPointerMetadata {
                    id: 0,
                    sequence: event.interaction.sequence,
                }),
                focus: None,
                data_grid_cell: None,
                text: None,
                control_value: None,
                drag_drop: Some(event.payload),
            };
            request.method = "ui.drag_drop.apply".into();
            request.expected_revision = Some(fragment.revision);
            request.params = json!({"event": semantic_event, "fragment": fragment});
            let mut response = self.handle(request);
            if response.status != RpcStatus::Accepted {
                return response;
            }
            let Some(replacement_fragment) = response
                .result
                .as_ref()
                .and_then(|value| value.get("fragment"))
                .cloned()
                .and_then(|value| serde_json::from_value::<UiFragment>(value).ok())
            else {
                response.status = RpcStatus::Rejected;
                response.result = None;
                response.error = Some(RpcError {
                    code: "ui_host_invalid_fragment".into(),
                    message: "accepted drag/drop did not produce a valid fragment".into(),
                    current_revision: self.revision,
                    object_id: None,
                });
                return response;
            };
            let config = drag_drop_adapter_config().expect("validated demo adapter config");
            response.result = Some(json!(UiHostPublication {
                scalar_frame: UiInputFrame {
                    program_revision: config.program.revision.clone(),
                    expected_input_revision: Revision(0),
                    request_id: publication_request_id,
                    idempotency_key: publication_idempotency_key,
                    changes: Vec::new(),
                },
                grid_inputs: Vec::new(),
                presentation_update: Some(UiHostPresentationUpdate {
                    expected_fragment_revision: active_revision,
                    replacement_fragment,
                    replacement_program: config.program,
                    replacement_input_schema: config.input_schema,
                }),
            }));
            return response;
        }
        if request.method == "ui.data_grid.window.request" {
            return self.handle_data_grid_window_request(request);
        }
        if matches!(
            request.method.as_str(),
            "virtual_list.name.commit"
                | "virtual_list.status.set"
                | "virtual_list.owner.toggle"
                | "virtual_list.notes.commit"
        ) {
            return self.handle_virtual_list_cell_event(request);
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
        let representation = instantiate_ui_template(
            &template,
            (drop.placement == UiDropPlacement::Into).then_some(target),
            &format!("{}-{}-r{}", template_key, drop.source_key, next_revision.0),
            Some(TextRef::Literal { value: label }),
        );
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
        let window_request: UiDataGridWindowRequest =
            match serde_json::from_value(request.params.clone()) {
                Ok(window_request) => window_request,
                Err(_) => {
                    return rejected(
                        request,
                        self.revision,
                        "invalid_request",
                        "a typed DataGrid window request is required",
                    );
                }
            };
        if window_request.source_key != "asset_window" || window_request.max_window_rows == 0 {
            return rejected(
                request,
                self.revision,
                "data_grid_not_found",
                "the requested DataGrid is not available",
            );
        }
        if window_request.expected_list_revision != self.virtual_list_revision {
            return rejected(
                request,
                Some(self.virtual_list_revision),
                "revision_conflict",
                "the requested DataGrid list revision is stale",
            );
        }
        let frame = self.virtual_list_window_frame(
            window_request.requested_first_row,
            window_request.max_window_rows,
            virtual_list_program_revision(),
            &["id", "name", "status", "owner", "notes"],
        );
        accepted(request, Some(frame.list_revision), json!(frame))
    }

    fn virtual_list_window_frame(
        &self,
        requested_first_row: u64,
        max_window_rows: u32,
        expected_program_revision: UiProgramRevision,
        declared_columns: &[&str],
    ) -> UiDataGridFrame {
        const TOTAL_ROWS: u64 = 10_000;
        const FIXTURE_MAX_WINDOW_ROWS: u32 = 256;
        let row_count = u64::from(max_window_rows.min(FIXTURE_MAX_WINDOW_ROWS)).min(TOTAL_ROWS);
        let first_row = requested_first_row.min(TOTAL_ROWS - row_count);
        UiDataGridFrame {
            list_revision: self.virtual_list_revision,
            total_rows: TOTAL_ROWS,
            first_row,
            window_rows: (first_row..first_row + row_count)
                .map(|row_index| {
                    let mut row = self.virtual_list_row(row_index);
                    row.cells
                        .retain(|key, _| declared_columns.contains(&key.as_str()));
                    row
                })
                .collect(),
            expected_program_revision,
        }
    }

    fn handle_virtual_list_cell_event(&mut self, request: RpcRequest) -> RpcResponse {
        let target = match request
            .params
            .get("data_grid_cell")
            .cloned()
            .and_then(|value| {
                serde_json::from_value::<neon_ui_schema::UiDataGridCellTarget>(value).ok()
            }) {
            Some(target) if target.source_key == "asset_window" => target,
            _ => {
                return rejected(
                    request,
                    Some(self.virtual_list_revision),
                    "invalid_data_grid_cell",
                    "a virtual-list cell target is required",
                );
            }
        };
        let frame = match request
            .params
            .get("data_grid_frame")
            .cloned()
            .and_then(|value| serde_json::from_value::<UiDataGridFrame>(value).ok())
        {
            Some(frame)
                if frame.list_revision == self.virtual_list_revision
                    && frame
                        .window_rows
                        .iter()
                        .any(|row| row.stable_row_key == target.stable_row_key) =>
            {
                frame
            }
            _ => {
                return rejected(
                    request,
                    Some(self.virtual_list_revision),
                    "revision_conflict",
                    "the virtual-list window is stale",
                );
            }
        };
        if virtual_list_row_index(&target.stable_row_key).is_none() {
            return rejected(
                request,
                Some(self.virtual_list_revision),
                "invalid_data_grid_cell",
                "the virtual-list row key is invalid",
            );
        }
        let row = self
            .virtual_list_rows
            .entry(target.stable_row_key.clone())
            .or_default();
        let valid = match request.method.as_str() {
            "virtual_list.name.commit" if target.column_key == "name" => request
                .params
                .get("text")
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<neon_ui_schema::UiTextInputCommit>(value).ok()
                })
                .filter(|text| !text.value.trim().is_empty())
                .map(|text| row.name = Some(text.value))
                .is_some(),
            "virtual_list.notes.commit" if target.column_key == "notes" => request
                .params
                .get("text")
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<neon_ui_schema::UiTextInputCommit>(value).ok()
                })
                .filter(|text| !text.value.trim().is_empty())
                .map(|text| row.notes = Some(text.value))
                .is_some(),
            "virtual_list.status.set" if target.column_key == "status" => {
                match request
                    .params
                    .get("control_value")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<UiSemanticPayloadValue>(value).ok())
                {
                    Some(UiSemanticPayloadValue::Enum { value })
                        if matches!(value.as_str(), "draft" | "ready" | "archived") =>
                    {
                        row.status = Some(value);
                        true
                    }
                    _ => false,
                }
            }
            "virtual_list.owner.toggle" if target.column_key == "owner" => {
                match request
                    .params
                    .get("control_value")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<UiSemanticPayloadValue>(value).ok())
                {
                    Some(UiSemanticPayloadValue::Bool { value }) => {
                        row.owner = Some(value);
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if !valid {
            return rejected(
                request,
                Some(self.virtual_list_revision),
                "invalid_data_grid_cell",
                "the virtual-list target or value is invalid",
            );
        }
        self.virtual_list_revision = Revision(self.virtual_list_revision.0 + 1);
        let row_count = frame.window_rows.len() as u64;
        let first_row = frame.first_row.min(10_000);
        let declared_columns = frame
            .window_rows
            .first()
            .map(|row| {
                row.cells
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        let window_rows = (first_row..first_row + row_count)
            .map(|index| {
                let mut row = self.virtual_list_row(index);
                row.cells.retain(|key, _| declared_columns.contains(key));
                row
            })
            .collect();
        let replacement = UiDataGridFrame {
            list_revision: self.virtual_list_revision,
            total_rows: 10_000,
            first_row,
            window_rows,
            expected_program_revision: frame.expected_program_revision,
        };
        accepted(request, Some(replacement.list_revision), json!(replacement))
    }

    fn virtual_list_row(&self, row_index: u64) -> UiDataGridWindowRow {
        virtual_list_row(
            row_index,
            self.virtual_list_revision,
            self.virtual_list_rows
                .get(&format!("virtual-row-{row_index}")),
        )
    }
}

fn virtual_list_row_index(stable_row_key: &str) -> Option<u64> {
    stable_row_key
        .strip_prefix("virtual-row-")?
        .parse::<u64>()
        .ok()
        .filter(|index| *index < 10_000)
}

fn virtual_list_row(
    row_index: u64,
    list_revision: Revision,
    state: Option<&VirtualListRowState>,
) -> UiDataGridWindowRow {
    let handle = |id| neon_ui_schema::UiTextHandle {
        id,
        generation: list_revision.0 as u32,
    };
    let base = 10_000 + row_index * 5;
    let state = state.cloned().unwrap_or_default();
    let name_handle = state.name.as_ref().map_or_else(
        || handle(base + 1),
        |name| neon_ui_schema::UiTextHandle {
            id: 1_000_000 + row_index * 1_000 + virtual_list_text_hash(name) % 1_000,
            generation: list_revision.0 as u32,
        },
    );
    let notes_handle = state.notes.as_ref().map_or_else(
        || handle(base + 4),
        |notes| neon_ui_schema::UiTextHandle {
            id: 2_000_000 + row_index * 1_000 + virtual_list_text_hash(notes) % 1_000,
            generation: list_revision.0 as u32,
        },
    );
    UiDataGridWindowRow {
        stable_row_key: format!("virtual-row-{row_index}"),
        cells: std::collections::BTreeMap::from([
            (
                "id".into(),
                UiDataGridCell {
                    value: UiInputValue::I32 {
                        value: row_index as i32,
                    },
                    display: handle(base),
                    presentation_override: None,
                },
            ),
            (
                "name".into(),
                UiDataGridCell {
                    value: UiInputValue::TextHandle { value: name_handle },
                    display: name_handle,
                    presentation_override: None,
                },
            ),
            (
                "status".into(),
                UiDataGridCell {
                    value: UiInputValue::Enum {
                        value: state.status.unwrap_or_else(|| "ready".into()),
                    },
                    display: handle(base + 2),
                    presentation_override: None,
                },
            ),
            (
                "owner".into(),
                UiDataGridCell {
                    value: UiInputValue::Bool {
                        value: state.owner.unwrap_or(row_index % 2 == 0),
                    },
                    display: handle(base + 3),
                    presentation_override: None,
                },
            ),
            (
                "notes".into(),
                UiDataGridCell {
                    value: UiInputValue::TextHandle {
                        value: notes_handle,
                    },
                    display: notes_handle,
                    presentation_override: None,
                },
            ),
        ]),
    }
}

fn virtual_list_text_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
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
        ]
        .into_iter()
        .map(|name| UiProgramCapability {
            name: name.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        })
        .collect(),
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
    use crate::{
        UiProgramSemanticEventRouter, lower_nui_flow_effects, parse_nui_flow,
    };
    use neon_ipc::RpcClient;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId, ServiceName};
    use neon_ui_schema::UiFragmentId;

    fn gallery_asset() -> AssetRef {
        AssetRef {
            project_id: "test-project".into(),
            asset_id: 1,
            revision: Revision(1),
            kind: "image".into(),
        }
    }

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
        let mut domain = DemoDragDropDomain::new();
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
        let target = find_node(&updated.root, "in-progress-panel").unwrap();
        let prototype = find_node(&updated.root, "progress-template").unwrap();
        let representation = find_node(
            &updated.root,
            "progress-template-backlog-card-01-r2-progress-template",
        )
        .unwrap();
        assert!(!prototype.visible);
        assert!(!prototype.enabled);
        assert!(representation.visible);
        assert!(representation.enabled);
        assert_eq!(
            target
                .children
                .iter()
                .filter(|child| child.node_id == representation.node_id)
                .count(),
            1
        );
        assert_eq!(
            first_literal(representation).as_deref(),
            Some("Card 01 / Terraform cliff")
        );
        updated.validate().unwrap();

        let mut duplicate = request(
            fragment(),
            "progress-drop",
            "backlog-card-01",
            "in-progress-panel",
            UiDropPlacement::Into,
            "progress-template",
        );
        let mut duplicate_event: UiSemanticEvent =
            serde_json::from_value(duplicate.params["event"].clone()).unwrap();
        duplicate_event.composition_revision = updated.revision;
        duplicate_event.fragment.revision = updated.revision;
        duplicate.params = json!({"event": duplicate_event, "fragment": updated.clone()});
        duplicate.expected_revision = Some(updated.revision);
        let duplicate = domain.handle(duplicate);
        assert_eq!(duplicate.status, RpcStatus::Rejected);
        assert_eq!(
            duplicate.error.unwrap().code,
            "drag_drop_not_declared",
            "the consumed drag binding must prevent a second materialization"
        );
        assert_eq!(
            find_node(&updated.root, "in-progress-panel")
                .unwrap()
                .children
                .iter()
                .filter(|child| child.visible && child.node_id == representation.node_id)
                .count(),
            1
        );
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
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("virtual-list-demo".into()),
                revision: Revision(3),
            },
            source_key: "asset_window".into(),
            expected_list_revision: Revision(1),
            requested_first_row: 9_996,
            max_window_rows: 56,
            sequence: 4,
        };
        let response = DemoDragDropDomain::new().handle(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("virtual-list-window".into()),
            client: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "test".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName("demo-domain".into()),
            method: "ui.data_grid.window.request".into(),
            params: json!(request),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("virtual-list-window".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        let frame: UiDataGridFrame = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(frame.total_rows, 10_000);
        assert_eq!(frame.first_row, 9_944);
        assert_eq!(frame.window_rows.len(), 56);
        assert_eq!(frame.window_rows[0].stable_row_key, "virtual-row-9944");
        assert_eq!(
            frame.window_rows.last().unwrap().stable_row_key,
            "virtual-row-9999"
        );
        let larger = DemoDragDropDomain::new().virtual_list_window_frame(
            9_996,
            99,
            virtual_list_program_revision(),
            &["id", "name", "status", "owner", "notes"],
        );
        assert_eq!(larger.first_row, 9_901);
        assert_eq!(larger.window_rows.len(), 99);
    }

    #[test]
    fn virtual_list_cell_handlers_persist_values_and_replace_the_current_window() {
        let mut domain = DemoDragDropDomain::new();
        let mut frame = UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 10_000,
            first_row: 0,
            window_rows: vec![domain.virtual_list_row(0)],
            expected_program_revision: virtual_list_program_revision(),
        };
        let request = |method: &str, column_key: &str, frame: &UiDataGridFrame, params: Value| {
            RpcRequest {
                protocol: "neon3.rpc".into(),
                version: ProtocolVersion { major: 1, minor: 0 },
                request_id: RequestId(format!("virtual-list-{method}")),
                client: ClientIdentity {
                    kind: ClientKind::UiRuntime,
                    instance_id: "test".into(),
                    pid: 1,
                    origin: "test".into(),
                },
                target: ServiceName("demo-domain".into()),
                method: method.into(),
                params: json!({
                    "data_grid_cell": { "source_key": "asset_window", "stable_row_key": "virtual-row-0", "column_key": column_key },
                    "data_grid_frame": frame,
                    "control_value": params.get("control_value"),
                    "text": params.get("text"),
                }),
                expected_revision: Some(frame.list_revision),
                idempotency_key: Some(format!("virtual-list-{method}")),
            }
        };
        let response = domain.handle(request(
            "virtual_list.name.commit",
            "name",
            &frame,
            json!({"text": {"value": "first"}}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        frame = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(frame.list_revision, Revision(2));
        assert_ne!(frame.window_rows[0].cells["name"].display.id, 10_001);

        let response = domain.handle(request(
            "virtual_list.status.set",
            "status",
            &frame,
            json!({"control_value": {"kind": "enum", "value": "archived"}}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        frame = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(frame.list_revision, Revision(3));
        assert_eq!(
            frame.window_rows[0].cells["status"].value,
            UiInputValue::Enum {
                value: "archived".into()
            }
        );

        let response = domain.handle(request(
            "virtual_list.owner.toggle",
            "owner",
            &frame,
            json!({"control_value": {"kind": "bool", "value": false}}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        frame = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(frame.list_revision, Revision(4));
        assert_eq!(
            frame.window_rows[0].cells["owner"].value,
            UiInputValue::Bool { value: false }
        );
    }

    #[test]
    fn component_gallery_accepts_a_grid_cell_as_its_first_inbound_event() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            DemoInputDomain::serve_component_gallery_server(server, gallery_asset()).unwrap();
        });
        let (_, program) = component_gallery_program(gallery_asset()).unwrap();
        let event = UiSemanticEvent {
            event: UiSemanticEventType::SelectionChanged,
            event_id: "gallery-first-grid-cell".into(),
            renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("component-gallery".into()),
                revision: Revision(1),
            },
            intent: neon_ui_schema::UiIntent::Invoke {
                action: "virtual_list.owner.toggle".into(),
                params: json!({}),
            },
            pointer: None,
            focus: None,
            data_grid_cell: Some(neon_ui_schema::UiDataGridCellTarget {
                source_key: "asset_window".into(),
                stable_row_key: "virtual-row-0".into(),
                column_key: "owner".into(),
            }),
            text: None,
            control_value: Some(UiSemanticPayloadValue::Bool { value: false }),
            drag_drop: None,
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId("gallery-first-grid-cell".into()),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: "test-window".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "ui.host.inbound".into(),
            params: json!(UiHostInbound::DataGridCell { event }),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("gallery-first-grid-cell".into()),
        };
        let response = RpcClient::connect(endpoint)
            .and_then(|mut client| client.call(&request))
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted, "{:?}", response.error);
        assert_ne!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("ui_host_grid_unavailable")
        );
        let publication: UiHostPublication =
            serde_json::from_value(response.result.unwrap()).unwrap();
        let frame = &publication.grid_inputs[0].frame;
        assert_eq!(frame.expected_program_revision, program.revision);
        assert_eq!(frame.window_rows.len(), 80);
        assert_eq!(frame.list_revision, Revision(2));
        assert_eq!(
            frame.window_rows[0].cells["owner"].value,
            UiInputValue::Bool { value: false }
        );

        let shutdown = RpcRequest {
            request_id: RequestId("gallery-shutdown".into()),
            method: "service.shutdown".into(),
            idempotency_key: Some("gallery-shutdown".into()),
            ..request
        };
        RpcClient::connect(endpoint)
            .and_then(|mut client| client.call(&shutdown))
            .unwrap();
        server_thread.join().unwrap();
    }

    #[test]
    fn component_gallery_headless_scenario_accepts_events_and_publishes_visible_status() {
        let (document, program) = component_gallery_program(gallery_asset()).unwrap();
        let revision = program.revision.clone();
        let mut domain =
            DemoInputDomain::new(program.clone(), document.input_schema.clone()).unwrap();
        let mut router =
            UiProgramSemanticEventRouter::new(program.clone(), domain.snapshot().inputs, 7);
        for (index, node_key) in [
            "action-button",
            "feature-toggle",
            "mode-radio",
            "exposure-slider",
            "count-drag",
            "mode-combo",
            "mode-dropdown",
            "mode-tabs",
            "item-selectable",
            "item-list",
            "gallery-scroll",
            "gallery-text",
        ]
        .iter()
        .enumerate()
        {
            let snapshot = domain.snapshot();
            let declaration = program
                .event_records
                .iter()
                .find(|event| event.node_key == *node_key)
                .unwrap();
            let payload = declaration
                .bound_input_keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        match &snapshot.inputs.values[key].value {
                            UiInputValue::Bool { value } => {
                                neon_ui_schema::UiSemanticPayloadValue::Bool { value: *value }
                            }
                            UiInputValue::I32 { value } => {
                                neon_ui_schema::UiSemanticPayloadValue::I32 { value: *value }
                            }
                            UiInputValue::F32 { value } => {
                                neon_ui_schema::UiSemanticPayloadValue::F32 { value: *value }
                            }
                            UiInputValue::Enum { value } => {
                                neon_ui_schema::UiSemanticPayloadValue::Enum {
                                    value: value.clone(),
                                }
                            }
                            UiInputValue::TextHandle { value } => {
                                neon_ui_schema::UiSemanticPayloadValue::TextHandle { value: *value }
                            }
                            _ => unreachable!(),
                        },
                    )
                })
                .collect();
            let event = UiProgramSemanticEvent {
                event_id: format!("gallery-scenario-{index}"),
                kind: if matches!(
                    *node_key,
                    "feature-toggle"
                        | "mode-radio"
                        | "mode-combo"
                        | "mode-tabs"
                        | "item-selectable"
                ) {
                    neon_ui_schema::UiProgramSemanticEventKind::SelectionChanged
                } else if *node_key == "action-button" {
                    neon_ui_schema::UiProgramSemanticEventKind::Activate
                } else if *node_key == "gallery-text" {
                    neon_ui_schema::UiProgramSemanticEventKind::TextEditCommit
                } else {
                    neon_ui_schema::UiProgramSemanticEventKind::ValueCommit
                },
                intent: declaration.intent.clone(),
                source_node_key: (*node_key).into(),
                payload,
                program_revision: revision.clone(),
                input_revision: snapshot.inputs.input_revision,
                request_id: format!("gallery-request-{index}"),
                idempotency_key: format!("gallery-key-{index}"),
                requested_value: None,
                interaction: neon_ui_schema::UiSemanticInteractionMetadata {
                    interaction_id: format!("gallery-interaction-{index}"),
                    sequence: index as u64 + 1,
                    renderer_epoch: 7,
                },
            };
            assert_eq!(
                router.validate(&event).status,
                neon_ui_schema::UiProgramSemanticEventStatus::Accepted
            );
            let updated = domain.apply(&event).unwrap();
            assert_eq!(updated.inputs.input_revision, Revision(index as u64 + 1));
            assert!(updated.visible_status.values().any(|status| status != ""));
            router.replace_resolved_inputs(updated.inputs);
        }
        let snapshot = domain.snapshot();
        assert_eq!(
            snapshot.inputs.values["feature_enabled"].value,
            UiInputValue::Bool { value: true }
        );
        assert_eq!(
            snapshot.inputs.values["combo_choice"].value,
            UiInputValue::Enum {
                value: "gamma".into()
            }
        );
        assert_eq!(
            snapshot.inputs.values["dropdown_choice"].value,
            UiInputValue::Enum {
                value: "gamma".into()
            }
        );
        assert_eq!(
            snapshot.inputs.values["tabs_choice"].value,
            UiInputValue::Enum {
                value: "gamma".into()
            }
        );
        assert_eq!(
            snapshot.inputs.values["list_choice"].value,
            UiInputValue::Enum {
                value: "gamma".into()
            }
        );
        assert_eq!(
            snapshot.visible_status["status-dropdown_choice"],
            "dropdown_choice: gamma"
        );
        let mut fragment = UiFragment {
            fragment_id: neon_ui_schema::UiFragmentId("gallery-scenario".into()),
            revision: Revision(2),
            root: document.ir.root.clone(),
            effects: Vec::new(),
        };
        apply_visible_status_to_fragment(&mut fragment, &snapshot);
        assert_eq!(
            first_literal(find_node(&fragment.root, "status-list_choice").unwrap()).as_deref(),
            Some("Selected mode: gamma")
        );
        assert!(fragment.effects.iter().any(|effect| matches!(
            effect,
            UiEffect::ControlPresentation {
                node_id,
                state: UiControlPresentation::Toggle { selected: true }
            } if node_id.0 == "feature-toggle"
        )));
    }

    #[test]
    fn variable_change_publishes_nui_variable_changed_over_loopback() {
        use neon_ipc::{DEFAULT_MAX_FRAME_SIZE, read_json_frame, write_json_frame};
        use neon_protocol::{EventAckStatus, EventFrame, EventResponse};

        // Stand in for neon-eventd: accepts one publish frame, returns an ack.
        let server = neon_ipc::RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut stream = server.accept().unwrap();
            let frame: EventFrame =
                read_json_frame(&mut stream, DEFAULT_MAX_FRAME_SIZE).unwrap();
            let EventFrame::Publish(publish) = frame else {
                panic!("expected a publish frame");
            };
            assert_eq!(publish.name, "flow.component-gallery.feature_enabled");
            assert_eq!(publish.payload["variable_key"], "feature_enabled");
            assert_eq!(publish.payload["kind"], "bool");
            assert_eq!(publish.payload["module"], "gallery");
            assert_eq!(publish.payload["new_value"], json!({"value": false}));
            assert_eq!(publish.payload["old_value"], json!({"value": true}));
            write_json_frame(
                &mut stream,
                &EventResponse::Ack(neon_protocol::EventAck {
                    protocol: publish.protocol,
                    version: publish.version,
                    request_id: publish.request_id,
                    status: EventAckStatus::Accepted,
                    event_id: Some(neon_protocol::EventId("evt-1-1".into())),
                    epoch: Some(1),
                    sequence: Some(1),
                    current_sequence: Some(1),
                    error: None,
                }),
                DEFAULT_MAX_FRAME_SIZE,
            )
            .unwrap();
        });

        let (document, program) = component_gallery_program(gallery_asset()).unwrap();
        let revision = program.revision.clone();
        let mut domain = DemoInputDomain::new(program.clone(), document.input_schema.clone())
            .unwrap()
            .with_schema_publisher(
                Some(endpoint),
                ClientIdentity {
                    kind: ClientKind::UiRuntime,
                    instance_id: "gallery-test".into(),
                    pid: 1,
                    origin: "test".into(),
                },
                "gallery",
                "surface.editor.gallery",
            );
        let snapshot = domain.snapshot();
        let declaration = program
            .event_records
            .iter()
            .find(|event| event.node_key == "feature-toggle")
            .unwrap();
        let event = UiProgramSemanticEvent {
            event_id: "gallery-publish-event".into(),
            kind: neon_ui_schema::UiProgramSemanticEventKind::SelectionChanged,
            intent: declaration.intent.clone(),
            source_node_key: "feature-toggle".into(),
            payload: declaration
                .bound_input_keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        neon_ui_schema::UiSemanticPayloadValue::Bool { value: false },
                    )
                })
                .collect(),
            program_revision: revision.clone(),
            input_revision: snapshot.inputs.input_revision,
            request_id: "gallery-publish-request".into(),
            idempotency_key: "gallery-publish-key".into(),
            requested_value: Some(neon_ui_schema::UiSemanticPayloadValue::Bool {
                value: false,
            }),
            interaction: neon_ui_schema::UiSemanticInteractionMetadata {
                interaction_id: "gallery-publish-interaction".into(),
                sequence: 1,
                renderer_epoch: 7,
            },
        };
        let updated = domain.apply(&event).unwrap();
        assert_eq!(
            updated.inputs.values["feature_enabled"].value,
            UiInputValue::Bool { value: false }
        );
        server_thread.join().unwrap();
    }
}
