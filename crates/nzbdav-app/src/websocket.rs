use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::warn;

/// Events that can be broadcast to WebSocket clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum WsEvent {
    Queue {
        queue_count: usize,
        is_processing: bool,
        current_job: Option<String>,
    },
    History {
        total_count: i64,
    },
    Status {
        status: String,
    },
    Log {
        level: String,
        target: String,
        message: String,
        timestamp: String,
    },
}

/// Manages a broadcast channel for pushing real-time events to WebSocket
/// clients.
pub struct WebSocketManager {
    tx: broadcast::Sender<WsEvent>,
}

impl WebSocketManager {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self { tx }
    }

    /// Get a sender handle for broadcasting events from other components.
    pub fn sender(&self) -> broadcast::Sender<WsEvent> {
        self.tx.clone()
    }

    /// Axum handler for WebSocket upgrade requests.
    pub async fn ws_handler(
        ws: WebSocketUpgrade,
        State(tx): State<broadcast::Sender<WsEvent>>,
    ) -> Response {
        ws.on_upgrade(|socket| handle_socket(socket, tx))
    }
}

impl Default for WebSocketManager {
    fn default() -> Self {
        Self::new()
    }
}

async fn handle_socket(mut socket: WebSocket, tx: broadcast::Sender<WsEvent>) {
    let mut rx = tx.subscribe();

    loop {
        tokio::select! {
            // Forward broadcast events to the WebSocket client.
            event = rx.recv() => {
                match event {
                    Ok(ev) => {
                        let json = serde_json::to_string(&ev).unwrap_or_default();
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged by {n} messages");
                    }
                    Err(_) => break, // Channel closed
                }
            }
            // Handle incoming messages from client (ping/pong, close).
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {} // Ignore other messages
                }
            }
        }
    }
}
