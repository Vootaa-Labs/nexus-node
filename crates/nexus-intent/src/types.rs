// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent layer data types.
//!
//! Core structures for intent representation, compilation results,
//! and agent protocol payloads.  These types bridge the user-facing
//! API layer with the internal compiler and routing engine.

use nexus_crypto::{DilithiumSignature, DilithiumVerifyKey};
use nexus_execution::types::SignedTransaction;
use nexus_primitives::{
    AccountAddress, Amount, ContractAddress, EpochNumber, IntentId, ShardId, TimestampMs, TokenId,
    ValidatorIndex,
};
use serde::{Deserialize, Serialize};

// ── Domain separation constants ─────────────────────────────────────────

/// Domain tag for intent body hashing.
pub const INTENT_DOMAIN: &[u8] = b"nexus::intent::user_intent::v1";

/// Maximum intent payload size in bytes (64 KiB).
pub const MAX_INTENT_SIZE: usize = 64 * 1024;

/// Maximum steps allowed in a single compiled intent plan.
pub const MAX_STEPS_PER_INTENT: usize = 16;

// ── UserIntent ──────────────────────────────────────────────────────────

/// High-level user intent — what the user *wants* to happen, without
/// specifying shard routing or low-level transaction details.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UserIntent {
    /// Simple token transfer to another account.
    Transfer {
        /// Recipient account.
        to: AccountAddress,
        /// Token to transfer.
        token: TokenId,
        /// Amount to transfer.
        amount: Amount,
    },

    /// Token swap via AMM or order matching.
    Swap {
        /// Token to sell.
        from_token: TokenId,
        /// Token to buy.
        to_token: TokenId,
        /// Amount of `from_token` to sell.
        amount: Amount,
        /// Maximum slippage in basis points (1/10000).
        max_slippage_bps: u16,
    },

    /// Call a published Move contract function.
    ContractCall {
        /// Target contract address.
        contract: ContractAddress,
        /// Move function identifier (e.g. `"transfer"`).
        function: String,
        /// BCS-encoded arguments.
        args: Vec<Vec<u8>>,
        /// Maximum gas budget for this call.
        gas_budget: u64,
    },

    /// Stake tokens with a validator.
    Stake {
        /// Target validator.
        validator: ValidatorIndex,
        /// Amount to stake.
        amount: Amount,
    },

    /// AI Agent-submitted multi-step task.
    AgentTask {
        /// NAP protocol task specification.
        spec: AgentIntentSpec,
    },
}

// ── SignedUserIntent ────────────────────────────────────────────────────

/// A user intent with cryptographic signature for authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedUserIntent {
    /// The high-level intent.
    pub intent: UserIntent,
    /// Sender's account address.
    pub sender: AccountAddress,
    /// ML-DSA (Dilithium3) signature over BCS(intent ‖ sender ‖ nonce).
    pub signature: DilithiumSignature,
    /// Sender's public key for verification.
    pub sender_pk: DilithiumVerifyKey,
    /// Monotonically increasing nonce for replay protection.
    pub nonce: u64,
    /// Timestamp when the intent was created.
    pub created_at: TimestampMs,
    /// BLAKE3 digest of the signed payload.
    pub digest: IntentId,
}

// ── CompiledIntentPlan ──────────────────────────────────────────────────

/// The output of intent compilation — an ordered sequence of concrete
/// transactions ready for consensus submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledIntentPlan {
    /// Unique identifier for this intent.
    pub intent_id: IntentId,
    /// Ordered execution steps.
    pub steps: Vec<IntentStep>,
    /// Whether this plan requires cross-shard HTLC coordination.
    pub requires_htlc: bool,
    /// Estimated total gas cost across all steps.
    pub estimated_gas: u64,
    /// Epoch after which this plan expires.
    pub expires_at: EpochNumber,
}

/// A single step in a compiled intent plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentStep {
    /// Shard on which this step executes.
    pub shard_id: ShardId,
    /// The concrete signed transaction for this step.
    pub transaction: SignedTransaction,
    /// Indices of prior steps this step depends on (DAG ordering).
    pub depends_on: Vec<usize>,
}

// ── GasEstimate ─────────────────────────────────────────────────────────

/// Result of a gas estimation query (simulation only, no state change).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GasEstimate {
    /// Estimated gas units.
    pub gas_units: u64,
    /// Number of shards the intent would touch.
    pub shards_touched: u16,
    /// Whether cross-shard coordination (HTLC) is needed.
    pub requires_cross_shard: bool,
}

// ── ContractLocation ────────────────────────────────────────────────────

/// Physical location of a deployed contract in the shard topology.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractLocation {
    /// Shard hosting the contract.
    pub shard_id: ShardId,
    /// On-chain contract address.
    pub contract_addr: ContractAddress,
    /// Move module name.
    pub module_name: String,
    /// Whether the contract has been formally verified.
    pub verified: bool,
}

// ── IntentStatus ────────────────────────────────────────────────────────

/// Status of an intent as it moves through the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntentStatus {
    /// Intent received, pending compilation.
    Pending,
    /// Intent compiled, steps submitted to consensus.
    Submitted {
        /// Number of steps submitted.
        steps: usize,
    },
    /// All steps executed successfully.
    Completed {
        /// Total gas consumed across all steps.
        gas_used: u64,
    },
    /// Intent failed.
    Failed {
        /// Human-readable failure reason.
        reason: String,
    },
    /// Intent expired before completion.
    Expired,
}

// ── AI Agent Protocol (NAP) types ───────────────────────────────────────

/// Nexus Agent Protocol (NAP) task specification.
///
/// Defines what an AI agent wants to do, with constraints and
/// human approval requirements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentIntentSpec {
    /// Protocol version (e.g. `"nap/1.0"`).
    pub version: String,
    /// Agent's account identity.
    pub agent_id: AccountAddress,
    /// Move Capability Token ID authorising the agent.
    pub capability_token: TokenId,
    /// Task(s) the agent wants to perform.
    pub task: AgentTask,
    /// Operational constraints.
    pub constraints: AgentConstraints,
    /// Human approval policy.
    pub human_approval: HumanApproval,
}

/// Agent task — single action or multi-step workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentTask {
    /// Execute a single intent.
    SingleAction {
        /// The intent to execute.
        action: Box<UserIntent>,
    },
    /// Multi-step workflow with dependency ordering.
    MultiStep {
        /// Steps to execute.
        steps: Vec<UserIntent>,
        /// Execution order as levels (each inner Vec runs in parallel).
        execution_order: Vec<Vec<usize>>,
    },
}

/// Constraints on AI agent behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentConstraints {
    /// Maximum gas the agent may spend.
    pub max_gas: u64,
    /// Maximum value the agent may transfer in a single action.
    pub max_value: Amount,
    /// Contracts the agent is allowed to call (empty = all allowed).
    pub allowed_contracts: Vec<ContractAddress>,
    /// Deadline after which the agent task expires.
    pub deadline: TimestampMs,
}

/// Human approval policy for agent actions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HumanApproval {
    /// No human approval required.
    PreApproved,
    /// Require confirmation for actions above a value threshold.
    RequireConfirmation {
        /// Actions above this value need human sign-off.
        threshold_value: Amount,
    },
}

// ── Digest computation ──────────────────────────────────────────────────

/// Compute the canonical BLAKE3 digest of a user intent payload.
///
/// `BLAKE3(INTENT_DOMAIN ‖ BCS(intent) ‖ BCS(sender) ‖ BCS(nonce))`
///
/// # Errors
///
/// Returns [`IntentError::Codec`] if BCS serialization fails.
pub fn compute_intent_digest(
    intent: &UserIntent,
    sender: &AccountAddress,
    nonce: u64,
) -> crate::error::IntentResult<IntentId> {
    let intent_bytes =
        bcs::to_bytes(intent).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let sender_bytes =
        bcs::to_bytes(sender).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;
    let nonce_bytes =
        bcs::to_bytes(&nonce).map_err(|e| crate::error::IntentError::Codec(e.to_string()))?;

    let mut hasher = blake3::Hasher::new();
    hasher.update(INTENT_DOMAIN);
    hasher.update(&intent_bytes);
    hasher.update(&sender_bytes);
    hasher.update(&nonce_bytes);
    let hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(nexus_primitives::Blake3Digest(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_transfer() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        }
    }

    fn sample_swap() -> UserIntent {
        UserIntent::Swap {
            from_token: TokenId::Native,
            to_token: TokenId::Contract(ContractAddress([0xCC; 32])),
            amount: Amount(500),
            max_slippage_bps: 50,
        }
    }

    fn sample_contract_call() -> UserIntent {
        UserIntent::ContractCall {
            contract: ContractAddress([0xDD; 32]),
            function: "transfer".to_string(),
            args: vec![vec![1, 2, 3]],
            gas_budget: 50_000,
        }
    }

    fn sample_stake() -> UserIntent {
        UserIntent::Stake {
            validator: ValidatorIndex(5),
            amount: Amount(10_000),
        }
    }

    // ── Digest tests ────────────────────────────────────────────────

    #[test]
    fn intent_digest_deterministic() {
        let intent = sample_transfer();
        let sender = AccountAddress([0xAA; 32]);
        let d1 = compute_intent_digest(&intent, &sender, 1).unwrap();
        let d2 = compute_intent_digest(&intent, &sender, 1).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn intent_digest_changes_with_nonce() {
        let intent = sample_transfer();
        let sender = AccountAddress([0xAA; 32]);
        let d1 = compute_intent_digest(&intent, &sender, 1).unwrap();
        let d2 = compute_intent_digest(&intent, &sender, 2).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn intent_digest_changes_with_sender() {
        let intent = sample_transfer();
        let d1 = compute_intent_digest(&intent, &AccountAddress([0xAA; 32]), 1).unwrap();
        let d2 = compute_intent_digest(&intent, &AccountAddress([0xBB; 32]), 1).unwrap();
        assert_ne!(d1, d2);
    }

    #[test]
    fn intent_digest_changes_with_intent_variant() {
        let sender = AccountAddress([0xAA; 32]);
        let d_transfer = compute_intent_digest(&sample_transfer(), &sender, 1).unwrap();
        let d_swap = compute_intent_digest(&sample_swap(), &sender, 1).unwrap();
        let d_call = compute_intent_digest(&sample_contract_call(), &sender, 1).unwrap();
        let d_stake = compute_intent_digest(&sample_stake(), &sender, 1).unwrap();
        assert_ne!(d_transfer, d_swap);
        assert_ne!(d_transfer, d_call);
        assert_ne!(d_transfer, d_stake);
        assert_ne!(d_swap, d_call);
    }

    // ── Serialization round-trip ────────────────────────────────────

    #[test]
    fn user_intent_bcs_round_trip_transfer() {
        let intent = sample_transfer();
        let bytes = bcs::to_bytes(&intent).unwrap();
        let decoded: UserIntent = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(intent, decoded);
    }

    #[test]
    fn user_intent_bcs_round_trip_swap() {
        let intent = sample_swap();
        let bytes = bcs::to_bytes(&intent).unwrap();
        let decoded: UserIntent = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(intent, decoded);
    }

    #[test]
    fn user_intent_bcs_round_trip_contract_call() {
        let intent = sample_contract_call();
        let bytes = bcs::to_bytes(&intent).unwrap();
        let decoded: UserIntent = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(intent, decoded);
    }

    #[test]
    fn user_intent_bcs_round_trip_stake() {
        let intent = sample_stake();
        let bytes = bcs::to_bytes(&intent).unwrap();
        let decoded: UserIntent = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(intent, decoded);
    }

    // ── Agent spec tests ────────────────────────────────────────────

    #[test]
    fn agent_intent_spec_bcs_round_trip() {
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id: AccountAddress([0x01; 32]),
            capability_token: TokenId::Contract(ContractAddress([0x02; 32])),
            task: AgentTask::SingleAction {
                action: Box::new(sample_transfer()),
            },
            constraints: AgentConstraints {
                max_gas: 100_000,
                max_value: Amount(50_000),
                allowed_contracts: vec![ContractAddress([0xDD; 32])],
                deadline: TimestampMs(1_700_000_000_000),
            },
            human_approval: HumanApproval::PreApproved,
        };
        let bytes = bcs::to_bytes(&spec).unwrap();
        let decoded: AgentIntentSpec = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(spec, decoded);
    }

    #[test]
    fn agent_multi_step_bcs_round_trip() {
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id: AccountAddress([0x01; 32]),
            capability_token: TokenId::Native,
            task: AgentTask::MultiStep {
                steps: vec![sample_transfer(), sample_swap()],
                execution_order: vec![vec![0], vec![1]],
            },
            constraints: AgentConstraints {
                max_gas: 200_000,
                max_value: Amount(100_000),
                allowed_contracts: vec![],
                deadline: TimestampMs(1_700_000_000_000),
            },
            human_approval: HumanApproval::RequireConfirmation {
                threshold_value: Amount(10_000),
            },
        };
        let bytes = bcs::to_bytes(&spec).unwrap();
        let decoded: AgentIntentSpec = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(spec, decoded);
    }

    // ── IntentStatus tests ──────────────────────────────────────────

    #[test]
    fn intent_status_variants() {
        let statuses = vec![
            IntentStatus::Pending,
            IntentStatus::Submitted { steps: 3 },
            IntentStatus::Completed { gas_used: 42_000 },
            IntentStatus::Failed {
                reason: "nope".into(),
            },
            IntentStatus::Expired,
        ];
        for s in statuses {
            let bytes = bcs::to_bytes(&s).unwrap();
            let decoded: IntentStatus = bcs::from_bytes(&bytes).unwrap();
            assert_eq!(s, decoded);
        }
    }

    // ── GasEstimate tests ───────────────────────────────────────────

    #[test]
    fn gas_estimate_bcs_round_trip() {
        let est = GasEstimate {
            gas_units: 50_000,
            shards_touched: 2,
            requires_cross_shard: true,
        };
        let bytes = bcs::to_bytes(&est).unwrap();
        let decoded: GasEstimate = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(est, decoded);
    }

    // ── ContractLocation tests ──────────────────────────────────────

    #[test]
    fn contract_location_bcs_round_trip() {
        let loc = ContractLocation {
            shard_id: ShardId(3),
            contract_addr: ContractAddress([0xDD; 32]),
            module_name: "my_module".to_string(),
            verified: true,
        };
        let bytes = bcs::to_bytes(&loc).unwrap();
        let decoded: ContractLocation = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(loc, decoded);
    }

    // ── Constants tests ─────────────────────────────────────────────

    #[test]
    fn constants_sane() {
        assert_eq!(MAX_INTENT_SIZE, 64 * 1024);
        assert_eq!(MAX_STEPS_PER_INTENT, 16);
        #[allow(clippy::const_is_empty)]
        {
            assert!(!INTENT_DOMAIN.is_empty());
        }
    }
}
