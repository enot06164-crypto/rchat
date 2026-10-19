//! Пересылка UI-событий ядра в ограниченную очередь handle.
//!
//! Очередь нужна, чтобы C-хост мог забирать события через
//! `rchat_receive_json`, не блокируя актор. При переполнении счётчик
//! потерянных событий отдаётся хосту отдельным событием `event_lagged`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rchat_core::events::{EventError, EventSubscription};
use tokio::sync::mpsc::{self, error::TrySendError};

use crate::json;

pub(crate) async fn forward(
    mut subscription: EventSubscription,
    tx: mpsc::Sender<Vec<u8>>,
    dropped: Arc<AtomicU64>,
) {
    loop {
        match subscription.next().await {
            Ok(event) => {
                let bytes = match serde_json::to_vec(&json::event(&event)) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(%error, "failed to serialize UI event");
                        continue;
                    }
                };
                if let Err(TrySendError::Full(_)) = tx.try_send(bytes) {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                if tx.is_closed() {
                    return;
                }
            }
            Err(EventError::Lagged(missed)) => {
                if let Err(TrySendError::Full(_)) = tx.try_send(json::lagged_event(missed)) {
                    dropped.fetch_add(missed, Ordering::Relaxed);
                }
                if tx.is_closed() {
                    return;
                }
            }
            Err(EventError::Closed) => return,
        }
    }
}
