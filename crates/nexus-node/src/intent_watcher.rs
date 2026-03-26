// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent lifecycle watcher — monitors execution receipts and updates the
//! [`IntentTracker`] when intent-related transactions are confirmed or fail.
//!
//! Subscribes to the [`NodeEvent`] broadcast channel emitted by the
//! execution bridge and matches `TransactionExecuted` events against
//! tracked intent transaction digests.

use std::sync::Arc;

use nexus_rpc::dto::IntentStatusDto;
use nexus_rpc::intent_tracker::IntentTracker;
use nexus_rpc::ws::NodeEvent;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::debug;

/// Spawn a background task that watches execution events and updates
/// the intent tracker.
///
/// When a status transition occurs and `events_tx` is provided, the
/// watcher emits a [`NodeEvent::IntentStatusChanged`] so WebSocket
/// subscribers receive real-time notifications.
///
/// Returns a `JoinHandle` so the caller can await or abort.
pub fn spawn_intent_watcher(
    mut events_rx: broadcast::Receiver<NodeEvent>,
    tracker: Arc<IntentTracker>,
    events_tx: Option<broadcast::Sender<NodeEvent>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        debug!("intent watcher started");

        loop {
            match events_rx.recv().await {
                Ok(NodeEvent::TransactionExecuted(receipt_dto)) => match receipt_dto.status {
                    nexus_rpc::dto::ExecutionStatusDto::Success => {
                        if let Some((intent_id, status)) =
                            tracker.on_tx_executed(&receipt_dto.tx_digest, receipt_dto.gas_used)
                        {
                            debug!(
                                tx = %receipt_dto.tx_digest.to_hex(),
                                ?status,
                                "intent watcher: tx executed, intent status updated"
                            );
                            emit_intent_status(&events_tx, intent_id, status);
                        }
                    }
                    nexus_rpc::dto::ExecutionStatusDto::MoveAbort { location, code } => {
                        let reason = format!("MoveAbort at {location} with code {code}");
                        if let Some((intent_id, status)) =
                            tracker.on_tx_failed(&receipt_dto.tx_digest, reason)
                        {
                            debug!(
                                tx = %receipt_dto.tx_digest.to_hex(),
                                ?status,
                                "intent watcher: tx aborted, intent marked failed"
                            );
                            emit_intent_status(&events_tx, intent_id, status);
                        }
                    }
                    nexus_rpc::dto::ExecutionStatusDto::OutOfGas => {
                        if let Some((intent_id, status)) =
                            tracker.on_tx_failed(&receipt_dto.tx_digest, "out of gas".into())
                        {
                            debug!(
                                tx = %receipt_dto.tx_digest.to_hex(),
                                ?status,
                                "intent watcher: tx out of gas, intent marked failed"
                            );
                            emit_intent_status(&events_tx, intent_id, status);
                        }
                    }
                    other => {
                        let reason = format!("{other:?}");
                        if let Some((intent_id, status)) =
                            tracker.on_tx_failed(&receipt_dto.tx_digest, reason)
                        {
                            debug!(
                                tx = %receipt_dto.tx_digest.to_hex(),
                                ?status,
                                "intent watcher: tx rejected, intent marked failed"
                            );
                            emit_intent_status(&events_tx, intent_id, status);
                        }
                    }
                },
                Ok(_) => {
                    // Ignore non-transaction events
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(skipped = n, "intent watcher lagged, some events missed");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("intent watcher: event channel closed — stopping");
                    break;
                }
            }
        }

        debug!("intent watcher stopped");
    })
}

/// Emit an `IntentStatusChanged` event via the broadcast channel if available.
fn emit_intent_status(
    events_tx: &Option<broadcast::Sender<NodeEvent>>,
    intent_id: nexus_primitives::IntentId,
    status: nexus_intent::types::IntentStatus,
) {
    if let Some(tx) = events_tx {
        let _ = tx.send(NodeEvent::IntentStatusChanged(IntentStatusDto {
            intent_id,
            status,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{Blake3Digest, TimestampMs, TxDigest};
    use nexus_rpc::dto::{ExecutionStatusDto, TransactionReceiptDto};
    use nexus_rpc::ws::NodeEvent;

    fn make_receipt_dto(digest_seed: u8, status: ExecutionStatusDto) -> TransactionReceiptDto {
        TransactionReceiptDto {
            tx_digest: TxDigest::from_bytes([digest_seed; 32]),
            commit_seq: nexus_primitives::CommitSequence(1),
            shard_id: nexus_primitives::ShardId(0),
            status,
            gas_used: 500,
            timestamp: TimestampMs::now(),
        }
    }

    #[tokio::test]
    async fn watcher_updates_tracker_on_success() {
        let tracker = Arc::new(IntentTracker::new());
        let intent_id = Blake3Digest([0xAA; 32]);
        let tx_hash = TxDigest::from_bytes([0x01; 32]);

        tracker.register(intent_id, vec![tx_hash]);

        let (tx, rx) = broadcast::channel(16);
        let handle = spawn_intent_watcher(rx, tracker.clone(), None);

        // Send a successful execution event
        let receipt = make_receipt_dto(0x01, ExecutionStatusDto::Success);
        tx.send(NodeEvent::TransactionExecuted(receipt)).unwrap();

        // Give the watcher time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let record = tracker.status(&intent_id).unwrap();
        assert!(matches!(
            record.status,
            nexus_intent::types::IntentStatus::Completed { gas_used: 500 }
        ));

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn watcher_marks_failed_on_abort() {
        let tracker = Arc::new(IntentTracker::new());
        let intent_id = Blake3Digest([0xBB; 32]);
        let tx_hash = TxDigest::from_bytes([0x02; 32]);

        tracker.register(intent_id, vec![tx_hash]);

        let (tx, rx) = broadcast::channel(16);
        let handle = spawn_intent_watcher(rx, tracker.clone(), None);

        let receipt = make_receipt_dto(
            0x02,
            ExecutionStatusDto::MoveAbort {
                location: "0x1::token".into(),
                code: 42,
            },
        );
        tx.send(NodeEvent::TransactionExecuted(receipt)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let record = tracker.status(&intent_id).unwrap();
        assert!(matches!(
            record.status,
            nexus_intent::types::IntentStatus::Failed { .. }
        ));

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn watcher_emits_intent_status_changed_on_success() {
        let tracker = Arc::new(IntentTracker::new());
        let intent_id = Blake3Digest([0xCC; 32]);
        let tx_hash = TxDigest::from_bytes([0x03; 32]);

        tracker.register(intent_id, vec![tx_hash]);

        let (tx, rx) = broadcast::channel(16);
        // Pass in the same sender so the watcher emits IntentStatusChanged
        let mut ws_rx = tx.subscribe();
        let handle = spawn_intent_watcher(rx, tracker.clone(), Some(tx.clone()));

        let receipt = make_receipt_dto(0x03, ExecutionStatusDto::Success);
        tx.send(NodeEvent::TransactionExecuted(receipt)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The watcher should have emitted an IntentStatusChanged event
        // (skip the TransactionExecuted we sent — watcher re-emits status)
        let mut found = false;
        while let Ok(event) = ws_rx.try_recv() {
            if let NodeEvent::IntentStatusChanged(dto) = event {
                assert_eq!(dto.intent_id, intent_id);
                assert!(matches!(
                    dto.status,
                    nexus_intent::types::IntentStatus::Completed { gas_used: 500 }
                ));
                found = true;
                break;
            }
        }
        assert!(found, "expected IntentStatusChanged event to be emitted");

        // The watcher holds a clone of events_tx, so the channel never
        // closes on its own — abort explicitly.
        handle.abort();
    }

    #[tokio::test]
    async fn watcher_emits_intent_status_changed_on_failure() {
        let tracker = Arc::new(IntentTracker::new());
        let intent_id = Blake3Digest([0xDD; 32]);
        let tx_hash = TxDigest::from_bytes([0x04; 32]);

        tracker.register(intent_id, vec![tx_hash]);

        let (tx, rx) = broadcast::channel(16);
        let mut ws_rx = tx.subscribe();
        let handle = spawn_intent_watcher(rx, tracker.clone(), Some(tx.clone()));

        let receipt = make_receipt_dto(0x04, ExecutionStatusDto::OutOfGas);
        tx.send(NodeEvent::TransactionExecuted(receipt)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut found = false;
        while let Ok(event) = ws_rx.try_recv() {
            if let NodeEvent::IntentStatusChanged(dto) = event {
                assert_eq!(dto.intent_id, intent_id);
                assert!(matches!(
                    dto.status,
                    nexus_intent::types::IntentStatus::Failed { .. }
                ));
                found = true;
                break;
            }
        }
        assert!(found, "expected IntentStatusChanged event for failure");

        handle.abort();
    }
}
