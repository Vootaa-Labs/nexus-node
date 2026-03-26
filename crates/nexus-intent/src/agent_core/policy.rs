// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Policy enforcement — human confirmation gates and value/contract rules.
//!
//! Absorbs and extends the logic from the legacy `agent/capability.rs`
//! into a unified policy engine.  Every agent request passes through
//! [`evaluate_policy`] before dispatch, which determines whether:
//!
//! - The request is pre-approved (no human needed).
//! - A human confirmation gate must fire.
//! - The request is outright denied.
//!
//! # Design (TLD-07 §7)
//!
//! The policy engine evaluates three orthogonal axes:
//! 1. **Value gate**: per-action and aggregate value limits.
//! 2. **Contract allowlist**: deny calls to unlisted contracts.
//! 3. **Confirmation threshold**: value-based human confirmation trigger.

use nexus_primitives::{Amount, ContractAddress};
use serde::{Deserialize, Serialize};

use crate::agent_core::capability_snapshot::AgentCapabilitySnapshot;
use crate::agent_core::envelope::AgentExecutionConstraints;
use crate::error::{IntentError, IntentResult};
use crate::types::UserIntent;

// ── PolicyDecision ──────────────────────────────────────────────────────

/// Outcome of policy evaluation for a single request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyDecision {
    /// Request is pre-approved — proceed without human gate.
    Approved,
    /// Request requires human confirmation before proceeding.
    RequiresConfirmation {
        /// Reason the gate was triggered.
        reason: String,
    },
    /// Request is denied by policy — do not proceed.
    Denied {
        /// Reason for denial.
        reason: String,
    },
}

// ── ConfirmationThreshold ───────────────────────────────────────────────

/// Configurable threshold for human confirmation triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationThreshold {
    /// Actions above this value require human confirmation.
    pub value_threshold: Amount,
}

impl Default for ConfirmationThreshold {
    fn default() -> Self {
        Self {
            value_threshold: Amount(0),
        }
    }
}

// ── Core policy evaluation ──────────────────────────────────────────────

/// Evaluate the policy for a given envelope against a capability snapshot.
///
/// Checks (in order):
/// 1. Value limit — each intent's value must be ≤ `capability.max_value`.
/// 2. Contract allowlist — if non-empty, each contract call must target
///    an allowed contract.
/// 3. Confirmation threshold — if `threshold` is set and action value ≥
///    threshold, require human confirmation.
///
/// # Returns
///
/// - `Ok(PolicyDecision::Approved)` if all checks pass and value is
///   below the confirmation threshold.
/// - `Ok(PolicyDecision::RequiresConfirmation { .. })` if the value
///   exceeds the confirmation threshold but is within limits.
/// - `Err(IntentError)` if an outright policy violation is detected
///   (value limit exceeded, contract not allowed).
pub fn evaluate_policy(
    intents: &[UserIntent],
    constraints: &AgentExecutionConstraints,
    capability: &AgentCapabilitySnapshot,
    threshold: &ConfirmationThreshold,
) -> IntentResult<PolicyDecision> {
    for intent in intents {
        // 1. Value limit check
        let value = crate::agent_core::action_value(intent);
        if value > capability.max_value {
            return Err(IntentError::AgentValueLimitExceeded {
                value: value.0,
                limit: capability.max_value.0,
            });
        }
        if value > constraints.max_total_value {
            return Err(IntentError::AgentValueLimitExceeded {
                value: value.0,
                limit: constraints.max_total_value.0,
            });
        }

        // 2. Contract allowlist check
        if let UserIntent::ContractCall { contract, .. } = intent {
            if !capability.allowed_contracts.is_empty()
                && !capability.allowed_contracts.contains(contract)
            {
                return Err(IntentError::AgentCapabilityDenied {
                    reason: format!("contract {:?} not in capability allowlist", contract.0,),
                });
            }
            if !constraints.allowed_contracts.is_empty()
                && !constraints.allowed_contracts.contains(contract)
            {
                return Err(IntentError::AgentCapabilityDenied {
                    reason: format!("contract {:?} not in envelope allowlist", contract.0,),
                });
            }
        }

        // 3. Confirmation threshold — zero-value actions (contract calls with no
        //    value transfer, read-only queries) never trigger confirmation even if
        //    threshold is 0, because they carry no financial impact.
        if threshold.value_threshold > Amount::ZERO && value >= threshold.value_threshold {
            return Ok(PolicyDecision::RequiresConfirmation {
                reason: format!(
                    "action value {} >= confirmation threshold {}",
                    value.0, threshold.value_threshold.0,
                ),
            });
        }
    }

    Ok(PolicyDecision::Approved)
}

/// Check a single contract call against an allowlist.
pub fn is_contract_allowed(contract: &ContractAddress, allowlist: &[ContractAddress]) -> bool {
    allowlist.is_empty() || allowlist.contains(contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::capability_snapshot::{AgentCapabilitySnapshot, CapabilityScope};
    use crate::agent_core::envelope::AgentExecutionConstraints;
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, ContractAddress, TimestampMs, TokenId,
    };

    fn make_capability(max_val: u64, contracts: Vec<ContractAddress>) -> AgentCapabilitySnapshot {
        AgentCapabilitySnapshot {
            agent_id: AccountAddress([0xAA; 32]),
            scope: CapabilityScope::Full,
            max_value: Amount(max_val),
            deadline: TimestampMs(u64::MAX),
            allowed_contracts: contracts,
            delegation_chain: vec![],
            snapshot_hash: Blake3Digest([0x01; 32]),
        }
    }

    fn make_constraints(
        max_val: u64,
        contracts: Vec<ContractAddress>,
    ) -> AgentExecutionConstraints {
        AgentExecutionConstraints {
            max_gas: 100_000,
            max_total_value: Amount(max_val),
            allowed_contracts: contracts,
        }
    }

    fn transfer_intent(amount: u64) -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(amount),
        }
    }

    fn contract_call(contract: ContractAddress) -> UserIntent {
        UserIntent::ContractCall {
            contract,
            function: "do_thing".into(),
            args: vec![],
            gas_budget: 10_000,
        }
    }

    #[test]
    fn approved_below_threshold() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold {
            value_threshold: Amount(5_000),
        };
        let intents = vec![transfer_intent(1_000)];
        let decision = evaluate_policy(&intents, &con, &cap, &threshold).unwrap();
        assert_eq!(decision, PolicyDecision::Approved);
    }

    #[test]
    fn requires_confirmation_at_threshold() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold {
            value_threshold: Amount(1_000),
        };
        let intents = vec![transfer_intent(1_000)];
        let decision = evaluate_policy(&intents, &con, &cap, &threshold).unwrap();
        assert!(matches!(
            decision,
            PolicyDecision::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn denied_over_cap_value() {
        let cap = make_capability(500, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold::default();
        let intents = vec![transfer_intent(1_000)];
        assert!(evaluate_policy(&intents, &con, &cap, &threshold).is_err());
    }

    #[test]
    fn denied_over_constraint_value() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(500, vec![]);
        let threshold = ConfirmationThreshold::default();
        let intents = vec![transfer_intent(1_000)];
        assert!(evaluate_policy(&intents, &con, &cap, &threshold).is_err());
    }

    #[test]
    fn contract_allowed_when_list_empty() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold::default();
        let intents = vec![contract_call(ContractAddress([0xCC; 32]))];
        let decision = evaluate_policy(&intents, &con, &cap, &threshold).unwrap();
        assert_eq!(decision, PolicyDecision::Approved);
    }

    #[test]
    fn contract_denied_not_in_cap_allowlist() {
        let allowed = ContractAddress([0xDD; 32]);
        let target = ContractAddress([0xCC; 32]);
        let cap = make_capability(10_000, vec![allowed]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold::default();
        let intents = vec![contract_call(target)];
        assert!(evaluate_policy(&intents, &con, &cap, &threshold).is_err());
    }

    #[test]
    fn contract_denied_not_in_constraint_allowlist() {
        let allowed = ContractAddress([0xDD; 32]);
        let target = ContractAddress([0xCC; 32]);
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![allowed]);
        let threshold = ConfirmationThreshold::default();
        let intents = vec![contract_call(target)];
        assert!(evaluate_policy(&intents, &con, &cap, &threshold).is_err());
    }

    #[test]
    fn multiple_intents_first_triggers_confirmation() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold {
            value_threshold: Amount(500),
        };
        let intents = vec![transfer_intent(1_000), transfer_intent(100)];
        let decision = evaluate_policy(&intents, &con, &cap, &threshold).unwrap();
        assert!(matches!(
            decision,
            PolicyDecision::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn empty_intent_list_approved() {
        let cap = make_capability(10_000, vec![]);
        let con = make_constraints(10_000, vec![]);
        let threshold = ConfirmationThreshold::default();
        let decision = evaluate_policy(&[], &con, &cap, &threshold).unwrap();
        assert_eq!(decision, PolicyDecision::Approved);
    }

    #[test]
    fn is_contract_allowed_empty_list() {
        assert!(is_contract_allowed(&ContractAddress([0xCC; 32]), &[],));
    }

    #[test]
    fn is_contract_allowed_present() {
        let c = ContractAddress([0xCC; 32]);
        assert!(is_contract_allowed(&c, &[c]));
    }

    #[test]
    fn is_contract_allowed_absent() {
        let c = ContractAddress([0xCC; 32]);
        let other = ContractAddress([0xDD; 32]);
        assert!(!is_contract_allowed(&c, &[other]));
    }
}
