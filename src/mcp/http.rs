use crate::api::server::AppState;
use crate::engine::Engine;
use crate::mcp::tools;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::Sse;
use futures::Stream;
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc;

type SessionTx = mpsc::Sender<serde_json::Value>;

/// Global session registry: session_id → channel sender for message endpoint
fn message_sessions() -> &'static Arc<Mutex<HashMap<String, SessionTx>>> {
    static SESSIONS: std::sync::OnceLock<Arc<Mutex<HashMap<String, SessionTx>>>> =
        std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Handle SSE connection from MCP client (GET /mcp).
/// Creates a session and streams JSON-RPC responses back to the client.
pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Two channels:
    // 1. `msg_tx` → `msg_rx`: receives JSON-RPC requests from POST /mcp/messages
    // 2. `sse_tx` → `sse_rx`: sends JSON-RPC responses to the SSE stream
    let (msg_tx, mut msg_rx) = mpsc::channel::<serde_json::Value>(100);
    let (sse_tx, sse_rx) = mpsc::channel::<serde_json::Value>(100);

    let session_id = uuid::Uuid::new_v4().to_string();

    // Register session so POST /mcp/messages can find it
    message_sessions()
        .lock()
        .unwrap()
        .insert(session_id.clone(), msg_tx.clone());

    let engine = Arc::clone(&state.engine);

    // Spawn processor: receives requests, calls engine, sends responses
    tokio::spawn(async move {
        // Send initial connection event
        let _ = sse_tx
            .send(json!({
                "jsonrpc": "2.0",
                "id": null,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "memex8", "version": "0.1.0" }
                }
            }))
            .await;

        while let Some(message) = msg_rx.recv().await {
            let id = message.get("id").cloned();
            let method = message
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let params = message.get("params").cloned().unwrap_or(json!({}));

            tracing::debug!("MCP message: method={} id={:?}", method, id);

            let result = match method {
                "initialize" => handle_initialize(),
                "initialized" => Ok(json!({})),
                "ping" => Ok(json!({})),
                "tools/list" => Ok(json!({ "tools": tools::list_tools() })),
                "tools/call" => handle_tool_call(&engine, &params).await,
                _ => Err(anyhow::anyhow!("Method not found: {}", method)),
            };

            let response = match result {
                Ok(data) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": data,
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": e.to_string() },
                }),
            };

            if sse_tx.send(response).await.is_err() {
                break; // SSE client disconnected
            }
        }

        // Clean up session
        message_sessions().lock().unwrap().remove(&session_id);
    });

    // Build SSE stream from the response channel
    let stream = async_stream::stream! {
        let mut rx = sse_rx;
        while let Some(msg) = rx.recv().await {
            if let Ok(event) = Event::default().json_data(&msg) {
                yield Ok(event);
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Handle POST from MCP client with a JSON-RPC message (POST /mcp/messages?session_id=xxx).
/// The request body must be a JSON-RPC 2.0 message.
pub async fn message_handler(
    State(_state): State<Arc<AppState>>,
    uri: http::Uri,
    body: axum::body::Body,
) -> StatusCode {
    // Collect body bytes
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let message: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    let session_id = uri
        .query()
        .and_then(|q| {
            q.split('&')
                .find(|p| p.starts_with("session_id="))
                .map(|p| &p["session_id=".len()..])
        })
        .unwrap_or("");

    if session_id.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    let sessions = message_sessions();
    let guard = sessions.lock().unwrap();

    if let Some(tx) = guard.get(session_id) {
        if tx.send(message).await.is_ok() {
            return StatusCode::ACCEPTED;
        }
    }

    StatusCode::BAD_REQUEST
}

/// Handle MCP tool calls by dispatching to the Engine.
async fn handle_tool_call(engine: &Engine, params: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "memex8_search" => {
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;
            let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());
            let min_score = arguments
                .get("min_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;

            let results = engine.search(query, realm, None, limit, 0, min_score).await?;
            Ok(format_results(results))
        }
        "memex8_store" => {
            let content = arguments
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;
            let tags = arguments.get("tags").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });
            let realm_hint = arguments.get("realm_hint").and_then(|v| v.as_str());

            let id = engine
                .store_memory(content, tags, realm_hint, Some("mcp"))
                .await?;
            Ok(json!({ "id": id, "status": "stored" }))
        }
        "memex8_recall" => {
            let limit = arguments.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());
            let results = engine.recall(limit, realm).await?;
            Ok(format_results(results))
        }
        "memex8_get" => {
            let id = arguments
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
            let memory = engine.get_memory(id).await?;
            Ok(json!({ "memory": memory }))
        }
        "memex8_ingest" => {
            let path = arguments
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;
            let chunk_by = arguments
                .get("chunk_by")
                .and_then(|v| v.as_str())
                .unwrap_or("section");
            let realm_hint = arguments.get("realm_hint").and_then(|v| v.as_str());

            engine.ingest_path(path, chunk_by, realm_hint).await?;
            Ok(json!({ "status": "ingested", "path": path }))
        }
        "memex8_realms_list" => {
            let realms = engine.list_realms().await?;
            Ok(json!({ "realms": realms }))
        }
        "memex8_realms_show" => {
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;
            let realm = engine.show_realm(name).await?;
            Ok(json!({ "realm": realm }))
        }
        "memex8_upvote" => {
            let id = arguments
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter"))?;
            engine.upvote(id).await?;
            Ok(json!({ "upvoted": id }))
        }
        "memex8_stats" => {
            let stats = engine.stats().await?;
            Ok(json!({ "stats": stats }))
        }
        "memex8_slumber_status" => {
            let status = engine.slumber_status().await;
            Ok(json!({ "status": status }))
        }
        "memex8_graph_search" => {
            let entity = arguments
                .get("entity")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'entity' parameter"))?;
            let relationship = arguments.get("relationship").and_then(|v| v.as_str());
            let depth = arguments.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

            let results = engine.graph_search(entity, relationship, depth).await?;
            Ok(json!({ "results": results }))
        }
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
}

fn format_results(results: Vec<crate::engine::MemoryResult>) -> serde_json::Value {
    json!({
        "results": results
            .iter()
            .map(|r| json!({
                "id": r.id,
                "content": r.content,
                "heading": r.heading,
                "realm": r.realm_name,
                "importance": r.importance,
                "score": r.score,
            }))
            .collect::<Vec<_>>(),
        "count": results.len(),
    })
}

fn handle_initialize() -> anyhow::Result<serde_json::Value> {
    Ok(json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "memex8",
            "version": "0.1.0"
        }
    }))
}
