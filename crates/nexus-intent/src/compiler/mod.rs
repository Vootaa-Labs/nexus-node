// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent compiler — transforms [`SignedUserIntent`] into
//! [`CompiledIntentPlan`].
//!
//! # Sub-modules
//!
//! - [`parser`] — resource extraction from `UserIntent` variants.
//! - [`validator`] — structural + cryptographic pre-checks.
//!
//! The full compilation pipeline is:
//!
//! 1. **Validate** — signature, digest, size, fields.
//! 2. **Parse** — extract required accounts, tokens, contracts.
//! 3. **Resolve** — query `AccountResolver` for balances, shards,
//!    contract locations.
//! 4. **Plan** — build `IntentStep` DAG with dependencies (T-3003).
//! 5. **Emit** — produce `CompiledIntentPlan`.
//!
//! Steps 1–3 are implemented in T-3002. Steps 4–5 will be added in
//! T-3003 (Planner & Cross-Shard DAG).

pub mod optimizer;
pub mod parser;
pub mod planner;
pub mod validator;

use crate::config::IntentConfig;
use crate::error::{IntentError, IntentResult};
use crate::traits::{AccountResolver, IntentCompiler};
use crate::types::{CompiledIntentPlan, GasEstimate, SignedUserIntent, UserIntent};

/// Concrete intent compiler.
///
/// Orchestrates validation → parsing → resolution → planning.
///
/// # Placeholder
///
/// The `compile()` method currently performs validation and resource
/// parsing (Steps 1–3).  The planning step (Step 4) is a stub that
/// will be implemented in T-3003.
pub struct IntentCompilerImpl<R: AccountResolver> {
    config: IntentConfig,
    _marker: std::marker::PhantomData<R>,
}

impl<R: AccountResolver> IntentCompilerImpl<R> {
    /// Create a new compiler with the given configuration.
    pub fn new(config: IntentConfig) -> Self {
        Self {
            config,
            _marker: std::marker::PhantomData,
        }
    }

    /// Access the current configuration.
    pub fn config(&self) -> &IntentConfig {
        &self.config
    }
}

impl<R: AccountResolver> IntentCompiler for IntentCompilerImpl<R> {
    type Resolver = R;

    async fn compile(
        &self,
        intent: &SignedUserIntent,
        resolver: &R,
    ) -> IntentResult<CompiledIntentPlan> {
        // Step 1: Validate.
        validator::validate_signed_intent(intent, &self.config)?;

        // Step 2: Parse resource requirements.
        let resources = parser::parse_resources(intent.sender, &intent.intent)?;

        // Step 3: Resolve — check balances exist and are sufficient.
        self.check_balances(&resources, resolver).await?;

        // Steps 4–5: Plan & Emit.
        planner::plan(intent, &resources, resolver, &self.config).await
    }

    async fn estimate_gas(&self, intent: &UserIntent, resolver: &R) -> IntentResult<GasEstimate> {
        // Parse resources to count shards touched.
        let sender = nexus_primitives::AccountAddress::ZERO;
        let resources = parser::parse_resources(sender, intent)?;

        let mut shards_touched = std::collections::HashSet::new();
        for account in &resources.accounts {
            let shard = resolver.primary_shard(account).await?;
            shards_touched.insert(shard);
        }
        for contract in &resources.contracts {
            let loc = resolver.contract_location(contract).await?;
            shards_touched.insert(loc.shard_id);
        }

        let base_gas = resources.gas_hint.unwrap_or(10_000);
        // Conservative: 2x for cross-shard coordination overhead.
        let multiplier = if shards_touched.len() > 1 { 2 } else { 1 };
        let estimated = base_gas.saturating_mul(multiplier);

        Ok(GasEstimate {
            gas_units: estimated,
            shards_touched: shards_touched.len() as u16,
            requires_cross_shard: shards_touched.len() > 1,
        })
    }
}

impl<R: AccountResolver> IntentCompilerImpl<R> {
    /// Verify that the sender holds sufficient balance for all required tokens.
    async fn check_balances(
        &self,
        resources: &parser::ParsedResources,
        resolver: &R,
    ) -> IntentResult<()> {
        // Aggregate required amounts per token.
        let mut required: std::collections::HashMap<nexus_primitives::TokenId, u64> =
            std::collections::HashMap::new();
        for (token, amount) in &resources.required_balances {
            *required.entry(*token).or_default() = required
                .get(token)
                .copied()
                .unwrap_or(0)
                .saturating_add(amount.0);
        }

        for (token, total_required) in &required {
            let balance = resolver.balance(&resources.sender, token).await?;
            if balance.0 < *total_required {
                return Err(IntentError::InsufficientBalance {
                    account: resources.sender,
                    token: *token,
                    required: *total_required,
                    available: balance.0,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::AccountResolverImpl;
    use crate::types::compute_intent_digest;
    use nexus_crypto::{DilithiumSigner, Signer as _};
    use nexus_primitives::{
        AccountAddress, Amount, ContractAddress, ShardId, TimestampMs, TokenId,
    };

    fn sender() -> AccountAddress {
        AccountAddress([0xAA; 32])
    }

    fn make_signed(intent: UserIntent) -> SignedUserIntent {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let s = sender();
        let nonce = 1u64;
        let digest = compute_intent_digest(&intent, &s, nonce).unwrap();

        let intent_bytes = bcs::to_bytes(&intent).unwrap();
        let sender_bytes = bcs::to_bytes(&s).unwrap();
        let nonce_bytes = bcs::to_bytes(&nonce).unwrap();
        let mut message = Vec::new();
        message.extend_from_slice(&intent_bytes);
        message.extend_from_slice(&sender_bytes);
        message.extend_from_slice(&nonce_bytes);

        let signature = DilithiumSigner::sign(&sk, crate::types::INTENT_DOMAIN, &message);

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

    fn setup_resolver() -> AccountResolverImpl {
        let resolver = AccountResolverImpl::new(16);
        resolver
            .balances()
            .set_balance(sender(), TokenId::Native, Amount(100_000));
        resolver
    }

    fn transfer_intent() -> UserIntent {
        UserIntent::Transfer {
            to: AccountAddress([0xBB; 32]),
            token: TokenId::Native,
            amount: Amount(1_000),
        }
    }

    #[tokio::test]
    async fn compile_produces_plan() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = setup_resolver();
        let signed = make_signed(transfer_intent());
        let plan = compiler.compile(&signed, &resolver).await.unwrap();
        assert_eq!(plan.intent_id, signed.digest);
        assert!(!plan.steps.is_empty());
    }

    #[tokio::test]
    async fn compile_rejects_invalid_signature() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = setup_resolver();
        let mut signed = make_signed(transfer_intent());
        let (sk2, _) = DilithiumSigner::generate_keypair();
        signed.signature = DilithiumSigner::sign(&sk2, crate::types::INTENT_DOMAIN, b"wrong");
        let result = compiler.compile(&signed, &resolver).await;
        assert!(matches!(result, Err(IntentError::InvalidSignature { .. })));
    }

    #[tokio::test]
    async fn compile_rejects_insufficient_balance() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = AccountResolverImpl::new(16);
        // Set balance too low.
        resolver
            .balances()
            .set_balance(sender(), TokenId::Native, Amount(500));
        let signed = make_signed(transfer_intent()); // needs 1000
        let result = compiler.compile(&signed, &resolver).await;
        assert!(matches!(
            result,
            Err(IntentError::InsufficientBalance { .. })
        ));
    }

    #[tokio::test]
    async fn compile_rejects_unknown_account() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = AccountResolverImpl::new(16);
        // No balance set for sender.
        let signed = make_signed(transfer_intent());
        let result = compiler.compile(&signed, &resolver).await;
        assert!(matches!(result, Err(IntentError::AccountNotFound { .. })));
    }

    #[tokio::test]
    async fn estimate_gas_single_shard() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = AccountResolverImpl::new(1); // single shard
        let intent = transfer_intent();
        let estimate = compiler.estimate_gas(&intent, &resolver).await.unwrap();
        // Single shard: no cross-shard multiplier.
        assert_eq!(estimate.shards_touched, 1);
        assert!(!estimate.requires_cross_shard);
    }

    #[tokio::test]
    async fn estimate_gas_contract_call() {
        let compiler = IntentCompilerImpl::<AccountResolverImpl>::new(IntentConfig::default());
        let resolver = AccountResolverImpl::new(16);
        let contract = ContractAddress([0xCC; 32]);
        resolver.contracts().register(
            contract,
            crate::types::ContractLocation {
                shard_id: ShardId(5),
                contract_addr: contract,
                module_name: "my_mod".to_string(),
                verified: false,
            },
        );
        let intent = UserIntent::ContractCall {
            contract,
            function: "run".to_string(),
            args: vec![],
            gas_budget: 50_000,
        };
        let estimate = compiler.estimate_gas(&intent, &resolver).await.unwrap();
        // gas_hint=50_000; sender on shard X, contract on shard 5 → likely cross-shard (2x).
        assert!(estimate.gas_units >= 50_000);
        assert!(estimate.shards_touched >= 1);
    }
}
