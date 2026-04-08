//! Captures tracing log events into a ring buffer and broadcasts them via WebSocket.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::websocket::WsEvent;

const MAX_LOG_LINES: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

/// Shared ring buffer of recent log lines.
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogLine>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES))),
        }
    }

    fn push(&self, line: LogLine) {
        let mut buf = self.inner.lock();
        if buf.len() >= MAX_LOG_LINES {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    /// Get all buffered log lines.
    pub fn get_all(&self) -> Vec<LogLine> {
        self.inner.lock().iter().cloned().collect()
    }
}

/// A tracing layer that captures events into a ring buffer and optionally
/// broadcasts to WebSocket clients.
pub struct CaptureLayer {
    buffer: LogBuffer,
    ws_tx: Option<broadcast::Sender<WsEvent>>,
}

impl CaptureLayer {
    pub fn new(buffer: LogBuffer) -> Self {
        Self {
            buffer,
            ws_tx: None,
        }
    }

    pub fn with_ws(mut self, tx: broadcast::Sender<WsEvent>) -> Self {
        self.ws_tx = Some(tx);
        self
    }
}

/// Visitor that collects the `message` field from a tracing event.
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: Vec::new(),
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push((field.name().to_string(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push((field.name().to_string(), value.to_string()));
        }
    }
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();

        // Only capture INFO and above (skip DEBUG/TRACE noise)
        if level > Level::INFO {
            return;
        }

        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);

        // Append fields to message
        let mut msg = visitor.message;
        for (k, v) in &visitor.fields {
            msg.push_str(&format!(" {k}={v}"));
        }

        let line = LogLine {
            timestamp: chrono::Utc::now().format("%H:%M:%S").to_string(),
            level: level.to_string(),
            target: meta.target().to_string(),
            message: msg.clone(),
        };

        self.buffer.push(line);

        // Broadcast to WebSocket if available
        if let Some(tx) = &self.ws_tx {
            let _ = tx.send(WsEvent::Log {
                level: level.to_string(),
                target: meta.target().to_string(),
                message: msg,
                timestamp: chrono::Utc::now().format("%H:%M:%S").to_string(),
            });
        }
    }
}
