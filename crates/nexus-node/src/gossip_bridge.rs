// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Gossip → Mempool bridge — spawns a task that receives transactions from
//! the P2P gossip layer and inserts validated ones into the local mempool.
//!
//! The bridge subscribes to [`Topic::Transaction`] on the [`GossipHandle`],
//! decodes incoming BCS-encoded [`SignedTransaction`]s, and feeds them into
//! the [`Mempool`].

use std::sync::Arc;

use nexus_crypto::{DilithiumSigner, Signer};
use nexus_execution::types::SignedTransaction;
use nexus_network::types::Topic;
use nexus_network::GossipHandle;
use nexus_primitives::EpochNumber;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Maximum encoded size for a transaction message (128 KB).
///
/// Defense-in-depth: rejects oversized payloads *before* BCS deserialization
/// to prevent allocation-based DoS. The GossipSub transport layer enforces
/// a 4 MB `max_transmit_size`, but transactions should never approach that.
const MAX_TX_MESSAGE_SIZE: usize = 128 * 1024;

use crate::mempool::{InsertResult, Mempool};
use crate::node_metrics;

/// Spawn a background task that bridges gossip → mempool.
///
/// The task subscribes to [`Topic::Transaction`] and runs until the broadcast
/// channel closes (which happens when the network service shuts down).
///
/// # Arguments
/// - `gossip`: handle to the gossip subsystem
/// - `mempool`: shared mempool to insert transactions into
/// - `current_epoch`: current epoch for expiry validation
///
/// Returns a `JoinHandle` so the caller can await or abort.
pub async fn spawn_gossip_mempool_bridge(
    gossip: GossipHandle,
    mempool: Arc<Mempool>,
    current_epoch: Arc<std::sync::atomic::AtomicU64>,
) -> Result<JoinHandle<()>, nexus_network::NetworkError> {
    gossip.subscribe(Topic::Transaction).await?;
    let mut rx = gossip.topic_receiver(Topic::Transaction);

    let handle = tokio::spawn(async move {
        debug!("gossip→mempool bridge started");

        loop {
            match rx.recv().await {
                Ok(data) => {
                    handle_incoming_tx(&data, &mempool, &current_epoch);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        skipped = n,
                        "gossip→mempool bridge lagged, some messages dropped"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("gossip broadcast channel closed — bridge stopping");
                    break;
                }
            }
        }

        debug!("gossip→mempool bridge stopped");
    });

    Ok(handle)
}

/// Decode and validate a single incoming transaction, then insert into mempool.
fn handle_incoming_tx(
    data: &[u8],
    mempool: &Mempool,
    current_epoch: &std::sync::atomic::AtomicU64,
) {
    // 0. Size guard — reject before BCS deserialization (defense-in-depth)
    if data.len() > MAX_TX_MESSAGE_SIZE {
        debug!(
            size = data.len(),
            limit = MAX_TX_MESSAGE_SIZE,
            "gossip: transaction message exceeds size limit — dropping"
        );
        return;
    }

    // 1. Decode BCS
    let tx: SignedTransaction = match bcs::from_bytes(data) {
        Ok(tx) => tx,
        Err(e) => {
            debug!(error = %e, "gossip: failed to decode transaction — dropping");
            return;
        }
    };

    // 2. Basic validation: check expiry_epoch > current_epoch
    let epoch = EpochNumber(current_epoch.load(std::sync::atomic::Ordering::Relaxed));
    if tx.body.expiry_epoch <= epoch {
        debug!(
            expiry = tx.body.expiry_epoch.0,
            current = epoch.0,
            "gossip: expired transaction — dropping"
        );
        return;
    }

    // 3. Verify digest matches body (integrity check)
    match nexus_execution::types::compute_tx_digest(&tx.body) {
        Ok(expected) if expected == tx.digest => { /* valid */ }
        Ok(expected) => {
            debug!(
                expected = %expected.to_hex(),
                actual = %tx.digest.to_hex(),
                "gossip: transaction digest mismatch — dropping"
            );
            return;
        }
        Err(e) => {
            debug!(error = %e, "gossip: failed to compute transaction digest — dropping");
            return;
        }
    }

    // 3b. Verify ML-DSA signature over the transaction digest (SEC-H5).
    //     The digest was already verified to match the body in step 3.
    //     Reject forged or tampered transactions before they enter the
    //     mempool, consensus, and execution pipeline.
    {
        use nexus_execution::types::TX_DOMAIN;
        if let Err(e) = DilithiumSigner::verify(
            &tx.sender_pk,
            TX_DOMAIN,
            tx.digest.as_bytes(),
            &tx.signature,
        ) {
            debug!(
                error = %e,
                sender = %tx.body.sender,
                "gossip: invalid transaction signature — dropping forged tx"
            );
            return;
        }
    }

    // 4. Insert into mempool
    match mempool.insert(tx) {
        InsertResult::Accepted => {
            node_metrics::mempool_enqueue(1);
            debug!("gossip: transaction accepted into mempool");
        }
        InsertResult::Duplicate => {
            debug!("gossip: duplicate transaction — skipped");
        }
        InsertResult::PoolFull => {
            warn!("gossip: mempool full — transaction rejected");
        }
        InsertResult::InvalidShard => {
            debug!("gossip: invalid target_shard — transaction rejected");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mempool::MempoolConfig;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{
        compute_tx_digest, TransactionBody, TransactionPayload, TX_DOMAIN,
    };
    use nexus_primitives::{AccountAddress, Amount, ShardId, TokenId};

    /// Helper: create a valid signed transaction.
    fn make_signed_tx(seq: u64, expiry_epoch: u64) -> SignedTransaction {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress::from_dilithium_pubkey(vk.as_bytes());
        let body = TransactionBody {
            sender,
            sequence_number: seq,
            expiry_epoch: EpochNumber(expiry_epoch),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: Some(ShardId(0)),
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([2u8; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).expect("digest");
        let sig = DilithiumSigner::sign(&sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: vk,
            digest,
        }
    }

    #[test]
    fn handle_valid_tx_inserts_into_mempool() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tx = make_signed_tx(1, 100);
        let data = bcs::to_bytes(&tx).expect("encode");

        handle_incoming_tx(&data, &mempool, &epoch);
        assert_eq!(mempool.len(), 1);
    }

    #[test]
    fn handle_expired_tx_drops() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(50));
        let tx = make_signed_tx(1, 10); // expires at epoch 10, current is 50
        let data = bcs::to_bytes(&tx).expect("encode");

        handle_incoming_tx(&data, &mempool, &epoch);
        assert_eq!(mempool.len(), 0, "expired tx should be dropped");
    }

    #[test]
    fn handle_invalid_bcs_drops() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let bad_data = vec![0xFF, 0xFE, 0xFD];

        handle_incoming_tx(&bad_data, &mempool, &epoch);
        assert_eq!(mempool.len(), 0, "invalid BCS should be dropped");
    }

    #[test]
    fn handle_duplicate_tx_skipped() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tx = make_signed_tx(1, 100);
        let data = bcs::to_bytes(&tx).expect("encode");

        handle_incoming_tx(&data, &mempool, &epoch);
        handle_incoming_tx(&data, &mempool, &epoch);
        assert_eq!(mempool.len(), 1, "duplicate should be skipped");
    }

    #[test]
    fn handle_tampered_digest_drops() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut tx = make_signed_tx(1, 100);
        // Tamper the digest
        tx.digest = nexus_primitives::TxDigest::from_bytes([0xAA; 32]);
        let data = bcs::to_bytes(&tx).expect("encode");

        handle_incoming_tx(&data, &mempool, &epoch);
        assert_eq!(mempool.len(), 0, "tampered digest should be rejected");
    }

    #[test]
    fn handle_mempool_full_rejects() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 1,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let tx1 = make_signed_tx(1, 100);
        let tx2 = make_signed_tx(2, 100);
        let data1 = bcs::to_bytes(&tx1).expect("encode");
        let data2 = bcs::to_bytes(&tx2).expect("encode");

        handle_incoming_tx(&data1, &mempool, &epoch);
        handle_incoming_tx(&data2, &mempool, &epoch);
        assert_eq!(mempool.len(), 1, "mempool should be at capacity");
    }

    #[tokio::test]
    async fn bridge_spawns_and_aborts_cleanly() {
        use nexus_network::{NetworkConfig, NetworkService};

        let config = NetworkConfig::for_testing();
        let (net_handle, service) = NetworkService::build(&config).expect("build");
        let shutdown = net_handle.transport.clone();
        let net_task = tokio::spawn(service.run());

        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let bridge = spawn_gossip_mempool_bridge(net_handle.gossip.clone(), mempool.clone(), epoch)
            .await
            .expect("bridge should spawn");

        // Bridge is running — abort it
        bridge.abort();
        let _ = bridge.await; // JoinError::Cancelled is expected

        // Mempool should still be usable after bridge stops
        assert_eq!(mempool.len(), 0);

        // Cleanup: drop handles so network can shut down
        drop(net_handle);
        shutdown.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), net_task).await;
    }

    #[test]
    fn handle_oversized_tx_drops() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        // Craft a payload larger than MAX_TX_MESSAGE_SIZE
        let oversized = vec![0u8; MAX_TX_MESSAGE_SIZE + 1];

        handle_incoming_tx(&oversized, &mempool, &epoch);
        assert_eq!(
            mempool.len(),
            0,
            "oversized message should be rejected before BCS decode"
        );
    }

    #[test]
    fn handle_at_limit_tx_attempts_decode() {
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        // Exactly at limit — should pass the size check (but fail BCS decode)
        let at_limit = vec![0u8; MAX_TX_MESSAGE_SIZE];

        handle_incoming_tx(&at_limit, &mempool, &epoch);
        // Not inserted because BCS decode fails, but the size check passed
        assert_eq!(mempool.len(), 0);
    }

    // ── Phase A acceptance tests ─────────────────────────────────────────

    #[test]
    fn forged_transaction_should_be_rejected_at_gossip_ingress() {
        // A-4 / SEC-H5: a transaction with a forged signature must be
        // rejected before reaching the mempool.
        let mempool = Arc::new(Mempool::new(&MempoolConfig {
            capacity: 100,
            num_shards: 1,
        }));
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Create a valid-looking tx, then forge the signature.
        let mut tx = make_signed_tx(1, 100);
        // Replace signature with one from a different keypair.
        let (forger_sk, _forger_vk) = DilithiumSigner::generate_keypair();
        tx.signature = DilithiumSigner::sign(&forger_sk, TX_DOMAIN, tx.digest.as_bytes());
        // Keep the original sender_pk (won't match the forged signature).

        let data = bcs::to_bytes(&tx).expect("encode");
        handle_incoming_tx(&data, &mempool, &epoch);

        assert_eq!(
            mempool.len(),
            0,
            "transaction with forged signature must not enter mempool"
        );
    }
}
