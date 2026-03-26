// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Intent resource parser — extracts accounts, tokens, contracts,
//! and gas budgets needed to compile a [`UserIntent`].
//!
//! The [`ParsedResources`] struct is used by the planner (T-3003) to
//! query the [`AccountResolver`] for balances, shards, and contract
//! locations, and to determine whether cross-shard coordination is needed.

use crate::error::IntentResult;
use crate::types::{AgentTask, UserIntent};
use nexus_primitives::{AccountAddress, Amount, ContractAddress, TokenId};

/// Resources that a user intent requires for compilation.
///
/// Extracted by [`parse_resources`] and consumed by the planner.
#[derive(Debug, Clone)]
pub struct ParsedResources {
    /// Sender's account (from `SignedUserIntent`).
    pub sender: AccountAddress,

    /// Tokens the sender must hold (token → minimum amount).
    pub required_balances: Vec<(TokenId, Amount)>,

    /// Accounts that need shard resolution (includes sender and recipients).
    pub accounts: Vec<AccountAddress>,

    /// Contracts that need location resolution.
    pub contracts: Vec<ContractAddress>,

    /// Total gas budget hinted by the intent.
    /// `None` means the compiler should estimate.
    pub gas_hint: Option<u64>,
}

/// Parse the resource requirements out of a user intent.
///
/// This is a purely structural analysis — no network I/O or state
/// queries.  The output feeds into the planner's resolution phase.
///
/// # Errors
///
/// Returns `IntentError::ParseError` for structurally invalid intents
/// that slipped past field validation (e.g., agent spec internal
/// inconsistencies).
pub fn parse_resources(
    sender: AccountAddress,
    intent: &UserIntent,
) -> IntentResult<ParsedResources> {
    let mut resources = ParsedResources {
        sender,
        required_balances: Vec::new(),
        accounts: vec![sender],
        contracts: Vec::new(),
        gas_hint: None,
    };

    collect_resources(intent, &mut resources)?;

    // Dedup accounts and contracts (no Ord, so use retain with seen set).
    dedup_in_place(&mut resources.accounts);
    dedup_in_place(&mut resources.contracts);

    Ok(resources)
}

/// Remove duplicates from a Vec in O(n²) — fine for small lists.
fn dedup_in_place<T: PartialEq>(v: &mut Vec<T>) {
    let mut i = 0;
    while i < v.len() {
        if v[..i].iter().any(|prev| *prev == v[i]) {
            v.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

/// Recursively collect resources from an intent (handles agent multi-step).
fn collect_resources(intent: &UserIntent, res: &mut ParsedResources) -> IntentResult<()> {
    match intent {
        UserIntent::Transfer {
            to, token, amount, ..
        } => {
            res.required_balances.push((*token, *amount));
            res.accounts.push(*to);
        }
        UserIntent::Swap {
            from_token, amount, ..
        } => {
            res.required_balances.push((*from_token, *amount));
            // Swap doesn't add external accounts — AMM is contract-level.
        }
        UserIntent::ContractCall {
            contract,
            gas_budget,
            ..
        } => {
            res.contracts.push(*contract);
            res.gas_hint = Some(res.gas_hint.unwrap_or(0).saturating_add(*gas_budget));
        }
        UserIntent::Stake { amount, .. } => {
            res.required_balances.push((TokenId::Native, *amount));
        }
        UserIntent::AgentTask { spec } => {
            match &spec.task {
                AgentTask::SingleAction { action } => {
                    collect_resources(action, res)?;
                }
                AgentTask::MultiStep { steps, .. } => {
                    for step in steps {
                        collect_resources(step, res)?;
                    }
                }
            }
            // Agent also needs to hold the capability token.
            res.accounts.push(spec.agent_id);
            // Use agent's gas constraint as hint if larger.
            let agent_gas = spec.constraints.max_gas;
            res.gas_hint = Some(res.gas_hint.unwrap_or(0).max(agent_gas));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentConstraints, AgentIntentSpec, AgentTask, HumanApproval};
    use nexus_primitives::{Amount, ContractAddress, TimestampMs, TokenId, ValidatorIndex};

    fn sender() -> AccountAddress {
        AccountAddress([0xAA; 32])
    }
    fn recipient() -> AccountAddress {
        AccountAddress([0xBB; 32])
    }
    fn contract_a() -> ContractAddress {
        ContractAddress([0xCC; 32])
    }

    #[test]
    fn parse_transfer() {
        let intent = UserIntent::Transfer {
            to: recipient(),
            token: TokenId::Native,
            amount: Amount(1000),
        };
        let res = parse_resources(sender(), &intent).unwrap();
        assert_eq!(res.required_balances.len(), 1);
        assert_eq!(res.required_balances[0], (TokenId::Native, Amount(1000)));
        assert!(res.accounts.contains(&sender()));
        assert!(res.accounts.contains(&recipient()));
        assert!(res.contracts.is_empty());
        assert!(res.gas_hint.is_none());
    }

    #[test]
    fn parse_swap() {
        let usdc = TokenId::Contract(ContractAddress([0x01; 32]));
        let intent = UserIntent::Swap {
            from_token: usdc,
            to_token: TokenId::Native,
            amount: Amount(500),
            max_slippage_bps: 50,
        };
        let res = parse_resources(sender(), &intent).unwrap();
        assert_eq!(res.required_balances.len(), 1);
        assert_eq!(res.required_balances[0], (usdc, Amount(500)));
        // Only sender, no extra accounts.
        assert_eq!(res.accounts.len(), 1);
        assert!(res.contracts.is_empty());
    }

    #[test]
    fn parse_contract_call() {
        let intent = UserIntent::ContractCall {
            contract: contract_a(),
            function: "transfer".to_string(),
            args: vec![],
            gas_budget: 50_000,
        };
        let res = parse_resources(sender(), &intent).unwrap();
        assert!(res.required_balances.is_empty());
        assert_eq!(res.contracts, vec![contract_a()]);
        assert_eq!(res.gas_hint, Some(50_000));
    }

    #[test]
    fn parse_stake() {
        let intent = UserIntent::Stake {
            validator: ValidatorIndex(5),
            amount: Amount(10_000),
        };
        let res = parse_resources(sender(), &intent).unwrap();
        assert_eq!(res.required_balances.len(), 1);
        assert_eq!(res.required_balances[0], (TokenId::Native, Amount(10_000)));
    }

    #[test]
    fn parse_agent_single_action() {
        let agent_id = AccountAddress([0x01; 32]);
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id,
            capability_token: TokenId::Native,
            task: AgentTask::SingleAction {
                action: Box::new(UserIntent::Transfer {
                    to: recipient(),
                    token: TokenId::Native,
                    amount: Amount(100),
                }),
            },
            constraints: AgentConstraints {
                max_gas: 200_000,
                max_value: Amount(100),
                allowed_contracts: vec![],
                deadline: TimestampMs(9_999_999),
            },
            human_approval: HumanApproval::PreApproved,
        };
        let intent = UserIntent::AgentTask { spec };
        let res = parse_resources(sender(), &intent).unwrap();
        assert!(res.accounts.contains(&agent_id));
        assert!(res.accounts.contains(&recipient()));
        assert_eq!(res.gas_hint, Some(200_000));
        assert_eq!(res.required_balances.len(), 1);
    }

    #[test]
    fn parse_agent_multistep_deduplicates() {
        let agent_id = AccountAddress([0x01; 32]);
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id,
            capability_token: TokenId::Native,
            task: AgentTask::MultiStep {
                steps: vec![
                    UserIntent::Transfer {
                        to: recipient(),
                        token: TokenId::Native,
                        amount: Amount(50),
                    },
                    UserIntent::ContractCall {
                        contract: contract_a(),
                        function: "deposit".to_string(),
                        args: vec![],
                        gas_budget: 30_000,
                    },
                    // Duplicate recipient.
                    UserIntent::Transfer {
                        to: recipient(),
                        token: TokenId::Native,
                        amount: Amount(25),
                    },
                ],
                execution_order: vec![vec![0], vec![1, 2]],
            },
            constraints: AgentConstraints {
                max_gas: 500_000,
                max_value: Amount(1000),
                allowed_contracts: vec![],
                deadline: TimestampMs(9_999_999),
            },
            human_approval: HumanApproval::PreApproved,
        };
        let intent = UserIntent::AgentTask { spec };
        let res = parse_resources(sender(), &intent).unwrap();
        // sender, recipient, agent_id — deduped.
        assert_eq!(res.accounts.len(), 3);
        // Two balances: 50 native + 25 native (not merged, planner handles aggregation).
        assert_eq!(res.required_balances.len(), 2);
        // contract_a.
        assert_eq!(res.contracts, vec![contract_a()]);
        // gas_hint = max(30_000, 500_000) = 500_000.
        assert_eq!(res.gas_hint, Some(500_000));
    }

    #[test]
    fn multiple_contract_calls_accumulate_gas() {
        let c1 = ContractAddress([0x01; 32]);
        let c2 = ContractAddress([0x02; 32]);
        let agent_id = AccountAddress([0x01; 32]);
        let spec = AgentIntentSpec {
            version: "nap/1.0".to_string(),
            agent_id,
            capability_token: TokenId::Native,
            task: AgentTask::MultiStep {
                steps: vec![
                    UserIntent::ContractCall {
                        contract: c1,
                        function: "f1".to_string(),
                        args: vec![],
                        gas_budget: 10_000,
                    },
                    UserIntent::ContractCall {
                        contract: c2,
                        function: "f2".to_string(),
                        args: vec![],
                        gas_budget: 20_000,
                    },
                ],
                execution_order: vec![vec![0, 1]],
            },
            constraints: AgentConstraints {
                max_gas: 5_000,
                max_value: Amount(0),
                allowed_contracts: vec![],
                deadline: TimestampMs(9_999_999),
            },
            human_approval: HumanApproval::PreApproved,
        };
        let intent = UserIntent::AgentTask { spec };
        let res = parse_resources(sender(), &intent).unwrap();
        // 10_000 + 20_000 = 30_000, then max(30_000, 5_000) = 30_000.
        assert_eq!(res.gas_hint, Some(30_000));
        assert_eq!(res.contracts.len(), 2);
    }
}
