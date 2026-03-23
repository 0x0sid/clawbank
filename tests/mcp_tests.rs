//! Integration tests for MCP skill manifest and JSON-RPC types.

use openclaw_aibank::mcp::skill::build_manifest;
use openclaw_aibank::types::{JsonRpcRequest, JsonRpcResponse};

#[test]
fn manifest_has_all_9_tools() {
    let manifest = build_manifest();
    assert_eq!(manifest.tools.len(), 9);

    let names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"agent_register"));
    assert!(names.contains(&"request_credit"));
    assert!(names.contains(&"propose_trade"));
    assert!(names.contains(&"repay_credit"));
    assert!(names.contains(&"get_portfolio"));
    assert!(names.contains(&"list_proposals"));
    assert!(names.contains(&"get_risk_score"));
    assert!(names.contains(&"get_credit_line"));
    assert!(names.contains(&"submit_x402_payment"));
}

#[test]
fn manifest_name_and_version() {
    let manifest = build_manifest();
    assert_eq!(manifest.name, "openclaw-aibank");
    assert_eq!(manifest.version, "0.1.0");
}

#[test]
fn jsonrpc_response_success() {
    let resp = JsonRpcResponse::success(serde_json::json!(1), serde_json::json!({"ok": true}));
    assert!(resp.error.is_none());
    assert!(resp.result.is_some());
    assert_eq!(resp.jsonrpc, "2.0");
}

#[test]
fn jsonrpc_response_error() {
    let resp = JsonRpcResponse::error(serde_json::json!(1), -32600, "Invalid request".to_string());
    assert!(resp.result.is_none());
    assert!(resp.error.is_some());
    assert_eq!(resp.error.unwrap().code, -32600);
}

#[test]
fn jsonrpc_request_parse() {
    let json = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.method, "tools/list");
    assert_eq!(req.jsonrpc, "2.0");
}

#[test]
fn jsonrpc_request_parse_no_params() {
    let json = r#"{"jsonrpc":"2.0","id":"abc","method":"initialize"}"#;
    let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.method, "initialize");
}

#[test]
fn each_tool_has_parameters() {
    let manifest = build_manifest();
    for tool in &manifest.tools {
        assert!(
            tool.parameters.is_object(),
            "Tool {} should have object parameters",
            tool.name
        );
    }
}
