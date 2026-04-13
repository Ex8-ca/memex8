use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde_json::json;

pub async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    // Send initial connection message
    let _ = socket
        .send(Message::Text(format!("{}", json!({"type": "connected"})).into()))
        .await;

    // TODO: broadcast events from slumber/ingester
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            let _ = socket
                .send(Message::Text(format!("{}", json!({"echo": text.as_str()})).into()))
                .await;
        }
    }
}
