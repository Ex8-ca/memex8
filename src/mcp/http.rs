use crate::api::server::AppState;
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
                    "initialize" => {
                        success_line(id.as_ref(), &Ok(super::dispatch::initialize_result()))
                    }
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
                            super::dispatch::call_tool(
                                &engine,
                                name,
                                super::publish::tool_arguments(&params),
                            ),
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
