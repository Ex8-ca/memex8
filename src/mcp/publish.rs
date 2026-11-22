//! MCP result publication boundary.
//!
//! Erasure safety is a property of *construction*, not delivery. For read-only
//! tools this module holds the maintenance read gate while the tool services the
//! request and while the JSON-RPC response is serialized, so an evidence erasure
//! that arrives after the fetch cannot commit before the response bytes exist.
//!
//! It deliberately does not claim more than that:
//! - bytes handed to stdout / an SSE stream / a socket can still reach a client
//!   after the erasure commits, and cannot be recalled;
//! - a transport that buffers, retries, or delays delivery is outside this
//!   guarantee;
//! - spawned tasks do not inherit the gate scope, so anything they read must
//!   take its own guard.
use std::future::Future;

use serde::Serialize;
use serde_json::Value;

use crate::storage::gate::MaintenanceGate;

/// Tools whose handlers only read memory state, and so may run under the
/// maintenance read gate. Writer tools must stay out of this list: the ones
/// that take the exclusive side of the same gate (certification, edit,
/// consolidation/undo, deletion) would deadlock, and the others take no
/// exclusive guard but publish no fetched content that needs ordering against
/// erasure.
pub fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "memex8_search"
            | "memex8_recall"
            | "memex8_get"
            | "memex8_timeline"
            | "memex8_realms_list"
            | "memex8_realms_show"
            | "memex8_stats"
            | "memex8_slumber_status"
            | "memex8_graph_search"
            | "memex8_graph_traverse"
            | "memex8_graph_stats"
            | "memex8_infer"
    )
}

#[derive(Serialize)]
struct RpcResponse<'a, T: Serialize> {
    jsonrpc: &'static str,
    id: Option<&'a Value>,
    result: T,
}

/// Serialize a full JSON-RPC success line for `tool`.
///
/// Read-only tools run `call` and the response serialization inside one gated
/// read scope. Writer tools run neither under the gate.
pub async fn tool_response_line<T, F>(
    gate: &MaintenanceGate,
    tool: &str,
    id: Option<&Value>,
    call: F,
) -> anyhow::Result<String>
where
    T: Serialize,
    F: Future<Output = anyhow::Result<T>>,
{
    let build = async move {
        let result = call.await?;
        Ok(serde_json::to_string(&RpcResponse {
            jsonrpc: "2.0",
            id,
            result,
        })?)
    };
    if is_read_only_tool(tool) {
        gate.read_gated(build).await
    } else {
        build.await
    }
}

/// Read the tool name from JSON-RPC `params`, without dispatching.
pub fn tool_name(params: &Value) -> anyhow::Result<&str> {
    params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))
}

/// Read the tool arguments from JSON-RPC `params`; an absent value means no arguments.
pub fn tool_arguments(params: &Value) -> Value {
    params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}
