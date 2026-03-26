// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent planner — converts parsed resources + resolved state into
//! a [`CompiledIntentPlan`] with ordered [`IntentStep`]s.
//!
//! The planner is responsible for:
//! - Mapping [`UserIntent`] variants to [`TransactionPayload`]s.
//! - Resolving shard assignments via [`AccountResolver`].
//! - Detecting cross-shard scenarios and setting dependency DAG edges.
//! - Producing the final [`CompiledIntentPlan`].
//!
//! # Determinism
//!
//! For a given `(intent, resolved_state)` input, the planner always
//! produces an identical output — same steps, same ordering, same gas
//! estimates.

use crate::compiler::parser::ParsedResources;
use crate::config::IntentConfig;
use crate::error::{IntentError, IntentResult};
use crate::traits::AccountResolver;
use crate::types::{AgentTask, CompiledIntentPlan, IntentStep, SignedUserIntent, UserIntent};
use nexus_execution::types::{compute_lock_hash, TransactionBody, TransactionPayload};
use nexus_primitives::{AccountAddress, EpochNumber, ShardId, TokenId};
use std::collections::HashSet;

/// Default chain ID for compiled transactions.
const DEFAULT_CHAIN_ID: u64 = 1;

/// Default gas price for compiled transactions.
const DEFAULT_GAS_PRICE: u64 = 1;

/// Default expiry epoch offset (current + 100).
const DEFAULT_EXPIRY_OFFSET: u64 = 100;

/// Plan the execution steps for a validated, parsed intent.
///
/// # Arguments
///
/// - `signed` — the original signed intent (provides sender, nonce, pk).
/// - `resources` — parsed resource requirements from the parser.
/// - `resolver` — account/contract resolution.
/// - `config` — compiler configuration (limits, etc.).
///
/// # Returns
///
/// A [`CompiledIntentPlan`] with concrete `IntentStep`s, or an
/// `IntentError` if planning fails (e.g., too many steps, no route).
pub async fn plan<R: AccountResolver>(
    signed: &SignedUserIntent,
    _resources: &ParsedResources,
    resolver: &R,
    config: &IntentConfig,
) -> IntentResult<CompiledIntentPlan> {
    // Resolve sender's primary shard.
    let sender_shard = resolver.primary_shard(&signed.sender).await?;

    // Build steps from the intent.
    let steps = build_steps(
        &signed.intent,
        signed.sender,
        sender_shard,
        signed.nonce,
        resolver,
        config.htlc_timeout_epochs,
    )
    .await?;

    // Enforce step limit.
    if steps.len() > config.max_steps_per_intent {
        return Err(IntentError::TooManySteps {
            steps: steps.len(),
            max: config.max_steps_per_intent,
        });
    }

    // Detect cross-shard.
    let shards: HashSet<ShardId> = steps.iter().map(|s| s.shard_id).collect();
    let requires_htlc = shards.len() > 1;

    // Sum gas.
    let estimated_gas: u64 = steps.iter().map(|s| s.body.gas_limit).sum();

    // Convert to IntentSteps with proper SignedTransaction wrappers.
    let intent_steps = steps
        .into_iter()
        .enumerate()
        .map(|(i, planned)| build_intent_step(planned, signed, i))
        .collect::<IntentResult<Vec<_>>>()?;

    Ok(CompiledIntentPlan {
        intent_id: signed.digest,
        steps: intent_steps,
        requires_htlc,
        estimated_gas,
        expires_at: EpochNumber(DEFAULT_EXPIRY_OFFSET),
    })
}

// ── Internal types ──────────────────────────────────────────────────────

/// Intermediate step before final signing.
struct PlannedStep {
    shard_id: ShardId,
    body: TransactionBody,
    depends_on: Vec<usize>,
}

/// Build planned steps from a UserIntent.
fn build_steps<'a, R: AccountResolver>(
    intent: &'a UserIntent,
    sender: AccountAddress,
    sender_shard: ShardId,
    base_nonce: u64,
    resolver: &'a R,
    htlc_timeout_epochs: u64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = IntentResult<Vec<PlannedStep>>> + Send + 'a>>
{
    Box::pin(async move {
        let mut steps = Vec::new();

        match intent {
            UserIntent::Transfer { to, token, amount } => {
                let recipient_shard = resolver.primary_shard(to).await?;

                if recipient_shard == sender_shard {
                    // Same-shard: simple transfer as before.
                    let payload = TransactionPayload::Transfer {
                        recipient: *to,
                        amount: *amount,
                        token: *token,
                    };
                    steps.push(PlannedStep {
                        shard_id: sender_shard,
                        body: make_body(sender, base_nonce, sender_shard, payload, 10_000),
                        depends_on: vec![],
                    });
                } else {
                    // Cross-shard: emit HtlcLock on sender_shard, HtlcClaim on recipient_shard.
                    //
                    // Generate a deterministic preimage from the intent sender + nonce + recipient.
                    // In production a client would supply a secret preimage; here we derive one
                    // so the compiled plan is self-contained for automated claims.
                    let preimage = generate_htlc_preimage(sender, base_nonce, *to);
                    let lock_hash = compute_lock_hash(&preimage);
                    let timeout_epoch =
                        EpochNumber(DEFAULT_EXPIRY_OFFSET.saturating_add(htlc_timeout_epochs));

                    let lock_payload = TransactionPayload::HtlcLock {
                        recipient: *to,
                        amount: *amount,
                        target_shard: recipient_shard,
                        lock_hash,
                        timeout_epoch,
                    };
                    steps.push(PlannedStep {
                        shard_id: sender_shard,
                        body: make_body(sender, base_nonce, sender_shard, lock_payload, 20_000),
                        depends_on: vec![],
                    });

                    // The claim step uses a placeholder lock_digest — the actual lock_digest
                    // is the tx digest of the lock transaction, which is computed when converting
                    // PlannedSteps into IntentSteps.  The runtime execution bridge or claim
                    // submitter is responsible for filling the correct lock_digest.
                    // For now we embed the preimage so the claim can be auto-submitted.
                    let claim_payload = TransactionPayload::HtlcClaim {
                        lock_digest: nexus_primitives::Blake3Digest([0u8; 32]), // placeholder
                        preimage,
                    };
                    steps.push(PlannedStep {
                        shard_id: recipient_shard,
                        body: make_body(
                            sender,
                            base_nonce.wrapping_add(1),
                            recipient_shard,
                            claim_payload,
                            20_000,
                        ),
                        depends_on: vec![0], // claim depends on lock
                    });
                }
            }

            UserIntent::Swap {
                from_token, amount, ..
            } => {
                // Swap executes on the sender's shard (AMM is co-located or routed).
                let payload = TransactionPayload::MoveCall {
                    contract: nexus_primitives::ContractAddress::ZERO,
                    function: "swap".to_string(),
                    type_args: vec![],
                    args: vec![
                        bcs::to_bytes(from_token).map_err(|e| IntentError::Codec(e.to_string()))?,
                        bcs::to_bytes(amount).map_err(|e| IntentError::Codec(e.to_string()))?,
                    ],
                };
                steps.push(PlannedStep {
                    shard_id: sender_shard,
                    body: make_body(sender, base_nonce, sender_shard, payload, 50_000),
                    depends_on: vec![],
                });
            }

            UserIntent::ContractCall {
                contract,
                function,
                args,
                gas_budget,
            } => {
                let location = resolver.contract_location(contract).await?;
                let contract_shard = location.shard_id;
                let payload = TransactionPayload::MoveCall {
                    contract: *contract,
                    function: function.clone(),
                    type_args: vec![],
                    args: args.clone(),
                };

                if contract_shard == sender_shard {
                    // Single-shard call.
                    steps.push(PlannedStep {
                        shard_id: sender_shard,
                        body: make_body(sender, base_nonce, sender_shard, payload, *gas_budget),
                        depends_on: vec![],
                    });
                } else {
                    // Cross-shard: debit on sender shard, call on contract shard.
                    let debit_payload = TransactionPayload::Transfer {
                        recipient: AccountAddress::ZERO, // Escrow placeholder.
                        amount: nexus_primitives::Amount::ZERO,
                        token: TokenId::Native,
                    };
                    steps.push(PlannedStep {
                        shard_id: sender_shard,
                        body: make_body(sender, base_nonce, sender_shard, debit_payload, 5_000),
                        depends_on: vec![],
                    });
                    steps.push(PlannedStep {
                        shard_id: contract_shard,
                        body: make_body(
                            sender,
                            base_nonce.wrapping_add(1),
                            contract_shard,
                            payload,
                            *gas_budget,
                        ),
                        depends_on: vec![0],
                    });
                }
            }

            UserIntent::Stake { validator, amount } => {
                // Staking executes on the sender's shard.
                let payload = TransactionPayload::MoveCall {
                    contract: nexus_primitives::ContractAddress::ZERO,
                    function: "stake".to_string(),
                    type_args: vec![],
                    args: vec![
                        bcs::to_bytes(validator).map_err(|e| IntentError::Codec(e.to_string()))?,
                        bcs::to_bytes(amount).map_err(|e| IntentError::Codec(e.to_string()))?,
                    ],
                };
                steps.push(PlannedStep {
                    shard_id: sender_shard,
                    body: make_body(sender, base_nonce, sender_shard, payload, 20_000),
                    depends_on: vec![],
                });
            }

            UserIntent::AgentTask { spec } => {
                match &spec.task {
                    AgentTask::SingleAction { action } => {
                        let mut sub = build_steps(
                            action,
                            sender,
                            sender_shard,
                            base_nonce,
                            resolver,
                            htlc_timeout_epochs,
                        )
                        .await?;
                        steps.append(&mut sub);
                    }
                    AgentTask::MultiStep {
                        steps: task_steps,
                        execution_order,
                    } => {
                        let mut nonce = base_nonce;
                        let mut level_starts: Vec<usize> = Vec::new();

                        for level in execution_order {
                            let level_start = steps.len();
                            level_starts.push(level_start);

                            for &step_idx in level {
                                if step_idx >= task_steps.len() {
                                    continue;
                                }
                                let sub = build_steps(
                                    &task_steps[step_idx],
                                    sender,
                                    sender_shard,
                                    nonce,
                                    resolver,
                                    htlc_timeout_epochs,
                                )
                                .await?;
                                nonce = nonce.wrapping_add(sub.len() as u64);

                                // Steps in this level depend on all steps from previous level.
                                let deps: Vec<usize> = if level_starts.len() > 1 {
                                    let prev_start = level_starts[level_starts.len() - 2];
                                    (prev_start..level_start).collect()
                                } else {
                                    vec![]
                                };

                                for mut s in sub {
                                    s.depends_on.extend(deps.iter());
                                    steps.push(s);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(steps)
    })
}

/// Build a TransactionBody.
fn make_body(
    sender: AccountAddress,
    nonce: u64,
    target_shard: ShardId,
    payload: TransactionPayload,
    gas_limit: u64,
) -> TransactionBody {
    TransactionBody {
        sender,
        sequence_number: nonce,
        expiry_epoch: EpochNumber(DEFAULT_EXPIRY_OFFSET),
        gas_limit,
        gas_price: DEFAULT_GAS_PRICE,
        target_shard: Some(target_shard),
        payload,
        chain_id: DEFAULT_CHAIN_ID,
    }
}

/// Generate a deterministic HTLC preimage from sender, nonce, and recipient.
///
/// The preimage is `BLAKE3("nexus::htlc::preimage::v1" ‖ sender ‖ nonce_le ‖ recipient)`.
/// This allows the intent compiler to produce self-contained plans that a
/// claim submitter can execute without external coordination.
fn generate_htlc_preimage(
    sender: AccountAddress,
    nonce: u64,
    recipient: AccountAddress,
) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nexus::htlc::preimage::v1");
    hasher.update(sender.as_ref());
    hasher.update(&nonce.to_le_bytes());
    hasher.update(recipient.as_ref());
    hasher.finalize().as_bytes().to_vec()
}

/// Convert a PlannedStep into a signed IntentStep.
///
/// Uses the original intent's sender_pk and creates a digest.
/// The signature is forwarded from the original intent (the consensus
/// layer will verify the intent-level signature, not per-step).
fn build_intent_step(
    planned: PlannedStep,
    signed: &SignedUserIntent,
    _step_index: usize,
) -> IntentResult<IntentStep> {
    let digest = nexus_execution::types::compute_tx_digest(&planned.body)
        .map_err(|e| IntentError::Internal(format!("tx digest computation failed: {e}")))?;

    let tx = nexus_execution::types::SignedTransaction {
        body: planned.body,
        signature: signed.signature.clone(),
        sender_pk: signed.sender_pk.clone(),
        digest,
    };

    Ok(IntentStep {
        shard_id: planned.shard_id,
        transaction: tx,
        depends_on: planned.depends_on,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::parser;
    use crate::config::IntentConfig;
    use crate::resolver::AccountResolverImpl;
    use crate::types::{
        compute_intent_digest, AgentConstraints, AgentIntentSpec, AgentTask, ContractLocation,
        HumanApproval, SignedUserIntent,
    };
    use nexus_crypto::{DilithiumSigner, Signer as _};
    use nexus_primitives::{
        AccountAddress, Amount, ContractAddress, ShardId, TimestampMs, TokenId, ValidatorIndex,
    };

    fn sender() -> AccountAddress {
        AccountAddress([0xAA; 32])
    }
    fn recipient() -> AccountAddress {
        AccountAddress([0xBB; 32])
    }
    fn contract_addr() -> ContractAddress {
        ContractAddress([0xCC; 32])
    }

    fn make_signed(intent: UserIntent) -> SignedUserIntent {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let s = sender();
        let nonce = 1u64;
        let digest = compute_intent_digest(&intent, &s, nonce).unwrap();
        let ib = bcs::to_bytes(&intent).unwrap();
        let sb = bcs::to_bytes(&s).unwrap();
        let nb = bcs::to_bytes(&nonce).unwrap();
        let mut msg = Vec::new();
        msg.extend_from_slice(&ib);
        msg.extend_from_slice(&sb);
        msg.extend_from_slice(&nb);
        let signature = DilithiumSigner::sign(&sk, crate::types::INTENT_DOMAIN, &msg);
        SignedUserIntent {
            intent,
            sender: s,
            signature,
            sender_pk: vk,
            nonce,
            created_at: TimestampMs(1_000_000),
            digest,
        }
    }

    fn setup_resolver(shard_count: u16) -> AccountResolverImpl {
        let resolver = AccountResolverImpl::new(shard_count);
        resolver
            .balances()
            .set_balance(sender(), TokenId::Native, Amount(1_000_000));
        resolver
    }

    fn config() -> IntentConfig {
        IntentConfig::default()
    }

    // ── Single-shard transfer ───────────────────────────────────

    #[tokio::test]
    async fn plan_single_shard_transfer() {
        let resolver = setup_resolver(1); // single shard → no cross-shard
        let intent = UserIntent::Transfer {
            to: recipient(),
            token: TokenId::Native,
            amount: Amount(1_000),
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan.intent_id, signed.digest);
        assert_eq!(plan.steps.len(), 1);
        assert!(!plan.requires_htlc);
        assert!(plan.estimated_gas > 0);
        assert_eq!(plan.steps[0].shard_id, ShardId(0));
        assert!(plan.steps[0].depends_on.is_empty());
    }

    // ── Cross-shard transfer ────────────────────────────────────

    #[tokio::test]
    async fn plan_cross_shard_transfer() {
        let resolver = setup_resolver(64); // many shards → likely cross-shard
        let intent = UserIntent::Transfer {
            to: recipient(),
            token: TokenId::Native,
            amount: Amount(1_000),
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        // Find if sender and recipient are on different shards.
        let sender_shard = resolver.primary_shard(&sender()).await.unwrap();
        let recip_shard = resolver.primary_shard(&recipient()).await.unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        if sender_shard != recip_shard {
            // Cross-shard: 2 steps with dependency.
            assert_eq!(plan.steps.len(), 2);
            assert!(plan.requires_htlc);
            assert!(plan.steps[0].depends_on.is_empty());
            assert_eq!(plan.steps[1].depends_on, vec![0]);
            assert_eq!(plan.steps[0].shard_id, sender_shard);
            assert_eq!(plan.steps[1].shard_id, recip_shard);
        } else {
            assert_eq!(plan.steps.len(), 1);
            assert!(!plan.requires_htlc);
        }
    }

    // ── Contract call (same shard) ──────────────────────────────

    #[tokio::test]
    async fn plan_contract_call_same_shard() {
        let resolver = setup_resolver(1); // single shard
        resolver.contracts().register(
            contract_addr(),
            ContractLocation {
                shard_id: ShardId(0),
                contract_addr: contract_addr(),
                module_name: "my_mod".to_string(),
                verified: false,
            },
        );
        let intent = UserIntent::ContractCall {
            contract: contract_addr(),
            function: "run".to_string(),
            args: vec![vec![1, 2]],
            gas_budget: 50_000,
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert!(!plan.requires_htlc);
        assert_eq!(plan.estimated_gas, 50_000);
    }

    // ── Contract call (cross-shard) ─────────────────────────────

    #[tokio::test]
    async fn plan_contract_call_cross_shard() {
        let resolver = setup_resolver(64);
        let sender_shard = resolver.primary_shard(&sender()).await.unwrap();
        // Put contract on a different shard.
        let other_shard = if sender_shard.0 == 0 {
            ShardId(1)
        } else {
            ShardId(0)
        };
        resolver.contracts().register(
            contract_addr(),
            ContractLocation {
                shard_id: other_shard,
                contract_addr: contract_addr(),
                module_name: "remote_mod".to_string(),
                verified: false,
            },
        );
        let intent = UserIntent::ContractCall {
            contract: contract_addr(),
            function: "run".to_string(),
            args: vec![],
            gas_budget: 40_000,
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan.steps.len(), 2);
        assert!(plan.requires_htlc);
        assert_eq!(plan.steps[0].shard_id, sender_shard);
        assert_eq!(plan.steps[1].shard_id, other_shard);
        assert_eq!(plan.steps[1].depends_on, vec![0]);
    }

    // ── Swap ────────────────────────────────────────────────────

    #[tokio::test]
    async fn plan_swap() {
        let resolver = setup_resolver(1);
        let intent = UserIntent::Swap {
            from_token: TokenId::Native,
            to_token: TokenId::Contract(ContractAddress([0x01; 32])),
            amount: Amount(500),
            max_slippage_bps: 100,
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert!(!plan.requires_htlc);
    }

    // ── Stake ───────────────────────────────────────────────────

    #[tokio::test]
    async fn plan_stake() {
        let resolver = setup_resolver(1);
        let intent = UserIntent::Stake {
            validator: ValidatorIndex(0),
            amount: Amount(10_000),
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert!(!plan.requires_htlc);
        assert_eq!(plan.estimated_gas, 20_000);
    }

    // ── Too many steps ──────────────────────────────────────────

    #[tokio::test]
    async fn plan_too_many_steps_rejected() {
        let resolver = setup_resolver(1);
        let mut cfg = config();
        cfg.max_steps_per_intent = 1;

        // Multi-step agent task that produces > 1 step.
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id: AccountAddress([0x01; 32]),
            capability_token: TokenId::Native,
            task: AgentTask::MultiStep {
                steps: vec![
                    UserIntent::Transfer {
                        to: recipient(),
                        token: TokenId::Native,
                        amount: Amount(10),
                    },
                    UserIntent::Stake {
                        validator: ValidatorIndex(0),
                        amount: Amount(20),
                    },
                ],
                execution_order: vec![vec![0], vec![1]],
            },
            constraints: AgentConstraints {
                max_gas: 100_000,
                max_value: Amount(100),
                allowed_contracts: vec![],
                deadline: TimestampMs(9_999_999),
            },
            human_approval: HumanApproval::PreApproved,
        };
        let intent = UserIntent::AgentTask { spec };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let err = plan(&signed, &resources, &resolver, &cfg).await;
        assert!(matches!(err, Err(IntentError::TooManySteps { .. })));
    }

    // ── Determinism ─────────────────────────────────────────────

    #[tokio::test]
    async fn plan_is_deterministic() {
        let resolver = setup_resolver(16);
        let intent = UserIntent::Transfer {
            to: recipient(),
            token: TokenId::Native,
            amount: Amount(1_000),
        };
        let signed = make_signed(intent);
        let resources = parser::parse_resources(signed.sender, &signed.intent).unwrap();

        let plan1 = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();
        let plan2 = plan(&signed, &resources, &resolver, &config())
            .await
            .unwrap();

        assert_eq!(plan1.steps.len(), plan2.steps.len());
        assert_eq!(plan1.requires_htlc, plan2.requires_htlc);
        assert_eq!(plan1.estimated_gas, plan2.estimated_gas);
        for (s1, s2) in plan1.steps.iter().zip(plan2.steps.iter()) {
            assert_eq!(s1.shard_id, s2.shard_id);
            assert_eq!(s1.depends_on, s2.depends_on);
        }
    }
}
