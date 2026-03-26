// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent validation — structural and cryptographic checks.
//!
//! Every [`SignedUserIntent`] passes through these checks before
//! compilation:
//!
//! 1. **Size gate** — BCS-serialised payload ≤ `MAX_INTENT_SIZE`.
//! 2. **Digest match** — recompute `BLAKE3(INTENT_DOMAIN ‖ BCS(intent) ‖
//!    BCS(sender) ‖ BCS(nonce))` and compare.
//! 3. **Signature** — ML-DSA (Dilithium3) verify over the digest.
//! 4. **Field validation** — per-variant sanity checks (zero amount,
//!    empty function name, agent constraints).

use crate::config::IntentConfig;
use crate::error::{IntentError, IntentResult};
use crate::types::{
    compute_intent_digest, AgentTask, HumanApproval, SignedUserIntent, UserIntent, MAX_INTENT_SIZE,
};
use nexus_crypto::{DilithiumSigner, Signer as _};
use nexus_primitives::Amount;

/// Run all validation checks on a signed intent.
///
/// Returns `Ok(())` if the intent is structurally sound, correctly
/// signed, and within configured limits.
///
/// # Errors
///
/// Returns the first failing check as an `IntentError`.
pub fn validate_signed_intent(
    signed: &SignedUserIntent,
    config: &IntentConfig,
) -> IntentResult<()> {
    check_size(signed, config)?;
    check_digest(signed)?;
    check_signature(signed)?;
    validate_intent_fields(&signed.intent, config)?;
    Ok(())
}

// ── Individual checks ───────────────────────────────────────────────────

/// BCS-serialised size ≤ `MAX_INTENT_SIZE`.
fn check_size(signed: &SignedUserIntent, config: &IntentConfig) -> IntentResult<()> {
    let bytes = bcs::to_bytes(signed).map_err(|e| IntentError::Codec(e.to_string()))?;
    let max = config.max_intent_size_bytes.min(MAX_INTENT_SIZE);
    if bytes.len() > max {
        return Err(IntentError::IntentTooLarge {
            size: bytes.len(),
            max,
        });
    }
    Ok(())
}

/// Recompute the BLAKE3 digest and compare against `signed.digest`.
fn check_digest(signed: &SignedUserIntent) -> IntentResult<()> {
    let expected = compute_intent_digest(&signed.intent, &signed.sender, signed.nonce)?;
    if expected != signed.digest {
        return Err(IntentError::ParseError {
            reason: "digest mismatch: recomputed digest does not match signed.digest".into(),
        });
    }
    Ok(())
}

/// Verify the Dilithium3 signature over the canonical message.
///
/// Message = `BCS(intent) ‖ BCS(sender) ‖ BCS(nonce)` (same bytes
/// that feed into `compute_intent_digest`, minus the domain prefix —
/// the Signer trait adds its own domain internally).
fn check_signature(signed: &SignedUserIntent) -> IntentResult<()> {
    // Build the message exactly as in compute_intent_digest.
    let intent_bytes =
        bcs::to_bytes(&signed.intent).map_err(|e| IntentError::Codec(e.to_string()))?;
    let sender_bytes =
        bcs::to_bytes(&signed.sender).map_err(|e| IntentError::Codec(e.to_string()))?;
    let nonce_bytes =
        bcs::to_bytes(&signed.nonce).map_err(|e| IntentError::Codec(e.to_string()))?;

    let mut message =
        Vec::with_capacity(intent_bytes.len() + sender_bytes.len() + nonce_bytes.len());
    message.extend_from_slice(&intent_bytes);
    message.extend_from_slice(&sender_bytes);
    message.extend_from_slice(&nonce_bytes);

    DilithiumSigner::verify(
        &signed.sender_pk,
        crate::types::INTENT_DOMAIN,
        &message,
        &signed.signature,
    )
    .map_err(|source| IntentError::InvalidSignature {
        sender: signed.sender,
        source,
    })
}

/// Validate per-variant field constraints.
fn validate_intent_fields(intent: &UserIntent, config: &IntentConfig) -> IntentResult<()> {
    match intent {
        UserIntent::Transfer { amount, .. } => {
            if *amount == Amount::ZERO {
                return Err(IntentError::ParseError {
                    reason: "transfer amount must be > 0".into(),
                });
            }
        }
        UserIntent::Swap {
            amount,
            max_slippage_bps,
            ..
        } => {
            if *amount == Amount::ZERO {
                return Err(IntentError::ParseError {
                    reason: "swap amount must be > 0".into(),
                });
            }
            if *max_slippage_bps > 10_000 {
                return Err(IntentError::ParseError {
                    reason: format!("slippage {} bps exceeds 100% (10000 bps)", max_slippage_bps),
                });
            }
        }
        UserIntent::ContractCall {
            function,
            gas_budget,
            ..
        } => {
            if function.is_empty() {
                return Err(IntentError::ParseError {
                    reason: "contract call function name must not be empty".into(),
                });
            }
            if *gas_budget == 0 {
                return Err(IntentError::ParseError {
                    reason: "contract call gas_budget must be > 0".into(),
                });
            }
        }
        UserIntent::Stake { amount, .. } => {
            if *amount == Amount::ZERO {
                return Err(IntentError::ParseError {
                    reason: "stake amount must be > 0".into(),
                });
            }
        }
        UserIntent::AgentTask { spec } => {
            validate_agent_spec(spec, config)?;
        }
    }
    Ok(())
}

/// Validate agent-specific constraints.
fn validate_agent_spec(
    spec: &crate::types::AgentIntentSpec,
    config: &IntentConfig,
) -> IntentResult<()> {
    // Check protocol version prefix.
    if !spec.version.starts_with("nap/") {
        return Err(IntentError::AgentSpecError {
            reason: format!("unsupported agent protocol version: {}", spec.version),
        });
    }

    // Gas budget within agent limits.
    if spec.constraints.max_gas > config.agent_max_gas_budget {
        return Err(IntentError::GasBudgetExceeded {
            estimated: spec.constraints.max_gas,
            budget: config.agent_max_gas_budget,
        });
    }

    // Per-action value within limits.
    if spec.constraints.max_value.0 > config.agent_max_value_per_action.0 {
        return Err(IntentError::AgentValueLimitExceeded {
            value: spec.constraints.max_value.0,
            limit: config.agent_max_value_per_action.0,
        });
    }

    // Validate task structure.
    match &spec.task {
        AgentTask::SingleAction { action } => {
            validate_intent_fields(action, config)?;
        }
        AgentTask::MultiStep {
            steps,
            execution_order,
        } => {
            if steps.is_empty() {
                return Err(IntentError::AgentSpecError {
                    reason: "agent multi-step task must have at least one step".into(),
                });
            }
            // All indices in execution_order must be valid step indices.
            for level in execution_order {
                for &idx in level {
                    if idx >= steps.len() {
                        return Err(IntentError::AgentSpecError {
                            reason: format!(
                                "execution_order index {} out of range (steps.len = {})",
                                idx,
                                steps.len()
                            ),
                        });
                    }
                }
            }
            for step in steps {
                validate_intent_fields(step, config)?;
            }
        }
    }

    // Approval threshold sanity.
    if let HumanApproval::RequireConfirmation { threshold_value } = &spec.human_approval {
        if *threshold_value == Amount::ZERO {
            return Err(IntentError::AgentSpecError {
                reason: "human approval threshold must be > 0".into(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentConstraints, AgentIntentSpec, AgentTask, HumanApproval};
    use nexus_crypto::DilithiumSigner;
    use nexus_primitives::{
        AccountAddress, Amount, ContractAddress, TimestampMs, TokenId, ValidatorIndex,
    };

    /// Helper: create a properly signed intent for testing.
    fn make_signed_intent(intent: UserIntent) -> SignedUserIntent {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sender = AccountAddress([0xAA; 32]);
        let nonce = 1u64;

        let digest = compute_intent_digest(&intent, &sender, nonce).unwrap();

        // Build message bytes the same way check_signature does.
        let intent_bytes = bcs::to_bytes(&intent).unwrap();
        let sender_bytes = bcs::to_bytes(&sender).unwrap();
        let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
        let mut message = Vec::new();
        message.extend_from_slice(&intent_bytes);
        message.extend_from_slice(&sender_bytes);
        message.extend_from_slice(&nonce_bytes);

        let signature = DilithiumSigner::sign(&sk, crate::types::INTENT_DOMAIN, &message);

        SignedUserIntent {
            intent,
            sender,
            signature,
            sender_pk: vk,
            nonce,
            created_at: TimestampMs(1_000_000),
            digest,
        }
    }

    fn default_config() -> IntentConfig {
        IntentConfig::default()
    }

    fn transfer_intent() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        }
    }

    // ── Happy path ──────────────────────────────────────────────

    #[test]
    fn valid_transfer_passes() {
        let signed = make_signed_intent(transfer_intent());
        assert!(validate_signed_intent(&signed, &default_config()).is_ok());
    }

    #[test]
    fn valid_swap_passes() {
        let intent = UserIntent::Swap {
            from_token: TokenId::Native,
            to_token: TokenId::Contract(ContractAddress([0xCC; 32])),
            amount: Amount(500),
            max_slippage_bps: 50,
        };
        let signed = make_signed_intent(intent);
        assert!(validate_signed_intent(&signed, &default_config()).is_ok());
    }

    #[test]
    fn valid_contract_call_passes() {
        let intent = UserIntent::ContractCall {
            contract: ContractAddress([0xDD; 32]),
            function: "transfer".to_string(),
            args: vec![vec![1, 2, 3]],
            gas_budget: 50_000,
        };
        let signed = make_signed_intent(intent);
        assert!(validate_signed_intent(&signed, &default_config()).is_ok());
    }

    #[test]
    fn valid_stake_passes() {
        let intent = UserIntent::Stake {
            validator: ValidatorIndex(0),
            amount: Amount(10_000),
        };
        let signed = make_signed_intent(intent);
        assert!(validate_signed_intent(&signed, &default_config()).is_ok());
    }

    // ── Signature failures ──────────────────────────────────────

    #[test]
    fn wrong_signature_fails() {
        let mut signed = make_signed_intent(transfer_intent());
        // Replace with a different keypair's signature.
        let (sk2, _) = DilithiumSigner::generate_keypair();
        signed.signature = DilithiumSigner::sign(&sk2, crate::types::INTENT_DOMAIN, b"wrong");
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::InvalidSignature { .. })));
    }

    // ── Digest mismatch ─────────────────────────────────────────

    #[test]
    fn digest_mismatch_fails() {
        let mut signed = make_signed_intent(transfer_intent());
        signed.digest = nexus_primitives::Blake3Digest([0xFF; 32]);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    // ── Field validation ────────────────────────────────────────

    #[test]
    fn zero_transfer_amount_rejected() {
        let intent = UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount::ZERO,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    #[test]
    fn zero_swap_amount_rejected() {
        let intent = UserIntent::Swap {
            from_token: TokenId::Native,
            to_token: TokenId::Contract(ContractAddress([0xCC; 32])),
            amount: Amount::ZERO,
            max_slippage_bps: 50,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    #[test]
    fn excessive_slippage_rejected() {
        let intent = UserIntent::Swap {
            from_token: TokenId::Native,
            to_token: TokenId::Contract(ContractAddress([0xCC; 32])),
            amount: Amount(500),
            max_slippage_bps: 10_001,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    #[test]
    fn empty_function_name_rejected() {
        let intent = UserIntent::ContractCall {
            contract: ContractAddress([0xDD; 32]),
            function: String::new(),
            args: vec![],
            gas_budget: 50_000,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    #[test]
    fn zero_gas_budget_rejected() {
        let intent = UserIntent::ContractCall {
            contract: ContractAddress([0xDD; 32]),
            function: "transfer".to_string(),
            args: vec![],
            gas_budget: 0,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    #[test]
    fn zero_stake_amount_rejected() {
        let intent = UserIntent::Stake {
            validator: ValidatorIndex(0),
            amount: Amount::ZERO,
        };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::ParseError { .. })));
    }

    // ── Agent validation ────────────────────────────────────────

    fn basic_agent_spec() -> AgentIntentSpec {
        AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id: AccountAddress([0x01; 32]),
            capability_token: TokenId::Native,
            task: AgentTask::SingleAction {
                action: Box::new(transfer_intent()),
            },
            constraints: AgentConstraints {
                max_gas: 100_000,
                max_value: Amount(10_000),
                allowed_contracts: vec![],
                deadline: TimestampMs(9_999_999),
            },
            human_approval: HumanApproval::PreApproved,
        }
    }

    #[test]
    fn valid_agent_task_passes() {
        let intent = UserIntent::AgentTask {
            spec: basic_agent_spec(),
        };
        let signed = make_signed_intent(intent);
        assert!(validate_signed_intent(&signed, &default_config()).is_ok());
    }

    #[test]
    fn bad_agent_version_rejected() {
        let mut spec = basic_agent_spec();
        spec.version = "unknown/2.0".to_string();
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::AgentSpecError { .. })));
    }

    #[test]
    fn agent_gas_over_limit_rejected() {
        let mut spec = basic_agent_spec();
        spec.constraints.max_gas = 99_000_000; // exceeds default 1M
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::GasBudgetExceeded { .. })));
    }

    #[test]
    fn agent_value_over_limit_rejected() {
        let mut spec = basic_agent_spec();
        spec.constraints.max_value = Amount(99_000_000); // exceeds default 1M
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(
            err,
            Err(IntentError::AgentValueLimitExceeded { .. })
        ));
    }

    #[test]
    fn agent_empty_multistep_rejected() {
        let mut spec = basic_agent_spec();
        spec.task = AgentTask::MultiStep {
            steps: vec![],
            execution_order: vec![],
        };
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::AgentSpecError { .. })));
    }

    #[test]
    fn agent_bad_execution_order_index_rejected() {
        let mut spec = basic_agent_spec();
        spec.task = AgentTask::MultiStep {
            steps: vec![transfer_intent()],
            execution_order: vec![vec![0, 5]], // 5 is out of range
        };
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::AgentSpecError { .. })));
    }

    #[test]
    fn agent_zero_approval_threshold_rejected() {
        let mut spec = basic_agent_spec();
        spec.human_approval = HumanApproval::RequireConfirmation {
            threshold_value: Amount::ZERO,
        };
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed_intent(intent);
        let err = validate_signed_intent(&signed, &default_config());
        assert!(matches!(err, Err(IntentError::AgentSpecError { .. })));
    }
}
