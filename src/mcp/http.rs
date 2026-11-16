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
const MAX_SESSIONS: usize = 64;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

struct Session {
    tx: SessionTx,
    // Never allow a session registered by another server/engine in this process.
    owner: std::sync::Weak<AppState>,
}

struct SessionGuard(String);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        message_sessions().lock().unwrap().remove(&self.0);
    }
}

/// Global session registry: session_id → channel sender for message endpoint
fn message_sessions() -> &'static Arc<Mutex<HashMap<String, Session>>> {
    static SESSIONS: std::sync::OnceLock<Arc<Mutex<HashMap<String, Session>>>> =
        std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Handle SSE connection from MCP client (GET /mcp).
/// Creates a session and streams JSON-RPC responses back to the client.
pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    // Two channels:
    // 1. `msg_tx` → `msg_rx`: receives JSON-RPC requests from POST /mcp/messages
    // 2. `sse_tx` → `sse_rx`: sends serialized JSON-RPC responses to the SSE stream
    let (msg_tx, mut msg_rx) = mpsc::channel::<serde_json::Value>(100);
    let (sse_tx, sse_rx) = mpsc::channel::<String>(100);

    let session_id = uuid::Uuid::new_v4().to_string();

    // Register session so POST /mcp/messages can find it
    {
        let mut sessions = message_sessions().lock().unwrap();
        if sessions.len() >= MAX_SESSIONS {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        sessions.insert(
            session_id.clone(),
            Session {
                tx: msg_tx,
                owner: Arc::downgrade(&state),
            },
        );
    }
    let guard = SessionGuard(session_id.clone());

    let engine = Arc::clone(&state.engine);

    // Spawn processor: receives requests, calls engine, sends responses
    tokio::spawn(async move {
        let _guard = guard;
        let process = async {
            while let Some(message) = msg_rx.recv().await {
                let id = message.get("id").cloned();
                let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let params = message.get("params").cloned().unwrap_or(json!({}));

                tracing::debug!("MCP message: method={} id={:?}", method, id);

                // Read-only tool calls serialize their response while the maintenance
                // read gate is held, so an erasure cannot commit between fetch and
                // response construction. SSE delivery after that is not covered.
                let response: String = match method {
                    "initialize" => success_line(id.as_ref(), &handle_initialize()),
                    "initialized" | "notifications/initialized" => {
                        if id.is_none() {
                            continue;
                        }
                        success_line(id.as_ref(), &Ok(json!({})))
                    }
                    "ping" => success_line(id.as_ref(), &Ok(json!({}))),
                    "tools/list" => {
                        success_line(id.as_ref(), &Ok(json!({ "tools": tools::list_tools() })))
                    }
                    "tools/call" => match super::publish::tool_name(&params) {
                        Ok(name) => match super::publish::tool_response_line(
                            engine.store().gate(),
                            name,
                            id.as_ref(),
                            handle_tool_call(&engine, &params),
                        )
                        .await
                        {
                            Ok(line) => line,
                            Err(e) => error_line(id.as_ref(), &e),
                        },
                        Err(e) => error_line(id.as_ref(), &e),
                    },
                    _ => json!({"jsonrpc":"2.0", "id":id,
                    "error":{"code":-32601,"message":"Method not found"}})
                    .to_string(),
                };

                // JSON-RPC notifications never receive responses.
                if id.is_none() {
                    continue;
                }
                if sse_tx.send(response).await.is_err() {
                    break; // SSE client disconnected
                }
            }
        };
        tokio::select! {
            biased;
            _ = sse_tx.closed() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {},
            _ = process => {},
        }
        // RAII removes the registry entry on EOF, timeout, or panic.
    });

    // Build SSE stream from the response channel
    let stream = async_stream::stream! {
        let mut rx = sse_rx;
        yield Ok(Event::default().event("endpoint")
            .data(format!("/mcp/messages?session_id={session_id}")));
        while let Some(msg) = rx.recv().await {
            // `msg` is already a serialized JSON-RPC message.
            yield Ok(Event::default().event("message").data(msg));
        }
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Handle POST from MCP client with a JSON-RPC message (POST /mcp/messages?session_id=xxx).
/// The request body must be a JSON-RPC 2.0 message.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionQuery {
    session_id: uuid::Uuid,
}

pub async fn message_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<SessionQuery>,
    body: axum::body::Body,
) -> StatusCode {
    let tx = {
        let sessions = message_sessions().lock().unwrap();
        match sessions.get(&query.session_id.to_string()) {
            Some(session)
                if session
                    .owner
                    .upgrade()
                    .is_some_and(|owner| Arc::ptr_eq(&owner, &state)) =>
            {
                session.tx.clone()
            }
            _ => return StatusCode::NOT_FOUND,
        }
    };
    let bytes = match axum::body::to_bytes(body, MAX_MESSAGE_BYTES).await {
        Ok(b) => b,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE,
    };
    let message: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    if message.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0")
        || message.get("method").and_then(|v| v.as_str()).is_none()
        || message
            .get("id")
            .is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
        || message
            .get("params")
            .is_some_and(|v| !v.is_object() && !v.is_array())
    {
        return StatusCode::BAD_REQUEST;
    }
    // Never await with the registry mutex held; bounded queues reject overload.
    match tx.try_send(message) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(mpsc::error::TrySendError::Full(_)) => StatusCode::TOO_MANY_REQUESTS,
        Err(mpsc::error::TrySendError::Closed(_)) => StatusCode::NOT_FOUND,
    }
}

fn success_line(
    id: Option<&serde_json::Value>,
    result: &anyhow::Result<serde_json::Value>,
) -> String {
    match result {
        Ok(data) => serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": data,
        }))
        .unwrap_or_else(|error| error_line(id, &anyhow::Error::new(error))),
        Err(error) => error_line(id, error),
    }
}

fn error_line(id: Option<&serde_json::Value>, error: &anyhow::Error) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32603, "message": error.to_string() },
    }))
    .unwrap_or_else(|_| "{}".into())
}

/// Handle MCP tool calls by dispatching to the Engine.
async fn handle_tool_call(
    engine: &Engine,
    params: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match name {
        "memex8_timeline" => {
            let request = serde_json::from_value(arguments.clone())?;
            Ok(serde_json::to_value(
                engine.timeline_request(request).await?,
            )?)
        }
        "memex8_search" => {
            let compact = super::tools::boolean_argument(&arguments, "compact")?;
            let query = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());
            let min_score = arguments
                .get("min_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;

            let tags: Option<Vec<String>> = arguments
                .get("tags")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?;
            let visibility = arguments.get("visibility").and_then(|v| v.as_str());
            let outcome = engine
                .search_with_coverage(
                    query,
                    realm,
                    tags.as_deref(),
                    limit,
                    0,
                    min_score,
                    visibility,
                )
                .await?;
            super::tools::with_coverage(format_results(outcome.results, compact), &outcome.coverage)
        }
        "memex8_store" | "memex8_remember" => {
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
            // MCP tool callers can opt in to public explicitly; default
            // to private so an MCP-driven capture session can't leak
            // everything it touches to the A2A bridge.
            let visibility = arguments.get("visibility").and_then(|v| v.as_str());

            let memory_type = arguments.get("memory_type").and_then(|v| v.as_str());
            let source = arguments.get("source").and_then(|v| v.as_str());
            let certified = super::tools::boolean_argument(&arguments, "certified")?;
            let id = engine
                .store_memory_with_certification(
                    content,
                    tags,
                    realm_hint,
                    source.or(Some("mcp")),
                    visibility,
                    memory_type,
                    certified,
                )
                .await?;
            Ok(json!({ "id": id, "status": "stored" }))
        }
        "memex8_restore_duplicate" => {
            let operation_id = arguments
                .get("operation_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("operation_id is required"))?;
            let result = engine.restore_duplicate(operation_id).await?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&result)?}]}))
        }
        "memex8_unconsolidate" => {
            let memory_id = arguments
                .get("memory_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("memory_id is required"))?;
            let result = engine.unconsolidate(memory_id).await?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&result)?}]}))
        }
        "memex8_certify" => {
            let memory_id = arguments
                .get("memory_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("memory_id is required"))?;
            let revoke = super::tools::boolean_argument(&arguments, "revoke")?;
            let by = arguments.get("by").and_then(|v| v.as_str());
            let note = arguments.get("note").and_then(|v| v.as_str());
            let memory = engine
                .store()
                .set_certification(memory_id, revoke, by, note)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Memory not found: {}", memory_id))?;
            Ok(json!({"content": [{"type": "text", "text": serde_json::to_string(&memory)?}]}))
        }
        "memex8_recall" => {
            let compact = super::tools::boolean_argument(&arguments, "compact")?;
            let limit = arguments
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let realm = arguments.get("realm").and_then(|v| v.as_str());
            let results = engine.recall(limit, realm).await?;
            Ok(format_results(results, compact))
        }
        "memex8_get" => super::tools::get_memories(engine, &arguments).await,
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
            let realms: Vec<crate::api::routes::realms::RealmSummary> =
                realms.into_iter().map(Into::into).collect();
            Ok(json!({ "realms": realms }))
        }
        "memex8_realms_show" => {
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;
            let realm = engine.show_realm(name).await?;
            let realm: crate::api::routes::realms::RealmSummary = realm.into();
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

fn format_results(results: Vec<crate::engine::MemoryResult>, compact: bool) -> serde_json::Value {
    if compact {
        let count = results.len();
        return json!({"results": crate::engine::retrieval::RetrievalResults::new(results, true), "count": count});
    }
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
