//! WebSocket event subscription module.
//!
//! Provides real-time event streaming over WebSocket connections.
//! Uses `tokio::broadcast` for fan-out to all connected clients.
//!
//! ## Wire protocol
//!
//! Clients connect to `GET /v2/ws` and send a JSON subscription message:
//! ```json
//! { "subscribe": ["new_commit", "intent_status"] }
//! ```
//!
//! The server then pushes matching events as JSON text frames:
//! ```json
//! { "event": "new_commit", "data": { ... } }
//! ```

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::AppState;
use crate::dto::{ConsensusStatusDto, IntentStatusDto, TransactionReceiptDto};

// ── Event types ─────────────────────────────────────────────────────────

/// Events broadcast to WebSocket subscribers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum NodeEvent {
    /// A new batch has been committed (BFT finality).
    #[serde(rename = "new_commit")]
    NewCommit {
        /// Monotonically increasing commit sequence.
        sequence: u64,
        /// Number of certificates in the committed sub-DAG.
        certificate_count: usize,
        /// Wall-clock finality timestamp.
        committed_at_ms: u64,
    },
    /// A transaction has been executed.
    #[serde(rename = "transaction_executed")]
    TransactionExecuted(TransactionReceiptDto),
    /// An intent has changed status.
    #[serde(rename = "intent_status")]
    IntentStatusChanged(IntentStatusDto),
    /// Consensus status update (epoch change, etc.).
    #[serde(rename = "consensus_status")]
    ConsensusStatus(ConsensusStatusDto),
}

/// Client-sent subscription request.
#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    /// Event types to subscribe to.
    pub subscribe: Vec<String>,
}

/// Broadcast channel capacity.
const BROADCAST_CAPACITY: usize = 1024;

/// Create a new broadcast channel for node events.
pub fn event_channel() -> (broadcast::Sender<NodeEvent>, broadcast::Receiver<NodeEvent>) {
    broadcast::channel(BROADCAST_CAPACITY)
}

// ── Router ──────────────────────────────────────────────────────────────

/// Build the WebSocket router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/ws", get(ws_upgrade))
}

/// Handle WebSocket upgrade request.
async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use std::sync::atomic::Ordering;

    // Enforce max concurrent WebSocket connections.
    let current = state.ws_connection_count.load(Ordering::Relaxed);
    if current >= state.max_ws_connections {
        return axum::response::Response::builder()
            .status(http::StatusCode::SERVICE_UNAVAILABLE)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({
                    "error": "SERVICE_UNAVAILABLE",
                    "message": "maximum WebSocket connections reached"
                })
                .to_string(),
            ))
            .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
            .into_response();
    }

    ws.on_upgrade(move |socket| handle_ws(socket, state))
        .into_response()
}

/// Run the WebSocket connection loop.
async fn handle_ws(mut socket: WebSocket, state: Arc<AppState>) {
    use std::sync::atomic::Ordering;

    // Track active connection count.
    state.ws_connection_count.fetch_add(1, Ordering::Relaxed);
    crate::metrics::ws_connection_opened();

    // RAII guard to decrement on exit (normal close, disconnect, panic).
    struct WsGuard(Arc<AppState>);
    impl Drop for WsGuard {
        fn drop(&mut self) {
            self.0
                .ws_connection_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            crate::metrics::ws_connection_closed();
        }
    }
    let _guard = WsGuard(Arc::clone(&state));

    // Wait for subscription message from client.
    let filters = match wait_for_subscribe(&mut socket).await {
        Some(f) => f,
        None => return, // Client disconnected before subscribing.
    };

    // Subscribe to the broadcast channel.
    let Some(ref tx) = state.events else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let mut rx = tx.subscribe();

    // Push matching events until the client disconnects.
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(ev) => {
                        if !matches_filter(&ev, &filters) {
                            continue;
                        }
                        let Ok(json) = serde_json::to_string(&ev) else {
                            continue;
                        };
                        if socket.send(Message::Text(json)).await.is_err() {
                            break; // Client disconnected.
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Client fell behind — notify and continue.
                        let msg = serde_json::json!({
                            "warning": "lagged",
                            "missed_events": n
                        });
                        let _ = socket
                            .send(Message::Text(msg.to_string()))
                            .await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Also check for client-initiated close.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // Ignore other client messages.
                }
            }
        }
    }
}

/// Wait for the client's subscription message.
///
/// Returns the set of event type filters, or `None` if the client
/// disconnects before sending a valid subscription.
async fn wait_for_subscribe(socket: &mut WebSocket) -> Option<Vec<String>> {
    // Give client 10 seconds to subscribe.
    let timeout = tokio::time::timeout(std::time::Duration::from_secs(10), socket.recv()).await;
    match timeout {
        Ok(Some(Ok(Message::Text(text)))) => {
            let req: SubscribeRequest = serde_json::from_str(&text).ok()?;
            if req.subscribe.is_empty() {
                // Empty filter = subscribe to everything.
                None
            } else {
                Some(req.subscribe)
            }
        }
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => None,
        _ => {
            // Subscribe to everything on unexpected message type.
            Some(Vec::new())
        }
    }
}

/// Check if an event matches the client's subscription filter.
fn matches_filter(event: &NodeEvent, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true; // No filter = all events.
    }
    let event_name = match event {
        NodeEvent::NewCommit { .. } => "new_commit",
        NodeEvent::TransactionExecuted(_) => "transaction_executed",
        NodeEvent::IntentStatusChanged(_) => "intent_status",
        NodeEvent::ConsensusStatus(_) => "consensus_status",
    };
    filters.iter().any(|f| f == event_name)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::*;

    #[test]
    fn node_event_serializes_with_tag() {
        let event = NodeEvent::NewCommit {
            sequence: 42,
            certificate_count: 3,
            committed_at_ms: 1_700_000_000_000,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(json["event"], "new_commit");
        assert_eq!(json["data"]["sequence"], 42);
    }

    #[test]
    fn matches_filter_all_when_empty() {
        let event = NodeEvent::NewCommit {
            sequence: 1,
            certificate_count: 1,
            committed_at_ms: 0,
        };
        assert!(matches_filter(&event, &[]));
    }

    #[test]
    fn matches_filter_specific() {
        let event = NodeEvent::NewCommit {
            sequence: 1,
            certificate_count: 1,
            committed_at_ms: 0,
        };
        assert!(matches_filter(&event, &["new_commit".into()]));
        assert!(!matches_filter(&event, &["intent_status".into()]));
    }

    #[test]
    fn subscribe_request_deserializes() {
        let json = r#"{"subscribe": ["new_commit", "intent_status"]}"#;
        let req: SubscribeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.subscribe.len(), 2);
        assert_eq!(req.subscribe[0], "new_commit");
    }

    #[test]
    fn event_channel_sends_and_receives() {
        let (tx, mut rx) = event_channel();
        let event = NodeEvent::NewCommit {
            sequence: 100,
            certificate_count: 5,
            committed_at_ms: 999,
        };
        tx.send(event).unwrap();
        let received = rx.try_recv().unwrap();
        match received {
            NodeEvent::NewCommit { sequence, .. } => assert_eq!(sequence, 100),
            _ => panic!("unexpected event type"),
        }
    }

    #[test]
    fn intent_status_event_serializes() {
        let event = NodeEvent::IntentStatusChanged(IntentStatusDto {
            intent_id: Blake3Digest([0xCC; 32]),
            status: nexus_intent::types::IntentStatus::Pending,
        });
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("intent_status"));
    }
}
