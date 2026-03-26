//! A2A Negotiation/Settlement Orchestrator.
//!
//! Manages the lifecycle of multiple A2A negotiations, processing
//! messages from either party and driving the state machine from
//! `Proposed` through `Settled` (or `Failed`/`Rejected`/`TimedOut`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};

use crate::agent_core::a2a::{
    apply_a2a_message, compute_negotiation_digest, validate_a2a_message, A2aMessage,
    A2aNegotiation, A2aSessionState, SettlementResult,
};
use crate::error::{IntentError, IntentResult};

// ── A2aNegotiator ──────────────────────────────────────────────────────

/// Maximum seen-nonce entries before eviction.
const MAX_SEEN_NONCES: usize = 1_000_000;

/// Maximum negotiation entries (active + terminal) before terminal eviction.
const MAX_NEGOTIATIONS: usize = 100_000;

/// Orchestrator that manages multiple A2A negotiations.
///
/// Thread-safe via interior `Mutex`.
pub struct A2aNegotiator {
    negotiations: Mutex<HashMap<Blake3Digest, A2aNegotiation>>,
    /// Seen message nonces — prevents replay of any previously
    /// accepted A2A message.
    seen_nonces: Mutex<HashSet<Blake3Digest>>,
}

impl A2aNegotiator {
    /// Create a new empty negotiator.
    pub fn new() -> Self {
        Self {
            negotiations: Mutex::new(HashMap::new()),
            seen_nonces: Mutex::new(HashSet::new()),
        }
    }

    /// Initiate a new negotiation between two agents.
    ///
    /// Returns the `negotiation_id`.
    pub fn initiate(
        &self,
        initiator: AccountAddress,
        responder: AccountAddress,
        nonce: Blake3Digest,
        deadline_ms: TimestampMs,
        now_ms: TimestampMs,
    ) -> IntentResult<Blake3Digest> {
        let negotiation_id = compute_negotiation_digest(&initiator, &responder, &nonce)?;
        let neg = A2aNegotiation::new(negotiation_id, initiator, responder, deadline_ms, now_ms);

        let mut store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        if store.contains_key(&negotiation_id) {
            return Err(IntentError::AgentSpecError {
                reason: "negotiation already exists".into(),
            });
        }
        store.insert(negotiation_id, neg);
        Ok(negotiation_id)
    }

    /// Process an incoming A2A message.
    ///
    /// Validates the message, applies it to the negotiation, and
    /// returns the new state.
    pub fn process_message(&self, msg: &A2aMessage, now_ms: u64) -> IntentResult<A2aSessionState> {
        // Acquire the nonce set for replay protection (used by validate_a2a_message).
        let mut nonces = self.seen_nonces.lock().unwrap_or_else(|e| e.into_inner());

        // Evict all nonces when at capacity to bound memory (M-001).
        if nonces.len() >= MAX_SEEN_NONCES {
            nonces.clear();
        }

        let mut store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        let neg =
            store
                .get_mut(&msg.negotiation_id)
                .ok_or_else(|| IntentError::AgentSpecError {
                    reason: format!("negotiation {:?} not found", msg.negotiation_id),
                })?;

        // Check timeout.
        if neg.is_timed_out(TimestampMs(now_ms)) {
            let _ = neg.transition_to(A2aSessionState::TimedOut);
            return Ok(A2aSessionState::TimedOut);
        }

        // Validate (includes nonce replay, digest, signature checks) + apply.
        validate_a2a_message(msg, neg, TimestampMs(now_ms), &mut nonces)?;
        apply_a2a_message(msg, neg)?;

        Ok(neg.state)
    }

    /// Get the current state of a negotiation.
    pub fn get_state(&self, negotiation_id: &Blake3Digest) -> Option<A2aSessionState> {
        let store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        store.get(negotiation_id).map(|n| n.state)
    }

    /// Get the settlement result for a completed negotiation.
    pub fn get_settlement(&self, negotiation_id: &Blake3Digest) -> IntentResult<SettlementResult> {
        let store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        let neg = store
            .get(negotiation_id)
            .ok_or_else(|| IntentError::AgentSpecError {
                reason: "negotiation not found".into(),
            })?;
        if neg.state != A2aSessionState::Settled {
            return Err(IntentError::AgentSpecError {
                reason: format!("negotiation not settled, state: {:?}", neg.state),
            });
        }
        // Extract plan hash from the locked negotiation.
        let plan_hash = neg.initiator_plan_hash.unwrap_or(Blake3Digest([0u8; 32]));
        Ok(SettlementResult {
            plan_hash,
            proof_ref: Blake3Digest([0u8; 32]), // filled by executor
            tx_hashes: vec![],                  // filled by executor
            settled_at_ms: TimestampMs(0),      // filled by executor
        })
    }

    /// Count active (non-terminal) negotiations.
    pub fn active_count(&self) -> usize {
        let store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        store.values().filter(|n| !n.state.is_terminal()).count()
    }

    /// Expire timed-out negotiations and remove terminal entries beyond capacity.
    pub fn expire_timed_out(&self, now_ms: TimestampMs) -> usize {
        let mut store = self.negotiations.lock().unwrap_or_else(|e| e.into_inner());
        let mut expired = 0;
        for neg in store.values_mut() {
            if !neg.state.is_terminal() && neg.is_timed_out(now_ms) {
                let _ = neg.transition_to(A2aSessionState::TimedOut);
                expired += 1;
            }
        }

        // Purge terminal negotiations when over capacity (M-002).
        if store.len() > MAX_NEGOTIATIONS {
            store.retain(|_, n| !n.state.is_terminal());
        }

        expired
    }
}

impl Default for A2aNegotiator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::a2a::{
        compute_message_digest,
        test_helpers::{gen_test_keypair, TestKeypair},
        A2aMessageKind,
    };

    fn make_msg(
        negotiation_id: Blake3Digest,
        kp: &TestKeypair,
        kind: A2aMessageKind,
        nonce: [u8; 32],
    ) -> A2aMessage {
        // Build a negotiation stub just for digest computation—the negotiation_id
        // is the real one from the negotiator.
        let nonce_d = Blake3Digest(nonce);
        let digest = compute_message_digest(&negotiation_id, &kp.address, &nonce_d, &kind).unwrap();
        use crate::agent_core::a2a::A2A_SIGNATURE_DOMAIN;
        use nexus_crypto::{DilithiumSigner, Signer};
        let sig = DilithiumSigner::sign(&kp.sk, A2A_SIGNATURE_DOMAIN, &digest.0);
        A2aMessage {
            negotiation_id,
            sender: kp.address,
            sender_pk: kp.pk.as_bytes().to_vec(),
            signature: sig.as_bytes().to_vec(),
            nonce: nonce_d,
            kind,
            expires_at_ms: TimestampMs(u64::MAX),
            message_digest: digest,
        }
    }

    #[test]
    fn full_lifecycle_propose_to_settle() {
        let negotiator = A2aNegotiator::new();
        let init_kp = gen_test_keypair();
        let resp_kp = gen_test_keypair();
        let nonce = Blake3Digest([0x01; 32]);
        let deadline = TimestampMs(u64::MAX);
        let now = TimestampMs(1_000);

        // 1. Initiate.
        let neg_id = negotiator
            .initiate(init_kp.address, resp_kp.address, nonce, deadline, now)
            .unwrap();
        assert_eq!(
            negotiator.get_state(&neg_id),
            Some(A2aSessionState::Proposed)
        );

        // 2. Counter.
        let msg = make_msg(
            neg_id,
            &resp_kp,
            A2aMessageKind::Counter {
                summary: "lower price".into(),
            },
            [0x02; 32],
        );
        let state = negotiator.process_message(&msg, 2_000).unwrap();
        assert_eq!(state, A2aSessionState::Countered);

        // 3. Accept.
        let msg = make_msg(neg_id, &init_kp, A2aMessageKind::Accept, [0x03; 32]);
        let state = negotiator.process_message(&msg, 3_000).unwrap();
        assert_eq!(state, A2aSessionState::Accepted);

        // 4. Lock plans.
        let msg = make_msg(
            neg_id,
            &init_kp,
            A2aMessageKind::LockPlans {
                initiator_plan_hash: Blake3Digest([0xCC; 32]),
                responder_plan_hash: Blake3Digest([0xDD; 32]),
            },
            [0x04; 32],
        );
        let state = negotiator.process_message(&msg, 4_000).unwrap();
        assert_eq!(state, A2aSessionState::Locked);

        // 5. Begin execution.
        let msg = make_msg(neg_id, &init_kp, A2aMessageKind::BeginExecution, [0x05; 32]);
        let state = negotiator.process_message(&msg, 5_000).unwrap();
        assert_eq!(state, A2aSessionState::Executing);

        // 6. Settle.
        let msg = make_msg(
            neg_id,
            &init_kp,
            A2aMessageKind::Settle {
                result: SettlementResult {
                    plan_hash: Blake3Digest([0xCC; 32]),
                    proof_ref: Blake3Digest([0xEE; 32]),
                    tx_hashes: vec![Blake3Digest([0xFF; 32])],
                    settled_at_ms: TimestampMs(6_000),
                },
            },
            [0x06; 32],
        );
        let state = negotiator.process_message(&msg, 6_000).unwrap();
        assert_eq!(state, A2aSessionState::Settled);

        // Verify settlement.
        let settlement = negotiator.get_settlement(&neg_id).unwrap();
        assert_eq!(settlement.plan_hash, Blake3Digest([0xCC; 32]));
    }

    #[test]
    fn reject_negotiation() {
        let negotiator = A2aNegotiator::new();
        let init_kp = gen_test_keypair();
        let resp_kp = gen_test_keypair();
        let neg_id = negotiator
            .initiate(
                init_kp.address,
                resp_kp.address,
                Blake3Digest([0x01; 32]),
                TimestampMs(u64::MAX),
                TimestampMs(1_000),
            )
            .unwrap();

        let msg = make_msg(
            neg_id,
            &resp_kp,
            A2aMessageKind::Reject {
                reason: "no deal".into(),
            },
            [0x02; 32],
        );
        let state = negotiator.process_message(&msg, 2_000).unwrap();
        assert_eq!(state, A2aSessionState::Rejected);
        assert_eq!(negotiator.active_count(), 0);
    }

    #[test]
    fn duplicate_initiation_rejected() {
        let negotiator = A2aNegotiator::new();
        let init_kp = gen_test_keypair();
        let resp_kp = gen_test_keypair();
        let nonce = Blake3Digest([0x01; 32]);
        negotiator
            .initiate(
                init_kp.address,
                resp_kp.address,
                nonce,
                TimestampMs(u64::MAX),
                TimestampMs(1_000),
            )
            .unwrap();
        let result = negotiator.initiate(
            init_kp.address,
            resp_kp.address,
            nonce,
            TimestampMs(u64::MAX),
            TimestampMs(1_000),
        );
        assert!(result.is_err());
    }

    #[test]
    fn timeout_detected_on_message() {
        let negotiator = A2aNegotiator::new();
        let init_kp = gen_test_keypair();
        let resp_kp = gen_test_keypair();
        let neg_id = negotiator
            .initiate(
                init_kp.address,
                resp_kp.address,
                Blake3Digest([0x01; 32]),
                TimestampMs(5_000), // deadline in 5s
                TimestampMs(1_000),
            )
            .unwrap();

        // Message arrives after deadline.
        let msg = make_msg(
            neg_id,
            &resp_kp,
            A2aMessageKind::Counter {
                summary: "late".into(),
            },
            [0x02; 32],
        );
        let state = negotiator.process_message(&msg, 10_000).unwrap();
        assert_eq!(state, A2aSessionState::TimedOut);
    }

    #[test]
    fn expire_timed_out_cleans_up() {
        let negotiator = A2aNegotiator::new();
        let kp1_init = gen_test_keypair();
        let kp1_resp = gen_test_keypair();
        let kp2_init = gen_test_keypair();
        let kp2_resp = gen_test_keypair();
        negotiator
            .initiate(
                kp1_init.address,
                kp1_resp.address,
                Blake3Digest([0x01; 32]),
                TimestampMs(5_000),
                TimestampMs(1_000),
            )
            .unwrap();
        negotiator
            .initiate(
                kp2_init.address,
                kp2_resp.address,
                Blake3Digest([0x02; 32]),
                TimestampMs(u64::MAX),
                TimestampMs(1_000),
            )
            .unwrap();

        assert_eq!(negotiator.active_count(), 2);
        let expired = negotiator.expire_timed_out(TimestampMs(10_000));
        assert_eq!(expired, 1);
        assert_eq!(negotiator.active_count(), 1);
    }

    #[test]
    fn get_settlement_not_settled_fails() {
        let negotiator = A2aNegotiator::new();
        let init_kp = gen_test_keypair();
        let resp_kp = gen_test_keypair();
        let neg_id = negotiator
            .initiate(
                init_kp.address,
                resp_kp.address,
                Blake3Digest([0x01; 32]),
                TimestampMs(u64::MAX),
                TimestampMs(1_000),
            )
            .unwrap();
        assert!(negotiator.get_settlement(&neg_id).is_err());
    }

    #[test]
    fn unknown_negotiation_rejected() {
        let negotiator = A2aNegotiator::new();
        let kp = gen_test_keypair();
        let msg = make_msg(
            Blake3Digest([0xFF; 32]),
            &kp,
            A2aMessageKind::Accept,
            [0x01; 32],
        );
        assert!(negotiator.process_message(&msg, 1_000).is_err());
    }
}
