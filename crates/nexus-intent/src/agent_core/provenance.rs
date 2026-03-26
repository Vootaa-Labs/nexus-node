//! Provenance recording and audit query model.
//!
//! Every agent-driven execution leaves a [`ProvenanceRecord`] that ties
//! together session, plan, capability, and transaction identifiers.
//! These records form a tamper-evident audit trail with dual storage:
//!
//! - **Hot path**: structured records for fast query.
//! - **Cold anchor**: hash digest anchored on-chain for integrity.
//!
//! # Chain Anchoring (TLD-07 §8)
//!
//! Provenance records are collected into [`AnchorBatch`] groups.
//! Each batch is digested with [`compute_anchor_digest`] and the
//! resulting hash is written on-chain. [`AnchorReceipt`] captures
//! the on-chain proof.  At any time, a record's inclusion can be
//! verified by recomputing the batch digest.
//!
//! # Query Views
//!
//! - By `agent_id` — most recent actions by a given agent.
//! - By `session_id` — full session audit trail.
//! - By `capability_token_id` — delegation usage trace.
//! - By `tx_hash` — reverse lookup plan / confirmation / capability.

use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs, TokenId};
use serde::{Deserialize, Serialize};

// ── ProvenanceStatus ────────────────────────────────────────────────────

/// Lifecycle status of a provenance record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvenanceStatus {
    /// Record created, execution pending.
    Pending,
    /// Execution succeeded.
    Committed,
    /// Execution failed.
    Failed,
    /// Session aborted before execution.
    Aborted,
    /// Session expired.
    Expired,
}

// ── ProvenanceRecord ────────────────────────────────────────────────────

/// Tamper-evident audit record for an agent-driven execution.
///
/// Every record links: session → plan → capability → transaction,
/// enabling complete traceability of agent actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    /// Unique provenance record identifier.
    pub provenance_id: Blake3Digest,
    /// Session that produced this record.
    pub session_id: Blake3Digest,
    /// Original request identifier.
    pub request_id: Blake3Digest,
    /// Agent that initiated the action.
    pub agent_id: AccountAddress,
    /// Parent agent (if this was a delegated action).
    pub parent_agent_id: Option<AccountAddress>,
    /// Capability token used (if any).
    pub capability_token_id: Option<TokenId>,
    /// BLAKE3 digest of the intent payload.
    pub intent_hash: Blake3Digest,
    /// BLAKE3 digest of the execution plan.
    pub plan_hash: Blake3Digest,
    /// Human/parent confirmation reference (if required).
    pub confirmation_ref: Option<Blake3Digest>,
    /// Resulting transaction hash on-chain (if executed).
    pub tx_hash: Option<Blake3Digest>,
    /// Current status.
    pub status: ProvenanceStatus,
    /// Timestamp when the record was created.
    pub created_at_ms: TimestampMs,
}

// ── Chain anchoring ─────────────────────────────────────────────────────

/// Domain tag for provenance chain anchor digest.
pub const PROVENANCE_ANCHOR_DOMAIN: &[u8] = b"nexus::provenance::chain_anchor::v1";

/// Compute the chain-anchor digest for a batch of provenance records.
///
/// `BLAKE3(PROVENANCE_ANCHOR_DOMAIN ‖ BCS(record_ids))`
///
/// This digest is periodically written on-chain to make the
/// provenance store tamper-evident.
pub fn compute_anchor_digest(
    record_ids: &[Blake3Digest],
) -> Result<Blake3Digest, crate::error::IntentError> {
    let bytes =
        bcs::to_bytes(record_ids).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROVENANCE_ANCHOR_DOMAIN);
    hasher.update(&bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

// ── AnchorBatch ─────────────────────────────────────────────────────────

/// A batch of provenance records prepared for on-chain anchoring.
///
/// The batch collects record IDs, computes a single digest, and
/// tracks attribution metadata for the anchoring transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorBatch {
    /// Monotonically increasing batch sequence number.
    pub batch_seq: u64,
    /// Provenance record IDs included in this batch.
    pub record_ids: Vec<Blake3Digest>,
    /// BLAKE3 anchor digest of `record_ids`.
    pub anchor_digest: Blake3Digest,
    /// Timestamp when the batch was prepared.
    pub created_at_ms: TimestampMs,
}

impl AnchorBatch {
    /// Build a new batch from a set of record IDs.
    ///
    /// Computes the anchor digest automatically.
    pub fn new(
        batch_seq: u64,
        record_ids: Vec<Blake3Digest>,
        now: TimestampMs,
    ) -> Result<Self, crate::error::IntentError> {
        let anchor_digest = compute_anchor_digest(&record_ids)?;
        Ok(Self {
            batch_seq,
            record_ids,
            anchor_digest,
            created_at_ms: now,
        })
    }

    /// Number of records in the batch.
    pub fn len(&self) -> usize {
        self.record_ids.len()
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.record_ids.is_empty()
    }

    /// Check if a specific record ID is included in this batch.
    pub fn contains(&self, record_id: &Blake3Digest) -> bool {
        self.record_ids.contains(record_id)
    }
}

// ── AnchorReceipt ───────────────────────────────────────────────────────

/// Receipt proving that a batch has been anchored on-chain.
///
/// After the `anchor_digest` is written as a transaction, the
/// receipt captures the resulting on-chain reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorReceipt {
    /// Batch sequence number.
    pub batch_seq: u64,
    /// The anchor digest that was written on-chain.
    pub anchor_digest: Blake3Digest,
    /// Transaction hash of the anchoring transaction.
    pub tx_hash: Blake3Digest,
    /// Block height at which the anchor was included.
    pub block_height: u64,
    /// Timestamp of anchor inclusion.
    pub anchored_at_ms: TimestampMs,
}

// ── Anchor verification ─────────────────────────────────────────────────

/// Verify that a set of record IDs matches a previously anchored digest.
///
/// Re-computes `compute_anchor_digest(record_ids)` and checks equality
/// against the receipt's `anchor_digest`.
pub fn verify_anchor(
    record_ids: &[Blake3Digest],
    receipt: &AnchorReceipt,
) -> Result<bool, crate::error::IntentError> {
    let recomputed = compute_anchor_digest(record_ids)?;
    Ok(recomputed == receipt.anchor_digest)
}

// ── ProvenanceQuery views ───────────────────────────────────────────────

/// Query result wrapper for provenance lookups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceQueryResult {
    /// Records matching the query filter.
    pub records: Vec<ProvenanceRecord>,
    /// Total count (may exceed `records.len()` if paginated).
    pub total_count: u64,
    /// Continuation token for pagination (opaque).
    pub cursor: Option<Blake3Digest>,
}

/// Parameters for provenance queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceQueryParams {
    /// Maximum number of records to return.
    pub limit: u32,
    /// Pagination cursor from a previous query.
    pub cursor: Option<Blake3Digest>,
    /// Only include records after this timestamp.
    pub after_ms: Option<TimestampMs>,
    /// Only include records before this timestamp.
    pub before_ms: Option<TimestampMs>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs, TokenId};

    fn sample_record() -> ProvenanceRecord {
        ProvenanceRecord {
            provenance_id: Blake3Digest([0x01; 32]),
            session_id: Blake3Digest([0x02; 32]),
            request_id: Blake3Digest([0x03; 32]),
            agent_id: AccountAddress([0xAA; 32]),
            parent_agent_id: None,
            capability_token_id: Some(TokenId::Native),
            intent_hash: Blake3Digest([0x04; 32]),
            plan_hash: Blake3Digest([0x05; 32]),
            confirmation_ref: None,
            tx_hash: Some(Blake3Digest([0x06; 32])),
            status: ProvenanceStatus::Committed,
            created_at_ms: TimestampMs(1_700_000_000_000),
        }
    }

    #[test]
    fn provenance_bcs_round_trip() {
        let rec = sample_record();
        let bytes = bcs::to_bytes(&rec).unwrap();
        let decoded: ProvenanceRecord = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn provenance_status_variants() {
        for status in [
            ProvenanceStatus::Pending,
            ProvenanceStatus::Committed,
            ProvenanceStatus::Failed,
            ProvenanceStatus::Aborted,
            ProvenanceStatus::Expired,
        ] {
            let bytes = bcs::to_bytes(&status).unwrap();
            let decoded: ProvenanceStatus = bcs::from_bytes(&bytes).unwrap();
            assert_eq!(status, decoded);
        }
    }

    #[test]
    fn anchor_digest_deterministic() {
        let ids = vec![Blake3Digest([0x01; 32]), Blake3Digest([0x02; 32])];
        let d1 = compute_anchor_digest(&ids).unwrap();
        let d2 = compute_anchor_digest(&ids).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn anchor_digest_changes_with_records() {
        let ids1 = vec![Blake3Digest([0x01; 32])];
        let ids2 = vec![Blake3Digest([0x01; 32]), Blake3Digest([0x02; 32])];
        let d1 = compute_anchor_digest(&ids1).unwrap();
        let d2 = compute_anchor_digest(&ids2).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn anchor_empty_batch() {
        let ids: Vec<Blake3Digest> = vec![];
        assert!(compute_anchor_digest(&ids).is_ok());
    }

    // ── AnchorBatch tests ───────────────────────────────────────────

    #[test]
    fn anchor_batch_new() {
        let ids = vec![Blake3Digest([0x01; 32]), Blake3Digest([0x02; 32])];
        let batch = AnchorBatch::new(1, ids.clone(), TimestampMs(1_000)).unwrap();
        assert_eq!(batch.batch_seq, 1);
        assert_eq!(batch.record_ids, ids);
        assert_eq!(batch.len(), 2);
        assert!(!batch.is_empty());
    }

    #[test]
    fn anchor_batch_contains() {
        let id = Blake3Digest([0x01; 32]);
        let batch = AnchorBatch::new(1, vec![id], TimestampMs(1_000)).unwrap();
        assert!(batch.contains(&id));
        assert!(!batch.contains(&Blake3Digest([0xFF; 32])));
    }

    #[test]
    fn anchor_batch_empty() {
        let batch = AnchorBatch::new(0, vec![], TimestampMs(1_000)).unwrap();
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn anchor_batch_digest_matches_standalone() {
        let ids = vec![Blake3Digest([0x01; 32]), Blake3Digest([0x02; 32])];
        let batch = AnchorBatch::new(1, ids.clone(), TimestampMs(1_000)).unwrap();
        let standalone = compute_anchor_digest(&ids).unwrap();
        assert_eq!(batch.anchor_digest, standalone);
    }

    #[test]
    fn anchor_batch_bcs_round_trip() {
        let batch =
            AnchorBatch::new(42, vec![Blake3Digest([0x01; 32])], TimestampMs(1_000)).unwrap();
        let bytes = bcs::to_bytes(&batch).unwrap();
        let decoded: AnchorBatch = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(batch, decoded);
    }

    // ── AnchorReceipt tests ─────────────────────────────────────────

    #[test]
    fn anchor_receipt_bcs_round_trip() {
        let receipt = AnchorReceipt {
            batch_seq: 1,
            anchor_digest: Blake3Digest([0x10; 32]),
            tx_hash: Blake3Digest([0x20; 32]),
            block_height: 100,
            anchored_at_ms: TimestampMs(2_000),
        };
        let bytes = bcs::to_bytes(&receipt).unwrap();
        let decoded: AnchorReceipt = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(receipt, decoded);
    }

    // ── Anchor verification tests ───────────────────────────────────

    #[test]
    fn verify_anchor_matches() {
        let ids = vec![Blake3Digest([0x01; 32]), Blake3Digest([0x02; 32])];
        let digest = compute_anchor_digest(&ids).unwrap();
        let receipt = AnchorReceipt {
            batch_seq: 1,
            anchor_digest: digest,
            tx_hash: Blake3Digest([0x99; 32]),
            block_height: 50,
            anchored_at_ms: TimestampMs(3_000),
        };
        assert!(verify_anchor(&ids, &receipt).unwrap());
    }

    #[test]
    fn verify_anchor_tampered() {
        let ids = vec![Blake3Digest([0x01; 32])];
        let digest = compute_anchor_digest(&ids).unwrap();
        let receipt = AnchorReceipt {
            batch_seq: 1,
            anchor_digest: digest,
            tx_hash: Blake3Digest([0x99; 32]),
            block_height: 50,
            anchored_at_ms: TimestampMs(3_000),
        };
        // Tamper with the record set
        let tampered = vec![Blake3Digest([0x01; 32]), Blake3Digest([0xFF; 32])];
        assert!(!verify_anchor(&tampered, &receipt).unwrap());
    }

    // ── ProvenanceQueryResult tests ─────────────────────────────────

    #[test]
    fn query_result_bcs_round_trip() {
        let result = ProvenanceQueryResult {
            records: vec![sample_record()],
            total_count: 1,
            cursor: None,
        };
        let bytes = bcs::to_bytes(&result).unwrap();
        let decoded: ProvenanceQueryResult = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(result, decoded);
    }

    #[test]
    fn query_params_bcs_round_trip() {
        let params = ProvenanceQueryParams {
            limit: 50,
            cursor: Some(Blake3Digest([0xAA; 32])),
            after_ms: Some(TimestampMs(1_000)),
            before_ms: None,
        };
        let bytes = bcs::to_bytes(&params).unwrap();
        let decoded: ProvenanceQueryParams = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(params, decoded);
    }
}
