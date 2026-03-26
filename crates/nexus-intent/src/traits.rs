// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent layer trait contracts.
//!
//! These traits define the boundaries of the intent layer.
//! Implementors are provided in sibling modules; contract tests
//! appear at the bottom of this module.
//!
//! # Stability
//!
//! | Trait | Level |
//! |---|---|
//! | [`IntentCompiler`] | **STABLE** — changes require architecture review + RFC |
//! | [`AccountResolver`] | **STABLE** — changes require architecture review + RFC |

use crate::error::IntentResult;
use crate::types::{
    CompiledIntentPlan, ContractLocation, GasEstimate, SignedUserIntent, UserIntent,
};
use nexus_primitives::{AccountAddress, Amount, ContractAddress, ShardId, TokenId};

// ── IntentCompiler ──────────────────────────────────────────────────────

/// **\[STABLE\]** Core compilation trait — takes a signed user intent
/// and the current account state, then produces a concrete execution plan
/// with ordered transaction steps and DAG dependencies.
///
/// Implementors:
/// - `IntentCompilerImpl` (standard compiler, T-3002/T-3003)
///
/// # Contract
///
/// 1. Compilation is **deterministic**: same `(intent, state)` → identical
///    `CompiledIntentPlan`.
/// 2. The output plan contains at most `config.max_steps_per_intent` steps.
/// 3. Gas estimation must be conservative — never under-estimate.
/// 4. Cross-shard plans set `requires_htlc = true`.
/// 5. Invalid intents return an appropriate `IntentError`, never panic.
#[allow(async_fn_in_trait)]
pub trait IntentCompiler: Send + Sync + 'static {
    /// The account resolver type used during compilation.
    type Resolver: AccountResolver;

    /// Compile a signed user intent into a concrete execution plan.
    ///
    /// The `resolver` is used to look up account balances, primary shards,
    /// and contract locations during compilation.
    ///
    /// # Errors
    ///
    /// Returns `IntentError` on validation failure, routing failure,
    /// or if the intent cannot be compiled within configured constraints.
    fn compile(
        &self,
        intent: &SignedUserIntent,
        resolver: &Self::Resolver,
    ) -> impl std::future::Future<Output = IntentResult<CompiledIntentPlan>> + Send;

    /// Estimate the gas cost of executing an intent (simulation only).
    ///
    /// This performs a dry-run without modifying state.  The estimate
    /// should be conservative to avoid out-of-gas failures during
    /// real execution.
    ///
    /// # Errors
    ///
    /// Returns `IntentError` if the intent cannot be simulated.
    fn estimate_gas(
        &self,
        intent: &UserIntent,
        resolver: &Self::Resolver,
    ) -> impl std::future::Future<Output = IntentResult<GasEstimate>> + Send;
}

// ── AccountResolver ─────────────────────────────────────────────────────

/// **\[STABLE\]** Cross-shard account and contract resolution.
///
/// Provides a unified view of account balances, shard assignments,
/// and contract locations across the entire network.  The intent
/// compiler uses this trait to plan routing and validate feasibility.
///
/// Implementors:
/// - `AccountResolverImpl` (in-memory + storage-backed, T-3001)
///
/// # Contract
///
/// 1. `balance()` returns the **aggregate** balance across all shards.
/// 2. `primary_shard()` is deterministic for a given account address
///    (Jump Consistent Hash over the shard count).
/// 3. `contract_location()` returns `ContractNotFound` for undeployed
///    contracts, never panics.
/// 4. All methods are async to support remote queries.
pub trait AccountResolver: Send + Sync + 'static {
    /// Aggregate balance across all shards for the given account and token.
    ///
    /// # Errors
    ///
    /// Returns `IntentError::AccountNotFound` if the account does not exist.
    fn balance(
        &self,
        account: &AccountAddress,
        token: &TokenId,
    ) -> impl std::future::Future<Output = IntentResult<Amount>> + Send;

    /// Deterministic primary shard for an account (Jump Consistent Hash).
    ///
    /// # Errors
    ///
    /// Returns `IntentError::AccountNotFound` if the account cannot be mapped.
    fn primary_shard(
        &self,
        account: &AccountAddress,
    ) -> impl std::future::Future<Output = IntentResult<ShardId>> + Send;

    /// Look up the physical deployment location of a contract.
    ///
    /// # Errors
    ///
    /// Returns `IntentError::ContractNotFound` if the contract is not registered.
    fn contract_location(
        &self,
        contract: &ContractAddress,
    ) -> impl std::future::Future<Output = IntentResult<ContractLocation>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify traits require Send + Sync.
    fn _assert_send_sync<T: Send + Sync>() {}
    #[test]
    fn traits_are_send_sync() {
        // These are compile-time checks — if the traits aren't Send+Sync
        // constrained, this won't compile.
        fn _check<T: IntentCompiler>() {
            _assert_send_sync::<T>();
        }
        fn _check2<T: AccountResolver>() {
            _assert_send_sync::<T>();
        }
    }
}
