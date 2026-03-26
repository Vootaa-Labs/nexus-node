//! Agent-to-Agent (A2A) canonical envelope and state machine.
//!
//! When one agent needs to collaborate with another (e.g. a swap
//! aggregator delegating to a liquidity provider), the inter-agent
//! negotiation follows the A2A sub-protocol defined in TLD-07 §6.
//!
//! # A2A Session State Machine
//!
//! ```text
//! Proposed → Countered ⇄ Countered → Accepted → Locked → Executing → Settled
//!                                                      → Failed
//! Any non-terminal → Rejected
//! Any non-terminal → TimedOut
//! ```
//!
//! Both agents share the same `negotiation_id`; side is tracked via
//! `initiator` / `responder` addressing.
//!
//! # A2A Message Types (T-10017)
//!
//! All inter-agent messages are wrapped in [`A2aMessage`] which
//! carries a signature, expiry, and replay protection nonce.
//! Messages map to state transitions:
//!
//! | Message Kind     | Transition              |
//! |------------------|-------------------------|
//! | `Propose`        | → Proposed              |
//! | `Counter`        | → Countered             |
//! | `Accept`         | → Accepted              |
//! | `Reject`         | → Rejected              |
//! | `LockPlans`      | Accepted → Locked       |
//! | `BeginExecution`  | Locked → Executing      |
//! | `Settle`         | Executing → Settled     |
//! | `Fail`           | Executing → Failed      |

use std::collections::HashSet;

use nexus_crypto::{DilithiumSignature, DilithiumSigner, DilithiumVerifyKey, Signer};
use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};
use serde::{Deserialize, Serialize};

/// Domain tag for A2A signature verification (SEC-M13).
pub const A2A_SIGNATURE_DOMAIN: &[u8] = b"nexus::a2a::signature::v1";

// ── A2aSessionState ─────────────────────────────────────────────────────

/// Lifecycle state of an A2A negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum A2aSessionState {
    /// Initial proposal submitted by the initiator.
    Proposed,
    /// Counter-proposal from the other side.
    Countered,
    /// Both sides accepted the terms.
    Accepted,
    /// Plan hashes locked (capabilities escrowed).
    Locked,
    /// Execution in progress.
    Executing,
    /// Successfully settled.
    Settled,
    /// Negotiation failed during execution.
    Failed,
    /// One side explicitly rejected.
    Rejected,
    /// Negotiation timed out.
    TimedOut,
}

impl A2aSessionState {
    /// Returns `true` for terminal states.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Settled | Self::Failed | Self::Rejected | Self::TimedOut,
        )
    }

    /// Validate that a transition from `self` to `target` is legal.
    pub fn can_transition_to(self, target: Self) -> bool {
        use A2aSessionState::*;
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, target),
            // Happy path
            (Proposed, Countered)
                | (Proposed, Accepted)
                | (Countered, Countered)
                | (Countered, Accepted)
                | (Accepted, Locked)
                | (Locked, Executing)
                | (Executing, Settled)
                // Error paths
                | (Executing, Failed)
                // Universal non-terminal → terminal
                | (Proposed, Rejected)
                | (Proposed, TimedOut)
                | (Countered, Rejected)
                | (Countered, TimedOut)
                | (Accepted, Rejected)
                | (Accepted, TimedOut)
                | (Locked, Rejected)
                | (Locked, TimedOut)
                | (Executing, TimedOut)
        )
    }
}

// ── A2aNegotiation ──────────────────────────────────────────────────────

/// Full state of an A2A negotiation session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aNegotiation {
    /// Unique negotiation identifier.
    pub negotiation_id: Blake3Digest,
    /// Agent that initiated the negotiation.
    pub initiator: AccountAddress,
    /// Agent that is responding.
    pub responder: AccountAddress,
    /// Current state.
    pub state: A2aSessionState,
    /// Plan hash proposed by the initiator (bound at Locked).
    pub initiator_plan_hash: Option<Blake3Digest>,
    /// Plan hash proposed by the responder (bound at Locked).
    pub responder_plan_hash: Option<Blake3Digest>,
    /// Number of counter-proposal rounds executed.
    pub counter_rounds: u32,
    /// Maximum allowed counter-proposal rounds.
    pub max_rounds: u32,
    /// Negotiation deadline.
    pub deadline_ms: TimestampMs,
    /// Timestamp of creation.
    pub created_at_ms: TimestampMs,
}

/// Default maximum counter-proposal rounds.
pub const DEFAULT_MAX_A2A_ROUNDS: u32 = 5;

impl A2aNegotiation {
    /// Create a new negotiation in `Proposed` state.
    pub fn new(
        negotiation_id: Blake3Digest,
        initiator: AccountAddress,
        responder: AccountAddress,
        deadline_ms: TimestampMs,
        now: TimestampMs,
    ) -> Self {
        Self {
            negotiation_id,
            initiator,
            responder,
            state: A2aSessionState::Proposed,
            initiator_plan_hash: None,
            responder_plan_hash: None,
            counter_rounds: 0,
            max_rounds: DEFAULT_MAX_A2A_ROUNDS,
            deadline_ms,
            created_at_ms: now,
        }
    }

    /// Check if the negotiation has timed out.
    pub fn is_timed_out(&self, now: TimestampMs) -> bool {
        now.0 > self.deadline_ms.0
    }

    /// Attempt to transition to `new_state`.
    ///
    /// # Errors
    ///
    /// Returns `AgentCapabilityDenied` if the transition is illegal.
    pub fn transition_to(&mut self, new_state: A2aSessionState) -> crate::error::IntentResult<()> {
        if !self.state.can_transition_to(new_state) {
            return Err(crate::error::IntentError::AgentCapabilityDenied {
                reason: format!("illegal A2A transition: {:?} → {:?}", self.state, new_state,),
            });
        }
        if new_state == A2aSessionState::Countered {
            self.counter_rounds += 1;
            if self.counter_rounds > self.max_rounds {
                self.state = A2aSessionState::TimedOut;
                return Err(crate::error::IntentError::AgentCapabilityDenied {
                    reason: format!(
                        "A2A counter rounds exceeded: {} > {}",
                        self.counter_rounds, self.max_rounds,
                    ),
                });
            }
        }
        self.state = new_state;
        Ok(())
    }

    /// Lock both plan hashes for execution.
    ///
    /// Must be called in `Accepted` state. Transitions to `Locked`.
    pub fn lock_plans(
        &mut self,
        initiator_plan: Blake3Digest,
        responder_plan: Blake3Digest,
    ) -> crate::error::IntentResult<()> {
        if self.state != A2aSessionState::Accepted {
            return Err(crate::error::IntentError::AgentCapabilityDenied {
                reason: format!(
                    "cannot lock plans in state {:?}, expected Accepted",
                    self.state,
                ),
            });
        }
        self.initiator_plan_hash = Some(initiator_plan);
        self.responder_plan_hash = Some(responder_plan);
        self.state = A2aSessionState::Locked;
        Ok(())
    }
}

// ── Domain tag ──────────────────────────────────────────────────────────

/// Domain tag for A2A negotiation digest.
pub const A2A_DOMAIN: &[u8] = b"nexus::agent_core::a2a::v1";

/// Compute the canonical digest for an A2A negotiation.
pub fn compute_negotiation_digest(
    initiator: &AccountAddress,
    responder: &AccountAddress,
    nonce: &Blake3Digest,
) -> crate::error::IntentResult<Blake3Digest> {
    let init_bytes =
        bcs::to_bytes(initiator).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let resp_bytes =
        bcs::to_bytes(responder).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let nonce_bytes =
        bcs::to_bytes(nonce).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(A2A_DOMAIN);
    hasher.update(&init_bytes);
    hasher.update(&resp_bytes);
    hasher.update(&nonce_bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

// ── A2A message types (T-10017) ─────────────────────────────────────────

/// Domain tag for A2A message digest.
pub const A2A_MESSAGE_DOMAIN: &[u8] = b"nexus::agent_core::a2a_msg::v1";

/// Discriminated A2A message kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum A2aMessageKind {
    /// Initial proposal with terms.
    Propose {
        /// Human-readable summary of the proposal.
        summary: String,
    },
    /// Counter-proposal with modified terms.
    Counter {
        /// Human-readable summary of the counter-proposal.
        summary: String,
    },
    /// Accept the current terms.
    Accept,
    /// Reject the negotiation.
    Reject {
        /// Reason for rejection.
        reason: String,
    },
    /// Lock both sides' plan hashes for execution.
    LockPlans {
        /// Initiator's plan hash.
        initiator_plan_hash: Blake3Digest,
        /// Responder's plan hash.
        responder_plan_hash: Blake3Digest,
    },
    /// Begin execution of locked plans.
    BeginExecution,
    /// Report successful settlement.
    Settle {
        /// Settlement result with proof.
        result: SettlementResult,
    },
    /// Report execution failure.
    Fail {
        /// Reason for failure.
        reason: String,
    },
}

/// Result of a successfully settled A2A negotiation.
///
/// TLD-07 §6 invariant: `execution_result` must carry `plan_hash` and
/// `proof_ref` — this struct enforces that at the type level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementResult {
    /// Plan hash that was executed.
    pub plan_hash: Blake3Digest,
    /// On-chain proof / receipt reference.
    pub proof_ref: Blake3Digest,
    /// Transaction hashes produced.
    pub tx_hashes: Vec<Blake3Digest>,
    /// Timestamp of settlement.
    pub settled_at_ms: TimestampMs,
}

/// Signed A2A message envelope (SEC-M13).
///
/// Every A2A message is wrapped in this envelope to provide:
/// - Sender authentication via ML-DSA-65 signature over the message digest.
/// - Public-key → sender address binding.
/// - Replay protection (nonce uniqueness + expiry).
/// - Linkage to the negotiation (negotiation_id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aMessage {
    /// Negotiation this message belongs to.
    pub negotiation_id: Blake3Digest,
    /// Sender of the message (must be initiator or responder).
    pub sender: AccountAddress,
    /// ML-DSA-65 public key bytes of the sender.
    pub sender_pk: Vec<u8>,
    /// ML-DSA-65 signature over the `message_digest` (SEC-M13).
    pub signature: Vec<u8>,
    /// Unique per-message nonce for replay protection.
    pub nonce: Blake3Digest,
    /// Message payload.
    pub kind: A2aMessageKind,
    /// Deadline for this message (ms since epoch).
    pub expires_at_ms: TimestampMs,
    /// BLAKE3 digest of the message content (for signature verification).
    pub message_digest: Blake3Digest,
}

/// Compute the canonical digest for an A2A message.
///
/// `BLAKE3(A2A_MESSAGE_DOMAIN ‖ BCS(negotiation_id, sender, nonce, kind))`
pub fn compute_message_digest(
    negotiation_id: &Blake3Digest,
    sender: &AccountAddress,
    nonce: &Blake3Digest,
    kind: &A2aMessageKind,
) -> crate::error::IntentResult<Blake3Digest> {
    let neg_bytes = bcs::to_bytes(negotiation_id)
        .map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let sender_bytes =
        bcs::to_bytes(sender).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let nonce_bytes =
        bcs::to_bytes(nonce).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let kind_bytes =
        bcs::to_bytes(kind).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(A2A_MESSAGE_DOMAIN);
    hasher.update(&neg_bytes);
    hasher.update(&sender_bytes);
    hasher.update(&nonce_bytes);
    hasher.update(&kind_bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(Blake3Digest(hash))
}

// ── A2A message validation ──────────────────────────────────────────────

/// Validate an A2A message against a negotiation.
///
/// Checks:
/// 1. Sender must be initiator or responder of the negotiation.
/// 2. Message must not be expired.
/// 3. Nonce must not have been seen before (replay protection).
/// 4. Message digest must match recomputation.
/// 5. Sender public key must bind to the claimed sender address.
/// 6. ML-DSA-65 signature over the digest must verify (SEC-M13).
/// 7. The message kind must correspond to a legal state transition.
pub fn validate_a2a_message(
    msg: &A2aMessage,
    negotiation: &A2aNegotiation,
    now: TimestampMs,
    seen_nonces: &mut HashSet<Blake3Digest>,
) -> crate::error::IntentResult<()> {
    // 1. Sender must be a participant
    if msg.sender != negotiation.initiator && msg.sender != negotiation.responder {
        return Err(crate::error::IntentError::AgentCapabilityDenied {
            reason: format!(
                "sender {:?} is not a participant in negotiation",
                msg.sender,
            ),
        });
    }

    // 2. Message must not be expired
    if now.0 > msg.expires_at_ms.0 {
        return Err(crate::error::IntentError::IntentExpired {
            deadline_ms: msg.expires_at_ms.0,
            current_ms: now.0,
        });
    }

    // 3. Nonce replay protection
    if !seen_nonces.insert(msg.nonce) {
        return Err(crate::error::IntentError::AgentCapabilityDenied {
            reason: "A2A message nonce already seen (replay detected)".into(),
        });
    }

    // 4. Digest integrity
    let expected = compute_message_digest(&msg.negotiation_id, &msg.sender, &msg.nonce, &msg.kind)?;
    if expected != msg.message_digest {
        return Err(crate::error::IntentError::AgentCapabilityDenied {
            reason: "A2A message digest mismatch".into(),
        });
    }

    // 5. Public key → address binding
    let derived_sender = AccountAddress::from_dilithium_pubkey(&msg.sender_pk);
    if derived_sender != msg.sender {
        return Err(crate::error::IntentError::AgentCapabilityDenied {
            reason: "sender_pk does not bind to claimed sender address".into(),
        });
    }

    // 6. Signature verification (SEC-M13)
    let vk = DilithiumVerifyKey::from_bytes(&msg.sender_pk).map_err(|e| {
        crate::error::IntentError::AgentCapabilityDenied {
            reason: format!("invalid sender_pk: {e}"),
        }
    })?;
    let sig = DilithiumSignature::from_bytes(&msg.signature).map_err(|e| {
        crate::error::IntentError::AgentCapabilityDenied {
            reason: format!("invalid signature: {e}"),
        }
    })?;
    DilithiumSigner::verify(&vk, A2A_SIGNATURE_DOMAIN, &msg.message_digest.0, &sig).map_err(
        |_| crate::error::IntentError::AgentCapabilityDenied {
            reason: "A2A message signature verification failed".into(),
        },
    )?;

    // 7. Message kind must map to a legal transition
    let target_state = message_kind_to_state(&msg.kind);
    if !negotiation.state.can_transition_to(target_state) {
        return Err(crate::error::IntentError::AgentCapabilityDenied {
            reason: format!(
                "message kind {:?} cannot transition from state {:?}",
                std::mem::discriminant(&msg.kind),
                negotiation.state,
            ),
        });
    }

    Ok(())
}

/// Map an A2A message kind to the target negotiation state.
fn message_kind_to_state(kind: &A2aMessageKind) -> A2aSessionState {
    match kind {
        A2aMessageKind::Propose { .. } => A2aSessionState::Proposed,
        A2aMessageKind::Counter { .. } => A2aSessionState::Countered,
        A2aMessageKind::Accept => A2aSessionState::Accepted,
        A2aMessageKind::Reject { .. } => A2aSessionState::Rejected,
        A2aMessageKind::LockPlans { .. } => A2aSessionState::Locked,
        A2aMessageKind::BeginExecution => A2aSessionState::Executing,
        A2aMessageKind::Settle { .. } => A2aSessionState::Settled,
        A2aMessageKind::Fail { .. } => A2aSessionState::Failed,
    }
}

/// Apply a validated message to a negotiation, advancing state.
///
/// The message must have been validated by [`validate_a2a_message`] first.
pub fn apply_a2a_message(
    msg: &A2aMessage,
    negotiation: &mut A2aNegotiation,
) -> crate::error::IntentResult<()> {
    match &msg.kind {
        A2aMessageKind::LockPlans {
            initiator_plan_hash,
            responder_plan_hash,
        } => {
            negotiation.lock_plans(*initiator_plan_hash, *responder_plan_hash)?;
        }
        _ => {
            let target = message_kind_to_state(&msg.kind);
            negotiation.transition_to(target)?;
        }
    }
    Ok(())
}

/// Test helpers for building signed A2A messages.
///
/// Available only under `#[cfg(test)]` — used by both `a2a` and
/// `a2a_negotiator` test modules.
#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};

    /// Test keypair bundle.
    pub struct TestKeypair {
        pub sk: DilithiumSigningKey,
        pub pk: DilithiumVerifyKey,
        pub address: AccountAddress,
    }

    /// Generate a fresh ML-DSA-65 keypair with its derived address.
    pub fn gen_test_keypair() -> TestKeypair {
        let (sk, pk) = DilithiumSigner::generate_keypair();
        let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        TestKeypair { sk, pk, address }
    }

    /// Build a negotiation whose initiator/responder are key-derived
    /// addresses.
    pub fn make_signed_negotiation(init: &TestKeypair, resp: &TestKeypair) -> A2aNegotiation {
        A2aNegotiation::new(
            Blake3Digest([0x01; 32]),
            init.address,
            resp.address,
            TimestampMs(2_000_000_000_000),
            TimestampMs(1_000_000_000_000),
        )
    }

    /// Build a fully signed A2A message suitable for
    /// `validate_a2a_message`.
    pub fn make_signed_message(
        neg: &A2aNegotiation,
        kp: &TestKeypair,
        kind: A2aMessageKind,
        nonce: Blake3Digest,
    ) -> A2aMessage {
        let digest =
            compute_message_digest(&neg.negotiation_id, &kp.address, &nonce, &kind).unwrap();
        let sig = DilithiumSigner::sign(&kp.sk, A2A_SIGNATURE_DOMAIN, &digest.0);
        A2aMessage {
            negotiation_id: neg.negotiation_id,
            sender: kp.address,
            sender_pk: kp.pk.as_bytes().to_vec(),
            signature: sig.as_bytes().to_vec(),
            nonce,
            kind,
            expires_at_ms: TimestampMs(3_000_000_000_000),
            message_digest: digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{AccountAddress, Blake3Digest, TimestampMs};

    fn make_negotiation() -> A2aNegotiation {
        A2aNegotiation::new(
            Blake3Digest([0x01; 32]),
            AccountAddress([0xAA; 32]),
            AccountAddress([0xBB; 32]),
            TimestampMs(2_000_000_000_000),
            TimestampMs(1_000_000_000_000),
        )
    }

    #[test]
    fn happy_path_full_lifecycle() {
        let mut neg = make_negotiation();
        assert_eq!(neg.state, A2aSessionState::Proposed);

        neg.transition_to(A2aSessionState::Countered).unwrap();
        assert_eq!(neg.state, A2aSessionState::Countered);
        assert_eq!(neg.counter_rounds, 1);

        neg.transition_to(A2aSessionState::Accepted).unwrap();
        assert_eq!(neg.state, A2aSessionState::Accepted);

        neg.lock_plans(Blake3Digest([0x10; 32]), Blake3Digest([0x20; 32]))
            .unwrap();
        assert_eq!(neg.state, A2aSessionState::Locked);
        assert!(neg.initiator_plan_hash.is_some());
        assert!(neg.responder_plan_hash.is_some());

        neg.transition_to(A2aSessionState::Executing).unwrap();
        neg.transition_to(A2aSessionState::Settled).unwrap();
        assert!(neg.state.is_terminal());
    }

    #[test]
    fn reject_from_proposed() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Rejected).unwrap();
        assert!(neg.state.is_terminal());
    }

    #[test]
    fn cannot_transition_from_terminal() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Rejected).unwrap();
        assert!(neg.transition_to(A2aSessionState::Proposed).is_err());
    }

    #[test]
    fn counter_rounds_tracked() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Countered).unwrap();
        assert_eq!(neg.counter_rounds, 1);
        neg.transition_to(A2aSessionState::Countered).unwrap();
        assert_eq!(neg.counter_rounds, 2);
    }

    #[test]
    fn counter_rounds_exceeded() {
        let mut neg = make_negotiation();
        neg.max_rounds = 2;
        neg.transition_to(A2aSessionState::Countered).unwrap();
        neg.transition_to(A2aSessionState::Countered).unwrap();
        let err = neg.transition_to(A2aSessionState::Countered);
        assert!(err.is_err());
        assert_eq!(neg.state, A2aSessionState::TimedOut);
    }

    #[test]
    fn lock_plans_requires_accepted() {
        let mut neg = make_negotiation();
        let r = neg.lock_plans(Blake3Digest([0x10; 32]), Blake3Digest([0x20; 32]));
        assert!(r.is_err());
    }

    #[test]
    fn timed_out_check() {
        let neg = make_negotiation();
        assert!(!neg.is_timed_out(TimestampMs(1_500_000_000_000)));
        assert!(neg.is_timed_out(TimestampMs(3_000_000_000_000)));
    }

    #[test]
    fn negotiation_bcs_round_trip() {
        let neg = make_negotiation();
        let bytes = bcs::to_bytes(&neg).unwrap();
        let decoded: A2aNegotiation = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(neg, decoded);
    }

    #[test]
    fn negotiation_digest_deterministic() {
        let init = AccountAddress([0xAA; 32]);
        let resp = AccountAddress([0xBB; 32]);
        let nonce = Blake3Digest([0x01; 32]);
        let d1 = compute_negotiation_digest(&init, &resp, &nonce).unwrap();
        let d2 = compute_negotiation_digest(&init, &resp, &nonce).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn negotiation_digest_varies() {
        let init = AccountAddress([0xAA; 32]);
        let resp1 = AccountAddress([0xBB; 32]);
        let resp2 = AccountAddress([0xCC; 32]);
        let nonce = Blake3Digest([0x01; 32]);
        let d1 = compute_negotiation_digest(&init, &resp1, &nonce).unwrap();
        let d2 = compute_negotiation_digest(&init, &resp2, &nonce).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn executing_can_fail() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Accepted).unwrap();
        neg.lock_plans(Blake3Digest([0x10; 32]), Blake3Digest([0x20; 32]))
            .unwrap();
        neg.transition_to(A2aSessionState::Executing).unwrap();
        neg.transition_to(A2aSessionState::Failed).unwrap();
        assert!(neg.state.is_terminal());
    }

    // ── A2A message tests (T-10017) ─────────────────────────────────

    /// Build a message with dummy (non-verifiable) signature fields.
    /// Suitable for digest, BCS, and state-machine tests that don't
    /// call `validate_a2a_message`.
    fn make_message(
        neg: &A2aNegotiation,
        sender: AccountAddress,
        kind: A2aMessageKind,
    ) -> A2aMessage {
        let nonce = Blake3Digest([0x99; 32]);
        let digest = compute_message_digest(&neg.negotiation_id, &sender, &nonce, &kind).unwrap();
        A2aMessage {
            negotiation_id: neg.negotiation_id,
            sender,
            sender_pk: vec![],
            signature: vec![],
            nonce,
            kind,
            expires_at_ms: TimestampMs(3_000_000_000_000),
            message_digest: digest,
        }
    }

    // Re-use the crate-level test helpers for signed-message tests.
    use super::test_helpers::{gen_test_keypair, make_signed_message, make_signed_negotiation};

    #[test]
    fn message_digest_deterministic() {
        let neg_id = Blake3Digest([0x01; 32]);
        let sender = AccountAddress([0xAA; 32]);
        let nonce = Blake3Digest([0x02; 32]);
        let kind = A2aMessageKind::Accept;
        let d1 = compute_message_digest(&neg_id, &sender, &nonce, &kind).unwrap();
        let d2 = compute_message_digest(&neg_id, &sender, &nonce, &kind).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn message_digest_varies_with_kind() {
        let neg_id = Blake3Digest([0x01; 32]);
        let sender = AccountAddress([0xAA; 32]);
        let nonce = Blake3Digest([0x02; 32]);
        let d1 = compute_message_digest(&neg_id, &sender, &nonce, &A2aMessageKind::Accept).unwrap();
        let d2 = compute_message_digest(
            &neg_id,
            &sender,
            &nonce,
            &A2aMessageKind::Reject {
                reason: "no".into(),
            },
        )
        .unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn validate_message_valid_accept() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let mut neg = make_signed_negotiation(&init, &resp);
        neg.transition_to(A2aSessionState::Countered).unwrap();
        let msg = make_signed_message(
            &neg,
            &resp,
            A2aMessageKind::Accept,
            Blake3Digest([0x99; 32]),
        );
        let mut seen = HashSet::new();
        assert!(
            validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen).is_ok()
        );
    }

    #[test]
    fn validate_message_wrong_sender() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let outsider = gen_test_keypair();
        let neg = make_signed_negotiation(&init, &resp);
        let msg = make_signed_message(
            &neg,
            &outsider,
            A2aMessageKind::Accept,
            Blake3Digest([0x99; 32]),
        );
        let mut seen = HashSet::new();
        assert!(
            validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen).is_err()
        );
    }

    #[test]
    fn validate_message_expired() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let neg = make_signed_negotiation(&init, &resp);
        let mut msg = make_signed_message(
            &neg,
            &init,
            A2aMessageKind::Reject {
                reason: "test".into(),
            },
            Blake3Digest([0x99; 32]),
        );
        msg.expires_at_ms = TimestampMs(100); // already expired
        let mut seen = HashSet::new();
        assert!(validate_a2a_message(&msg, &neg, TimestampMs(200), &mut seen).is_err());
    }

    #[test]
    fn validate_message_tampered_digest() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let neg = make_signed_negotiation(&init, &resp);
        let mut msg = make_signed_message(
            &neg,
            &init,
            A2aMessageKind::Reject {
                reason: "test".into(),
            },
            Blake3Digest([0x99; 32]),
        );
        msg.message_digest = Blake3Digest([0xFF; 32]); // tampered
        let mut seen = HashSet::new();
        assert!(
            validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen).is_err()
        );
    }

    #[test]
    fn validate_message_illegal_transition() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let neg = make_signed_negotiation(&init, &resp); // state = Proposed
        let msg = make_signed_message(
            &neg,
            &init,
            A2aMessageKind::LockPlans {
                initiator_plan_hash: Blake3Digest([0x10; 32]),
                responder_plan_hash: Blake3Digest([0x20; 32]),
            },
            Blake3Digest([0x99; 32]),
        );
        let mut seen = HashSet::new();
        assert!(
            validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen).is_err()
        );
    }

    #[test]
    fn apply_message_counter() {
        let mut neg = make_negotiation();
        let msg = make_message(
            &neg,
            neg.responder,
            A2aMessageKind::Counter {
                summary: "lower price".into(),
            },
        );
        apply_a2a_message(&msg, &mut neg).unwrap();
        assert_eq!(neg.state, A2aSessionState::Countered);
        assert_eq!(neg.counter_rounds, 1);
    }

    #[test]
    fn apply_message_lock_plans() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Accepted).unwrap();
        let msg = make_message(
            &neg,
            neg.initiator,
            A2aMessageKind::LockPlans {
                initiator_plan_hash: Blake3Digest([0x10; 32]),
                responder_plan_hash: Blake3Digest([0x20; 32]),
            },
        );
        apply_a2a_message(&msg, &mut neg).unwrap();
        assert_eq!(neg.state, A2aSessionState::Locked);
        assert!(neg.initiator_plan_hash.is_some());
        assert!(neg.responder_plan_hash.is_some());
    }

    #[test]
    fn apply_message_settle() {
        let mut neg = make_negotiation();
        neg.transition_to(A2aSessionState::Accepted).unwrap();
        neg.lock_plans(Blake3Digest([0x10; 32]), Blake3Digest([0x20; 32]))
            .unwrap();
        neg.transition_to(A2aSessionState::Executing).unwrap();
        let msg = make_message(
            &neg,
            neg.initiator,
            A2aMessageKind::Settle {
                result: SettlementResult {
                    plan_hash: Blake3Digest([0x10; 32]),
                    proof_ref: Blake3Digest([0x30; 32]),
                    tx_hashes: vec![Blake3Digest([0x40; 32])],
                    settled_at_ms: TimestampMs(1_600_000_000_000),
                },
            },
        );
        apply_a2a_message(&msg, &mut neg).unwrap();
        assert_eq!(neg.state, A2aSessionState::Settled);
    }

    #[test]
    fn settlement_result_bcs_round_trip() {
        let result = SettlementResult {
            plan_hash: Blake3Digest([0x01; 32]),
            proof_ref: Blake3Digest([0x02; 32]),
            tx_hashes: vec![Blake3Digest([0x03; 32])],
            settled_at_ms: TimestampMs(1_700_000_000_000),
        };
        let bytes = bcs::to_bytes(&result).unwrap();
        let decoded: SettlementResult = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(result, decoded);
    }

    #[test]
    fn a2a_message_bcs_round_trip() {
        let neg = make_negotiation();
        let msg = make_message(&neg, neg.initiator, A2aMessageKind::Accept);
        let bytes = bcs::to_bytes(&msg).unwrap();
        let decoded: A2aMessage = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn message_kind_to_state_coverage() {
        // Ensure every message kind maps to a state
        let kinds: Vec<A2aMessageKind> = vec![
            A2aMessageKind::Propose {
                summary: "s".into(),
            },
            A2aMessageKind::Counter {
                summary: "c".into(),
            },
            A2aMessageKind::Accept,
            A2aMessageKind::Reject { reason: "r".into() },
            A2aMessageKind::LockPlans {
                initiator_plan_hash: Blake3Digest([0; 32]),
                responder_plan_hash: Blake3Digest([0; 32]),
            },
            A2aMessageKind::BeginExecution,
            A2aMessageKind::Settle {
                result: SettlementResult {
                    plan_hash: Blake3Digest([0; 32]),
                    proof_ref: Blake3Digest([0; 32]),
                    tx_hashes: vec![],
                    settled_at_ms: TimestampMs(0),
                },
            },
            A2aMessageKind::Fail { reason: "f".into() },
        ];
        for kind in &kinds {
            let state = message_kind_to_state(kind);
            // Just ensure it doesn't panic and returns a valid state
            let _ = state.is_terminal();
        }
    }

    #[test]
    fn a2a_message_with_invalid_signature_should_be_rejected() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let mut neg = make_signed_negotiation(&init, &resp);
        neg.transition_to(A2aSessionState::Countered).unwrap();
        let mut msg = make_signed_message(
            &neg,
            &resp,
            A2aMessageKind::Accept,
            Blake3Digest([0x99; 32]),
        );
        // Tamper one byte of the signature
        msg.signature[0] ^= 0xFF;
        let mut seen = HashSet::new();
        let result = validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen);
        assert!(result.is_err(), "tampered signature must be rejected");
    }

    #[test]
    fn a2a_message_nonce_replay_should_be_rejected() {
        let init = gen_test_keypair();
        let resp = gen_test_keypair();
        let mut neg = make_signed_negotiation(&init, &resp);
        neg.transition_to(A2aSessionState::Countered).unwrap();
        let msg = make_signed_message(
            &neg,
            &resp,
            A2aMessageKind::Accept,
            Blake3Digest([0x99; 32]),
        );
        let mut seen = HashSet::new();
        assert!(
            validate_a2a_message(&msg, &neg, TimestampMs(1_500_000_000_000), &mut seen).is_ok()
        );
        // Same nonce must be rejected on replay
        neg.transition_to(A2aSessionState::Accepted).unwrap();
        neg.transition_to(A2aSessionState::Locked).unwrap();
        let msg2 = A2aMessage { ..msg.clone() };
        let result = validate_a2a_message(&msg2, &neg, TimestampMs(1_500_000_000_000), &mut seen);
        assert!(result.is_err(), "replayed nonce must be rejected");
    }
}
