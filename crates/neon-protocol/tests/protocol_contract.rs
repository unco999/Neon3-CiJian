use neon_protocol::{AssetRef, RpcRequest, RpcResponse};
use serde_json::json;

fn request_fixture() -> serde_json::Value {
    json!({
        "protocol": "neon3.rpc",
        "version": { "major": 1, "minor": 0 },
        "request_id": "request-001",
        "client": {
            "kind": "cli",
            "instance_id": "client-001",
            "pid": 1234,
            "origin": "contract-test"
        },
        "target": "wgpu-runtime",
        "method": "service.health",
        "params": {},
        "expected_revision": null,
        "idempotency_key": null
    })
}

#[test]
fn complete_request_fixture_round_trips() {
    let request: RpcRequest = serde_json::from_value(request_fixture()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), request_fixture());
}

#[test]
fn accepted_response_fixture_round_trips() {
    let fixture = json!({
        "request_id": "request-001",
        "status": "accepted",
        "revision": 7,
        "result": { "status": "healthy" },
        "snapshot": null,
        "error": null
    });
    let response: RpcResponse = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), fixture);
}

#[test]
fn revision_conflict_preserves_request_and_current_revision() {
    let fixture = json!({
        "request_id": "request-002",
        "status": "rejected",
        "revision": null,
        "result": null,
        "snapshot": null,
        "error": {
            "code": "revision_conflict",
            "message": "revision changed",
            "current_revision": 43,
            "object_id": "terrain:12"
        }
    });
    let response: RpcResponse = serde_json::from_value(fixture).unwrap();
    assert_eq!(response.request_id.0, "request-002");
    assert_eq!(response.error.unwrap().current_revision.unwrap().0, 43);
}

#[test]
fn missing_required_request_field_is_rejected() {
    let mut fixture = request_fixture();
    fixture.as_object_mut().unwrap().remove("request_id");
    assert!(serde_json::from_value::<RpcRequest>(fixture).is_err());
}

#[test]
fn unknown_request_fields_are_rejected_by_the_compatibility_policy() {
    let mut fixture = request_fixture();
    fixture
        .as_object_mut()
        .unwrap()
        .insert("future_field".to_owned(), json!(true));
    assert!(serde_json::from_value::<RpcRequest>(fixture).is_err());
}

#[test]
fn asset_ref_contains_only_stable_cross_service_identity() {
    let asset = AssetRef {
        project_id: "project-001".to_owned(),
        asset_id: 81,
        revision: neon_protocol::Revision(5),
        kind: "water_material".to_owned(),
    };
    let value = serde_json::to_value(asset).unwrap();
    assert!(value.get("path").is_none());
    assert!(value.get("local_path").is_none());
    assert!(value.get("file_path").is_none());
}
