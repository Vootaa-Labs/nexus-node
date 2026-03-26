// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Proptest-based formal verification — Agent Core Layer
//!
//! Strengthens the deterministic unit tests with randomised property-based
//! testing via the `proptest` framework.
//!
//! # Invariants Covered
//! - FV-AC-001: Delegation chain monotonic contraction
//! - FV-AC-002: A2A state machine — only legal transitions
//! - FV-AC-003: A2A replay protection (nonce deduplication)
//! - FV-AC-004: Provenance store bounded memory
//! - FV-AC-005: Snapshot hash integrity
//! - FV-AC-006: Revocation cascade completeness

use nexus_intent::agent_core::a2a::A2aSessionState;
use nexus_intent::agent_core::capability_snapshot::{
    compute_snapshot_hash, revocation_cascade, scope_is_subset,
    validate_capability_against_snapshot, validate_delegation_chain, AgentCapabilitySnapshot,
    CapabilityScope, DelegationLink, DelegationToken,
};
use nexus_intent::agent_core::provenance::{ProvenanceRecord, ProvenanceStatus};
use nexus_intent::agent_core::provenance_store::ProvenanceStore;
use nexus_primitives::{AccountAddress, Amount, Blake3Digest, TimestampMs, TokenId};
use proptest::prelude::*;

// ── Strategies ──────────────────────────────────────────────────────────

fn arb_address() -> impl Strategy<Value = AccountAddress> {
    prop::array::uniform32(any::<u8>()).prop_map(AccountAddress)
}

fn arb_digest() -> impl Strategy<Value = Blake3Digest> {
    prop::array::uniform32(any::<u8>()).prop_map(Blake3Digest)
}

fn arb_scope() -> impl Strategy<Value = CapabilityScope> {
    prop_oneof![
        Just(CapabilityScope::Full),
        Just(CapabilityScope::ReadOnly),
        prop::collection::vec("[a-z]{3,8}", 1..=4).prop_map(CapabilityScope::IntentTypes),
    ]
}

/// Generate a valid monotonically contracting delegation chain of length `len`.
fn arb_valid_chain(len: usize) -> impl Strategy<Value = Vec<DelegationLink>> {
    // Start with high values, each step decreases.
    let max_val = 1_000_000u64;
    let max_deadline = 10_000_000u64;

    prop::collection::vec(arb_address(), len + 1).prop_flat_map(move |addrs| {
        // Generate decreasing values and deadlines.
        prop::collection::vec((0u64..max_val, 0u64..max_deadline), len).prop_map(move |pairs| {
            let mut sorted_vals: Vec<u64> = pairs.iter().map(|(v, _)| *v).collect();
            sorted_vals.sort_unstable_by(|a, b| b.cmp(a));
            let mut sorted_deadlines: Vec<u64> = pairs.iter().map(|(_, d)| *d).collect();
            sorted_deadlines.sort_unstable_by(|a, b| b.cmp(a));

            (0..len)
                .map(|i| DelegationLink {
                    delegator: addrs[i],
                    delegatee: addrs[i + 1],
                    max_value: Amount(sorted_vals[i]),
                    deadline: TimestampMs(sorted_deadlines[i]),
                    allowed_contracts: vec![],
                    scope: CapabilityScope::Full,
                })
                .collect::<Vec<_>>()
        })
    })
}

// ── FV-AC-001: Delegation chain monotonic contraction ───────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Any randomly generated chain with monotonically decreasing
    /// max_value and deadline must pass validation.
    #[test]
    fn fv_ac_001_valid_chain_passes(chain in arb_valid_chain(5)) {
        prop_assert!(validate_delegation_chain(&chain).is_ok());
    }

    /// If we flip max_value so child > parent at any position, the chain
    /// must be rejected (value escalation).
    #[test]
    fn fv_ac_001_value_escalation_rejected(
        chain in arb_valid_chain(3),
        bump in 1u64..1_000_000,
    ) {
        if chain.len() >= 2 {
            let mut bad = chain;
            // Make last link's max_value > its parent's.
            let parent_val = bad[bad.len() - 2].max_value.0;
            let last = bad.len() - 1;
            bad[last].max_value = Amount(parent_val.saturating_add(bump));
            prop_assert!(validate_delegation_chain(&bad).is_err());
        }
    }

    /// If we flip deadline so child > parent, the chain must be rejected.
    #[test]
    fn fv_ac_001_deadline_escalation_rejected(
        chain in arb_valid_chain(3),
        bump in 1u64..1_000_000,
    ) {
        if chain.len() >= 2 {
            let mut bad = chain;
            let parent_dl = bad[bad.len() - 2].deadline.0;
            let last = bad.len() - 1;
            bad[last].deadline = TimestampMs(parent_dl.saturating_add(bump));
            prop_assert!(validate_delegation_chain(&bad).is_err());
        }
    }

    /// Broken chain: mismatch delegator ≠ parent.delegatee → rejected.
    #[test]
    fn fv_ac_001_broken_chain_rejected(
        chain in arb_valid_chain(3),
        random_addr in arb_address(),
    ) {
        if chain.len() >= 2 {
            let mut bad = chain;
            // Break the chain link.
            bad[1].delegator = random_addr;
            // Only expect error if the random address doesn't accidentally match.
            if bad[1].delegator != bad[0].delegatee {
                prop_assert!(validate_delegation_chain(&bad).is_err());
            }
        }
    }
}

// ── FV-AC-002: A2A state machine — only legal transitions ──────────────

/// All 10 A2A states.
const ALL_STATES: [A2aSessionState; 10] = [
    A2aSessionState::Proposed,
    A2aSessionState::Countered,
    A2aSessionState::Accepted,
    A2aSessionState::Locked,
    A2aSessionState::Executing,
    A2aSessionState::Settled,
    A2aSessionState::Failed,
    A2aSessionState::Rejected,
    A2aSessionState::TimedOut,
    // One more try with Proposed again to cover all pairs
    A2aSessionState::Proposed,
];

/// Legal transitions as defined by `can_transition_to`.
fn is_legal(from: A2aSessionState, to: A2aSessionState) -> bool {
    use A2aSessionState::*;
    if from.is_terminal() {
        return false;
    }
    matches!(
        (from, to),
        (Proposed, Countered)
            | (Proposed, Accepted)
            | (Countered, Countered)
            | (Countered, Accepted)
            | (Accepted, Locked)
            | (Locked, Executing)
            | (Executing, Settled)
            | (Executing, Failed)
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// For any pair of states (from, to), `can_transition_to` must agree
    /// with our reference oracle `is_legal`.
    #[test]
    fn fv_ac_002_transition_oracle(
        from_idx in 0usize..9,
        to_idx in 0usize..9,
    ) {
        let from = ALL_STATES[from_idx];
        let to = ALL_STATES[to_idx];
        prop_assert_eq!(
            from.can_transition_to(to),
            is_legal(from, to),
            "mismatch: {:?} → {:?}",
            from,
            to,
        );
    }

    /// Terminal states cannot transition to anything.
    #[test]
    fn fv_ac_002_terminal_no_transition(to_idx in 0usize..9) {
        let terminals = [
            A2aSessionState::Settled,
            A2aSessionState::Failed,
            A2aSessionState::Rejected,
            A2aSessionState::TimedOut,
        ];
        for &terminal in &terminals {
            let to = ALL_STATES[to_idx];
            prop_assert!(!terminal.can_transition_to(to));
        }
    }
}

// ── FV-AC-003: A2A replay protection ────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Two negotiations with the same (initiator, responder) but different
    /// nonces must produce different negotiation_ids (implicit replay
    /// protection — duplicate nonces = same negotiation, not a new one).
    #[test]
    fn fv_ac_003_different_nonces_different_ids(
        nonce1 in arb_digest(),
        nonce2 in arb_digest(),
    ) {
        use nexus_intent::agent_core::a2a::compute_negotiation_digest;

        let init = AccountAddress([0xAA; 32]);
        let resp = AccountAddress([0xBB; 32]);
        let id1 = compute_negotiation_digest(&init, &resp, &nonce1).unwrap();
        let id2 = compute_negotiation_digest(&init, &resp, &nonce2).unwrap();

        if nonce1 != nonce2 {
            prop_assert_ne!(id1, id2);
        } else {
            prop_assert_eq!(id1, id2);
        }
    }
}

// ── FV-AC-004: Provenance store bounded memory ──────────────────────────

fn make_prov_record(idx: u8, agent: u8, session: u8, time: u64) -> ProvenanceRecord {
    ProvenanceRecord {
        provenance_id: Blake3Digest([idx; 32]),
        session_id: Blake3Digest([session; 32]),
        request_id: Blake3Digest([idx; 32]),
        agent_id: AccountAddress([agent; 32]),
        parent_agent_id: None,
        capability_token_id: Some(TokenId::Native),
        intent_hash: Blake3Digest([idx; 32]),
        plan_hash: Blake3Digest([idx; 32]),
        confirmation_ref: None,
        tx_hash: None,
        status: ProvenanceStatus::Pending,
        created_at_ms: TimestampMs(time),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// For any cap ∈ [1, 200] and any insertion count ∈ [cap, cap+100],
    /// the store never exceeds the cap.
    #[test]
    fn fv_ac_004_bounded_memory(
        cap in 1usize..=200,
        extra in 0usize..=100,
    ) {
        let store = ProvenanceStore::with_max_records(cap);
        let total = cap + extra;
        for i in 0..total {
            store.record(make_prov_record(i as u8, (i % 4) as u8, (i % 3) as u8, i as u64));
        }
        prop_assert!(store.len() <= cap, "store.len()={} > cap={}", store.len(), cap);
    }
}

// ── FV-AC-005: Snapshot hash integrity ──────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Correct hash → validation passes. Tampered hash → validation fails.
    #[test]
    fn fv_ac_005_snapshot_integrity(
        agent_byte in any::<u8>(),
        max_value in 0u64..1_000_000,
        deadline in 1_000u64..u64::MAX,
        tamper_byte in any::<u8>(),
    ) {
        let mut snap = AgentCapabilitySnapshot {
            agent_id: AccountAddress([agent_byte; 32]),
            scope: CapabilityScope::Full,
            max_value: Amount(max_value),
            deadline: TimestampMs(deadline),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: Blake3Digest([0u8; 32]),
        };

        // Set correct hash.
        snap.snapshot_hash = compute_snapshot_hash(&snap);
        prop_assert!(validate_capability_against_snapshot(&snap, 0).is_ok());

        // Tamper: flip one byte in the hash.
        let mut tampered = snap.snapshot_hash.0;
        tampered[0] = tampered[0].wrapping_add(tamper_byte.max(1));
        snap.snapshot_hash = Blake3Digest(tampered);
        prop_assert!(validate_capability_against_snapshot(&snap, 0).is_err());
    }

    /// Different agent fields → different hashes (collision resistance).
    #[test]
    fn fv_ac_005_hash_varies_with_fields(
        a1 in any::<u8>(),
        a2 in any::<u8>(),
        val1 in 0u64..1_000_000,
        val2 in 0u64..1_000_000,
    ) {
        let snap1 = AgentCapabilitySnapshot {
            agent_id: AccountAddress([a1; 32]),
            scope: CapabilityScope::Full,
            max_value: Amount(val1),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: Blake3Digest([0u8; 32]),
        };
        let snap2 = AgentCapabilitySnapshot {
            agent_id: AccountAddress([a2; 32]),
            scope: CapabilityScope::Full,
            max_value: Amount(val2),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: Blake3Digest([0u8; 32]),
        };

        let h1 = compute_snapshot_hash(&snap1);
        let h2 = compute_snapshot_hash(&snap2);

        if a1 != a2 || val1 != val2 {
            prop_assert_ne!(h1, h2);
        }
    }
}

// ── FV-AC-006: Revocation cascade completeness ──────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// Build a linear chain A→B→C→D→...→Z and revoke agent at position `k`.
    /// Everything from position `k` onward must be revoked; earlier links untouched.
    #[test]
    fn fv_ac_006_cascade_completeness(
        chain_len in 2usize..=10,
        revoke_pos in 0usize..10,
    ) {
        let revoke_pos = revoke_pos % chain_len;

        // Build addresses: 0x01, 0x02, ..., 0x(chain_len+1)
        let addrs: Vec<AccountAddress> = (0..=chain_len)
            .map(|i| AccountAddress([(i + 1) as u8; 32]))
            .collect();

        let mut tokens: Vec<DelegationToken> = (0..chain_len)
            .map(|i| {
                DelegationToken::new(DelegationLink {
                    delegator: addrs[i],
                    delegatee: addrs[i + 1],
                    max_value: Amount(10_000 - i as u64 * 100),
                    deadline: TimestampMs(10_000_000 - i as u64 * 1000),
                    allowed_contracts: vec![],
                    scope: CapabilityScope::Full,
                })
            })
            .collect();

        // Revoke the agent at `revoke_pos` (= delegatee of link[revoke_pos-1],
        // but we revoke by delegator matching).
        let revoked_agent = addrs[revoke_pos];
        revocation_cascade(&mut tokens, &revoked_agent);

        for (i, token) in tokens.iter().enumerate() {
            if i >= revoke_pos {
                prop_assert!(
                    token.revoked,
                    "token[{i}] should be revoked (revoke_pos={revoke_pos})"
                );
            } else {
                prop_assert!(
                    !token.revoked,
                    "token[{i}] should NOT be revoked (revoke_pos={revoke_pos})"
                );
            }
        }
    }
}

// ── Scope subset relation (bonus — strengthens FV-AC-001 scope check) ──

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Full is a universal parent: any scope ⊆ Full.
    #[test]
    fn scope_any_subset_of_full(scope in arb_scope()) {
        prop_assert!(scope_is_subset(&scope, &CapabilityScope::Full));
    }

    /// Full is NOT a subset of non-Full scopes.
    #[test]
    fn scope_full_not_subset_of_narrow(scope in arb_scope()) {
        match scope {
            CapabilityScope::Full => {} // skip: Full ⊆ Full = true
            other => prop_assert!(!scope_is_subset(&CapabilityScope::Full, &other)),
        }
    }

    /// IntentTypes subset reflexivity: any type list is a subset of itself.
    #[test]
    fn scope_intent_types_reflexive(
        types in prop::collection::vec("[a-z]{3,8}", 1..=5),
    ) {
        let scope = CapabilityScope::IntentTypes(types);
        prop_assert!(scope_is_subset(&scope, &scope));
    }
}
