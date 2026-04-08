use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::queue_manager::QueueStatus;
use crate::websocket::WsEvent;

/// Watch [`QueueStatus`] changes and broadcast them as [`WsEvent::QueueUpdate`]
/// messages to all connected WebSocket clients.
pub async fn run_status_broadcaster(
    mut status_rx: watch::Receiver<QueueStatus>,
    ws_tx: broadcast::Sender<WsEvent>,
    cancel: CancellationToken,
) {
    info!("status broadcaster started");

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            changed = status_rx.changed() => {
                if changed.is_err() {
                    // The sender was dropped — nothing left to watch.
                    break;
                }
                let status = status_rx.borrow().clone();
                let _ = ws_tx.send(WsEvent::Queue {
                    queue_count: 0, // TODO: query DB for accurate count
                    is_processing: status.is_processing,
                    current_job: status.current_job,
                });
            }
        }
    }

    info!("status broadcaster stopped");
}
