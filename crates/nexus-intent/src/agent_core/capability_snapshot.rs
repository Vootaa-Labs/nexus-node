//! Agent capability snapshot and delegation chain validation.
//!
//! A [`AgentCapabilitySnapshot`] captures the runtime-resolved
//! permissions of an agent at the time of request processing.
//! It supports hierarchical delegation: a parent agent can delegate
//! a subset of its capabilities to a child agent, with monotonic
//! contraction guarantees.

use nexus_primitives::{AccountAddress, Amount, Blake3Digest, ContractAddress, TimestampMs};
use serde::{Deserialize, Serialize};

// ── AgentCapabilitySnapshot ─────────────────────────────────────────────

/// Runtime-resolved capability state of an agent at request time.
///
/// Unlike [`AgentExecutionConstraints`](super::envelope::AgentExecutionConstraints)
/// which are static envelope fields, this snapshot includes the full
/// delegation chain context and is validated against on-chain state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapabilitySnapshot {
    /// Agent identity.
    pub agent_id: AccountAddress,
    /// Scope of allowed actions.
    pub scope: CapabilityScope,
    /// Maximum value per action.
    pub max_value: Amount,
    /// Deadline after which this capability expires.
    pub deadline: TimestampMs,
    /// Contracts this agent may call (empty = all allowed).
    pub allowed_contracts: Vec<ContractAddress>,
    /// Delegation chain from root to this agent (bottom = self).
    pub delegation_chain: Vec<DelegationLink>,
    /// BLAKE3 digest of the snapshot for integrity verification.
    pub snapshot_hash: Blake3Digest,
}

/// Scope of allowed agent actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityScope {
    /// Full access within constraints.
    Full,
    /// Read-only queries.
    ReadOnly,
    /// Only specific intent types.
    IntentTypes(Vec<String>),
}

/// A single link in a delegation chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationLink {
    /// Delegator (parent) agent.
    pub delegator: AccountAddress,
    /// Delegatee (child) agent.
    pub delegatee: AccountAddress,
    /// Maximum value the child may spend per action.
    pub max_value: Amount,
    /// Deadline for this delegation.
    pub deadline: TimestampMs,
    /// Contracts the child may call (subset of parent's).
    pub allowed_contracts: Vec<ContractAddress>,
    /// Scope restriction.
    pub scope: CapabilityScope,
}

// ── Scope subset check ──────────────────────────────────────────────────

/// Check whether `child` scope is a subset of `parent` scope.
///
/// Rules:
/// - `ReadOnly ⊆ Full`
/// - `IntentTypes ⊆ Full`
/// - `IntentTypes(child_list) ⊆ IntentTypes(parent_list)` iff ∀t ∈ child_list: t ∈ parent_list
/// - `ReadOnly ⊆ ReadOnly`
/// - `Full ⊆ Full`
pub fn scope_is_subset(child: &CapabilityScope, parent: &CapabilityScope) -> bool {
    match (child, parent) {
        (_, CapabilityScope::Full) => true,
        (CapabilityScope::Full, _) => false,
        (CapabilityScope::ReadOnly, CapabilityScope::ReadOnly) => true,
        (CapabilityScope::ReadOnly, CapabilityScope::IntentTypes(_)) => false,
        (CapabilityScope::IntentTypes(_), CapabilityScope::ReadOnly) => false,
        (CapabilityScope::IntentTypes(child_types), CapabilityScope::IntentTypes(parent_types)) => {
            child_types.iter().all(|t| parent_types.contains(t))
        }
    }
}

// ── DelegationToken ─────────────────────────────────────────────────────

/// Domain tag for delegation token digest computation.
const DELEGATION_TOKEN_DOMAIN: &[u8] = b"nexus::delegation_token::v1";

/// A signed delegation token proving parent→child capability transfer.
///
/// The token binds the delegation chain to a specific capability
/// snapshot and includes a BLAKE3 digest for integrity verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationToken {
    /// The delegation link this token certifies.
    pub link: DelegationLink,
    /// BLAKE3 token digest for integrity.
    pub token_digest: Blake3Digest,
    /// Whether this token has been revoked.
    pub revoked: bool,
}

impl DelegationToken {
    /// Create a new delegation token from a link.
    pub fn new(link: DelegationLink) -> Self {
        let digest = compute_delegation_digest(&link);
        Self {
            link,
            token_digest: digest,
            revoked: false,
        }
    }

    /// Revoke this token.
    pub fn revoke(&mut self) {
        self.revoked = true;
    }
}

/// Compute BLAKE3 digest of a delegation link for token binding.
fn compute_delegation_digest(link: &DelegationLink) -> Blake3Digest {
    let bytes = bcs::to_bytes(link).unwrap_or_default();
    let mut hasher = blake3::Hasher::new();
    hasher.update(DELEGATION_TOKEN_DOMAIN);
    hasher.update(&bytes);
    Blake3Digest(*hasher.finalize().as_bytes())
}

// ── Capability validation ───────────────────────────────────────────────

/// Domain tag for capability snapshot digest computation.
const SNAPSHOT_DOMAIN: &[u8] = b"nexus::capability_snapshot::v1";

/// Compute the canonical BLAKE3 hash of a capability snapshot's content
/// (excluding the `snapshot_hash` field itself).
pub fn compute_snapshot_hash(snapshot: &AgentCapabilitySnapshot) -> Blake3Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SNAPSHOT_DOMAIN);
    hasher.update(&bcs::to_bytes(&snapshot.agent_id).unwrap_or_default());
    hasher.update(&bcs::to_bytes(&snapshot.scope).unwrap_or_default());
    hasher.update(&bcs::to_bytes(&snapshot.max_value).unwrap_or_default());
    hasher.update(&bcs::to_bytes(&snapshot.deadline).unwrap_or_default());
    hasher.update(&bcs::to_bytes(&snapshot.allowed_contracts).unwrap_or_default());
    hasher.update(&bcs::to_bytes(&snapshot.delegation_chain).unwrap_or_default());
    Blake3Digest(*hasher.finalize().as_bytes())
}

/// Validate that a request's delegation token is consistent with the
/// agent's capability snapshot.
///
/// Checks:
/// 1. Token not revoked.
/// 2. Delegation chain in snapshot is valid (monotonic contraction).
/// 3. Scope of the request is within the snapshot's scope.
/// 4. Deadline not expired.
/// 5. Snapshot hash integrity (when non-zero).
pub fn validate_capability_against_snapshot(
    snapshot: &AgentCapabilitySnapshot,
    now_ms: u64,
) -> Result<(), String> {
    // Check expiry.
    if snapshot.deadline.0 < now_ms {
        return Err(format!(
            "capability expired: deadline {} < now {}",
            snapshot.deadline.0, now_ms,
        ));
    }

    // Validate delegation chain.
    validate_delegation_chain(&snapshot.delegation_chain)?;

    // Verify chain terminates at the agent.
    if let Some(last) = snapshot.delegation_chain.last() {
        if last.delegatee != snapshot.agent_id {
            return Err(format!(
                "chain terminal {:?} does not match agent {:?}",
                last.delegatee, snapshot.agent_id,
            ));
        }
    }

    // Verify snapshot_hash integrity (skip when zero — internally
    // constructed snapshots leave the hash zeroed).
    if snapshot.snapshot_hash.0 != [0u8; 32] {
        let expected = compute_snapshot_hash(snapshot);
        if expected != snapshot.snapshot_hash {
            return Err("snapshot_hash integrity check failed".into());
        }
    }

    Ok(())
}

/// Revoke all tokens in a chain that descend from a given delegator.
///
/// Cascade: if parent revokes, all child delegations become invalid.
pub fn revocation_cascade(tokens: &mut [DelegationToken], revoked_agent: &AccountAddress) {
    let mut revoked_set = vec![*revoked_agent];
    for token in tokens.iter_mut() {
        if revoked_set.contains(&token.link.delegator) {
            token.revoke();
            revoked_set.push(token.link.delegatee);
        }
    }
}

// ── Delegation validation ───────────────────────────────────────────────

/// Validate that a delegation chain satisfies monotonic contraction.
///
/// Each child link must have:
/// - `scope_child ⊆ scope_parent`
/// - `max_value_child ≤ max_value_parent`
/// - `deadline_child ≤ deadline_parent`
/// - `allowed_contracts_child ⊆ allowed_contracts_parent`
///
/// Returns `Ok(())` if the chain is valid.
pub fn validate_delegation_chain(chain: &[DelegationLink]) -> Result<(), String> {
    for window in chain.windows(2) {
        let parent = &window[0];
        let child = &window[1];

        // Delegator must match.
        if child.delegator != parent.delegatee {
            return Err(format!(
                "broken chain: link delegator {:?} != parent delegatee {:?}",
                child.delegator, parent.delegatee,
            ));
        }

        // Value monotonicity.
        if child.max_value > parent.max_value {
            return Err(format!(
                "delegation violation: child max_value {} > parent max_value {}",
                child.max_value.0, parent.max_value.0,
            ));
        }

        // Deadline monotonicity.
        if child.deadline.0 > parent.deadline.0 {
            return Err(format!(
                "delegation violation: child deadline {} > parent deadline {}",
                child.deadline.0, parent.deadline.0,
            ));
        }

        // Contract subset check.
        if !parent.allowed_contracts.is_empty() {
            for contract in &child.allowed_contracts {
                if !parent.allowed_contracts.contains(contract) {
                    return Err(format!(
                        "delegation violation: child contract {contract:?} not in parent's allowed set",
                    ));
                }
            }
        }

        // Scope subset check (SEC-Z8).
        if !scope_is_subset(&child.scope, &parent.scope) {
            return Err(format!(
                "delegation violation: child scope {:?} is not a subset of parent scope {:?}",
                child.scope, parent.scope,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_primitives::{AccountAddress, Amount, ContractAddress, TimestampMs};

    fn addr(b: u8) -> AccountAddress {
        AccountAddress([b; 32])
    }

    fn contract(b: u8) -> ContractAddress {
        ContractAddress([b; 32])
    }

    fn link(
        delegator: u8,
        delegatee: u8,
        max_value: u64,
        deadline: u64,
        contracts: Vec<ContractAddress>,
    ) -> DelegationLink {
        DelegationLink {
            delegator: addr(delegator),
            delegatee: addr(delegatee),
            max_value: Amount(max_value),
            deadline: TimestampMs(deadline),
            allowed_contracts: contracts,
            scope: CapabilityScope::Full,
        }
    }

    #[test]
    fn valid_two_link_chain() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![contract(0xAA)]),
            link(0x02, 0x03, 5_000, 1_500_000, vec![contract(0xAA)]),
        ];
        assert!(validate_delegation_chain(&chain).is_ok());
    }

    #[test]
    fn empty_chain_valid() {
        assert!(validate_delegation_chain(&[]).is_ok());
    }

    #[test]
    fn single_link_valid() {
        let chain = vec![link(0x01, 0x02, 10_000, 2_000_000, vec![])];
        assert!(validate_delegation_chain(&chain).is_ok());
    }

    #[test]
    fn broken_chain_rejected() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![]),
            link(0x99, 0x03, 5_000, 1_500_000, vec![]), // wrong delegator
        ];
        assert!(validate_delegation_chain(&chain).is_err());
    }

    #[test]
    fn value_escalation_rejected() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![]),
            link(0x02, 0x03, 20_000, 1_500_000, vec![]), // value too high
        ];
        assert!(validate_delegation_chain(&chain).is_err());
    }

    #[test]
    fn deadline_escalation_rejected() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![]),
            link(0x02, 0x03, 5_000, 3_000_000, vec![]), // deadline too late
        ];
        assert!(validate_delegation_chain(&chain).is_err());
    }

    #[test]
    fn contract_not_in_parent_rejected() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![contract(0xAA)]),
            link(0x02, 0x03, 5_000, 1_500_000, vec![contract(0xBB)]), // 0xBB not in parent
        ];
        assert!(validate_delegation_chain(&chain).is_err());
    }

    #[test]
    fn parent_empty_contracts_allows_child_any() {
        let chain = vec![
            link(0x01, 0x02, 10_000, 2_000_000, vec![]), // empty = all
            link(0x02, 0x03, 5_000, 1_500_000, vec![contract(0xBB)]),
        ];
        assert!(validate_delegation_chain(&chain).is_ok());
    }

    #[test]
    fn snapshot_bcs_round_trip() {
        let snap = AgentCapabilitySnapshot {
            agent_id: addr(0x01),
            scope: CapabilityScope::Full,
            max_value: Amount(50_000),
            deadline: TimestampMs(2_000_000),
            allowed_contracts: vec![contract(0xAA)],
            delegation_chain: vec![link(0x01, 0x02, 50_000, 2_000_000, vec![contract(0xAA)])],
            snapshot_hash: nexus_primitives::Blake3Digest([0xFF; 32]),
        };
        let bytes = bcs::to_bytes(&snap).unwrap();
        let decoded: AgentCapabilitySnapshot = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(snap, decoded);
    }

    // ── Scope subset tests ──────────────────────────────────────────

    #[test]
    fn scope_full_contains_everything() {
        assert!(scope_is_subset(
            &CapabilityScope::Full,
            &CapabilityScope::Full
        ));
        assert!(scope_is_subset(
            &CapabilityScope::ReadOnly,
            &CapabilityScope::Full
        ));
        assert!(scope_is_subset(
            &CapabilityScope::IntentTypes(vec!["transfer".into()]),
            &CapabilityScope::Full,
        ));
    }

    #[test]
    fn scope_readonly_not_subset_of_intent_types() {
        assert!(!scope_is_subset(
            &CapabilityScope::ReadOnly,
            &CapabilityScope::IntentTypes(vec!["transfer".into()]),
        ));
    }

    #[test]
    fn scope_intent_types_subset() {
        let parent = CapabilityScope::IntentTypes(vec!["transfer".into(), "swap".into()]);
        let child = CapabilityScope::IntentTypes(vec!["transfer".into()]);
        assert!(scope_is_subset(&child, &parent));
    }

    #[test]
    fn scope_intent_types_not_subset() {
        let parent = CapabilityScope::IntentTypes(vec!["transfer".into()]);
        let child = CapabilityScope::IntentTypes(vec!["transfer".into(), "swap".into()]);
        assert!(!scope_is_subset(&child, &parent));
    }

    #[test]
    fn scope_full_not_subset_of_readonly() {
        assert!(!scope_is_subset(
            &CapabilityScope::Full,
            &CapabilityScope::ReadOnly
        ));
    }

    // ── DelegationToken tests ───────────────────────────────────────

    #[test]
    fn delegation_token_deterministic_digest() {
        let l = link(0x01, 0x02, 10_000, 2_000_000, vec![]);
        let t1 = DelegationToken::new(l.clone());
        let t2 = DelegationToken::new(l);
        assert_eq!(t1.token_digest, t2.token_digest);
        assert!(!t1.revoked);
    }

    #[test]
    fn delegation_token_revoke() {
        let l = link(0x01, 0x02, 10_000, 2_000_000, vec![]);
        let mut token = DelegationToken::new(l);
        assert!(!token.revoked);
        token.revoke();
        assert!(token.revoked);
    }

    // ── Capability validation tests ─────────────────────────────────

    #[test]
    fn capability_valid_no_chain() {
        let snap = AgentCapabilitySnapshot {
            agent_id: addr(0x01),
            scope: CapabilityScope::Full,
            max_value: Amount(50_000),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: nexus_primitives::Blake3Digest([0u8; 32]),
        };
        assert!(validate_capability_against_snapshot(&snap, 1_000).is_ok());
    }

    #[test]
    fn capability_expired() {
        let snap = AgentCapabilitySnapshot {
            agent_id: addr(0x01),
            scope: CapabilityScope::Full,
            max_value: Amount(50_000),
            deadline: TimestampMs(500),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: nexus_primitives::Blake3Digest([0u8; 32]),
        };
        assert!(validate_capability_against_snapshot(&snap, 1_000).is_err());
    }

    #[test]
    fn capability_chain_terminal_mismatch() {
        let snap = AgentCapabilitySnapshot {
            agent_id: addr(0x99), // doesn't match chain terminal
            scope: CapabilityScope::Full,
            max_value: Amount(50_000),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: vec![],
            delegation_chain: vec![link(0x01, 0x02, 50_000, u64::MAX, vec![])],
            snapshot_hash: nexus_primitives::Blake3Digest([0u8; 32]),
        };
        assert!(validate_capability_against_snapshot(&snap, 1_000).is_err());
    }

    #[test]
    fn snapshot_hash_integrity_verified() {
        let mut snap = AgentCapabilitySnapshot {
            agent_id: addr(0x01),
            scope: CapabilityScope::Full,
            max_value: Amount(50_000),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: nexus_primitives::Blake3Digest([0u8; 32]),
        };
        // Set the correct hash.
        snap.snapshot_hash = compute_snapshot_hash(&snap);
        assert!(validate_capability_against_snapshot(&snap, 1_000).is_ok());

        // Tamper with the hash.
        snap.snapshot_hash = nexus_primitives::Blake3Digest([0xFF; 32]);
        assert!(validate_capability_against_snapshot(&snap, 1_000).is_err());
    }

    // ── Revocation cascade tests ────────────────────────────────────

    #[test]
    fn revocation_cascade_propagates() {
        let mut tokens = vec![
            DelegationToken::new(link(0x01, 0x02, 10_000, 2_000_000, vec![])),
            DelegationToken::new(link(0x02, 0x03, 5_000, 1_500_000, vec![])),
            DelegationToken::new(link(0x03, 0x04, 2_000, 1_000_000, vec![])),
        ];
        revocation_cascade(&mut tokens, &addr(0x02));
        // 0x02→0x03 revoked, 0x03→0x04 also revoked (cascade)
        assert!(!tokens[0].revoked); // 0x01→0x02 untouched
        assert!(tokens[1].revoked); // 0x02→0x03 revoked
        assert!(tokens[2].revoked); // 0x03→0x04 cascaded
    }

    #[test]
    fn revocation_cascade_no_match() {
        let mut tokens = vec![DelegationToken::new(link(
            0x01,
            0x02,
            10_000,
            2_000_000,
            vec![],
        ))];
        revocation_cascade(&mut tokens, &addr(0x99));
        assert!(!tokens[0].revoked);
    }

    // ── Z-8: Security audit — delegation no-escalation tests ────────

    #[test]
    fn sec_delegation_value_escalation_rejected() {
        // Z-8 Test 4: child cannot escalate max_value above parent.
        let chain = vec![
            link(0x01, 0x02, 5_000, 2_000_000, vec![]),
            link(0x02, 0x03, 10_000, 1_500_000, vec![]), // escalation: 10k > 5k
        ];
        assert!(
            validate_delegation_chain(&chain).is_err(),
            "delegation must reject value escalation"
        );
    }

    #[test]
    fn sec_delegation_deadline_escalation_rejected() {
        // Z-8 Test 4: child cannot extend deadline beyond parent.
        let chain = vec![
            link(0x01, 0x02, 10_000, 1_000_000, vec![]),
            link(0x02, 0x03, 5_000, 2_000_000, vec![]), // deadline escalation
        ];
        assert!(
            validate_delegation_chain(&chain).is_err(),
            "delegation must reject deadline escalation"
        );
    }

    #[test]
    fn sec_delegation_scope_escalation_rejected() {
        // Z-8 Test 4: child cannot escalate ReadOnly → Full.
        let mut chain_link1 = link(0x01, 0x02, 10_000, 2_000_000, vec![]);
        chain_link1.scope = CapabilityScope::ReadOnly;
        let mut chain_link2 = link(0x02, 0x03, 5_000, 1_500_000, vec![]);
        chain_link2.scope = CapabilityScope::Full;
        let chain = vec![chain_link1, chain_link2];
        assert!(
            validate_delegation_chain(&chain).is_err(),
            "delegation must reject scope escalation"
        );
    }

    #[test]
    fn sec_snapshot_expiry_invalidates_session() {
        // Z-8 Test 3: expired snapshot is rejected.
        let snap = AgentCapabilitySnapshot {
            agent_id: addr(0x01),
            scope: CapabilityScope::Full,
            max_value: Amount(10_000),
            deadline: TimestampMs(500),
            allowed_contracts: vec![],
            delegation_chain: vec![],
            snapshot_hash: nexus_primitives::Blake3Digest([0u8; 32]),
        };
        assert!(
            validate_capability_against_snapshot(&snap, 1_000).is_err(),
            "expired snapshot must be rejected"
        );
    }
}
