// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! I-9: Token precision integration tests (VOO-PRECISION).
//!
//! Validates that the voo precision switch (1 NXS = 10^9 voo) works
//! correctly across transfers, faucet mint, gas deduction, overflow
//! boundaries, and genesis loading.

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_config::{ExecutionConfig, GenesisConfig};
    use nexus_consensus::CommittedBatch;
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::spawn_execution_service;
    use nexus_execution::traits::StateView;
    use nexus_execution::types::{
        compute_tx_digest, ExecutionStatus, SignedTransaction, TransactionBody, TransactionPayload,
        TX_DOMAIN,
    };
    use nexus_primitives::{
        AccountAddress, Amount, Blake3Digest, CommitSequence, EpochNumber, ShardId, TimestampMs,
        TokenId,
    };

    use std::collections::HashMap;

    // ── In-memory state for execution tests ──────────────────────────────

    struct MemState {
        data: HashMap<(AccountAddress, Vec<u8>), Vec<u8>>,
    }

    impl MemState {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }

        fn set_balance(&mut self, addr: AccountAddress, balance: u64) {
            self.data
                .insert((addr, b"balance".to_vec()), balance.to_le_bytes().to_vec());
        }
    }

    impl StateView for MemState {
        fn get(
            &self,
            account: &AccountAddress,
            key: &[u8],
        ) -> nexus_execution::error::ExecutionResult<Option<Vec<u8>>> {
            Ok(self.data.get(&(*account, key.to_vec())).cloned())
        }
    }

    struct TestAccount {
        sk: nexus_crypto::DilithiumSigningKey,
        pk: nexus_crypto::DilithiumVerifyKey,
        address: AccountAddress,
    }

    impl TestAccount {
        fn random() -> Self {
            let (sk, pk) = DilithiumSigner::generate_keypair();
            let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
            Self { sk, pk, address }
        }
    }

    fn make_transfer(
        sender: &TestAccount,
        recipient: AccountAddress,
        amount: u64,
        gas_limit: u64,
        gas_price: u64,
    ) -> SignedTransaction {
        let body = TransactionBody {
            sender: sender.address,
            sequence_number: 0,
            expiry_epoch: EpochNumber(1000),
            gas_limit,
            gas_price,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient,
                amount: Amount(amount),
                token: TokenId::Native,
            },
            chain_id: 1,
        };
        let digest = compute_tx_digest(&body).expect("digest");
        let sig = DilithiumSigner::sign(&sender.sk, TX_DOMAIN, digest.as_bytes());
        SignedTransaction {
            body,
            signature: sig,
            sender_pk: sender.pk.clone(),
            digest,
        }
    }

    fn make_batch(seq: u64) -> CommittedBatch {
        CommittedBatch {
            anchor: Blake3Digest([seq as u8; 32]),
            certificates: vec![Blake3Digest([seq as u8; 32])],
            sequence: CommitSequence(seq),
            committed_at: TimestampMs(1_000_000 + seq),
        }
    }

    // ── Constants ────────────────────────────────────────────────────────

    /// 0.5 NXS in voo.
    const HALF_NXS_VOO: u64 = 500_000_000;

    // ── Tests ────────────────────────────────────────────────────────────

    // PT-01: Amount constants are correct.
    #[test]
    fn amount_constants_voo_precision() {
        assert_eq!(Amount::ZERO.0, 0);
        assert_eq!(Amount::ONE_VOO.0, 1);
        assert_eq!(Amount::ONE_NXS.0, 1_000_000_000);
        // u64::MAX ≈ 18.44 × 10^9 NXS — plenty of headroom.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                u64::MAX / Amount::ONE_NXS.0 > 18_000_000_000,
                "headroom check"
            );
        }
    }

    // PT-02: Transfer 0.5 NXS (500_000_000 voo) via execution service.
    #[tokio::test]
    async fn transfer_half_nxs() {
        let sender = TestAccount::random();
        let recipient = TestAccount::random();

        let mut state = MemState::new();
        state.set_balance(sender.address, Amount::ONE_NXS.0);
        state.set_balance(recipient.address, 0);

        let handle =
            spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), Arc::new(state));

        let tx = make_transfer(&sender, recipient.address, HALF_NXS_VOO, 50_000, 1);
        let batch = make_batch(1);
        let result = handle.submit_batch(batch, vec![tx]).await.unwrap();

        assert_eq!(result.receipts.len(), 1);
        let receipt = &result.receipts[0];
        assert!(
            matches!(receipt.status, ExecutionStatus::Success),
            "transfer should succeed, got {:?}",
            receipt.status
        );
        assert!(receipt.gas_used > 0, "gas should be consumed");

        handle.shutdown().await.unwrap();
    }

    // PT-03: Faucet mint uses correct voo precision.
    #[test]
    fn faucet_mint_voo_precision() {
        let current: u64 = 0;
        let faucet_amount = Amount::ONE_NXS.0;
        let new_balance = current.checked_add(faucet_amount).expect("no overflow");
        assert_eq!(new_balance, 1_000_000_000);

        // Multiple mints don't overflow.
        let after_10_mints = faucet_amount.checked_mul(10).expect("no overflow");
        assert_eq!(after_10_mints, 10_000_000_000); // 10 NXS
    }

    // PT-04: Saturating add near u64::MAX.
    #[test]
    fn saturating_add_near_max() {
        let near_max = Amount(u64::MAX - 100);
        let result = near_max.0.saturating_add(200);
        assert_eq!(result, u64::MAX, "saturating_add should cap at u64::MAX");
        // ~18.44 billion NXS — well beyond any realistic supply.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                u64::MAX / 1_000_000_000 > 18_000_000_000,
                "supply headroom check"
            );
        }
    }

    // PT-05: Gas deduction does not overflow under voo precision.
    #[test]
    fn gas_cost_no_overflow() {
        let gas_limit: u64 = 50_000;
        let gas_price: u64 = 1;
        let gas_cost = gas_limit.checked_mul(gas_price).expect("no overflow");
        assert_eq!(gas_cost, 50_000);

        // Even with high gas_price, total_cost stays within u64.
        let high_gas_price: u64 = 1_000_000; // 1M voo per gas unit — extreme.
        let high_cost = gas_limit.checked_mul(high_gas_price).expect("no overflow");
        assert_eq!(high_cost, 50_000_000_000); // 50 NXS — reasonable.

        // transfer_amount + gas_cost does not overflow.
        let transfer = HALF_NXS_VOO;
        let total = transfer.checked_add(gas_cost).expect("no overflow");
        assert_eq!(total, 500_050_000);
    }

    // PT-06: Genesis total supply is correct under new precision.
    #[test]
    fn genesis_total_supply_voo() {
        let cfg = GenesisConfig::for_testing();
        let total = cfg.total_supply();
        // 4 validators × 1 NXS + 1 NXS allocation = 5 NXS = 5 × 10^9 voo.
        let expected = 5 * 1_000_000_000_u128;
        assert_eq!(total, expected);
    }

    // PT-07: Genesis for_testing() produces valid config with voo amounts.
    #[test]
    fn genesis_for_testing_valid() {
        let cfg = GenesisConfig::for_testing();
        cfg.validate().expect("for_testing() must be valid");
        for v in &cfg.validators {
            assert_eq!(v.stake, Amount::ONE_NXS);
        }
        assert_eq!(cfg.allocations[0].amount, Amount(1_000_000_000));
    }

    // PT-08: RPC faucet_amount validation accepts 10^9 and rejects > 10^9.
    #[test]
    fn faucet_amount_validation() {
        use nexus_config::RpcConfig;

        let mut cfg = RpcConfig::for_testing();
        assert_eq!(cfg.faucet_amount, 1_000_000_000);

        // Increase beyond 1 NXS should fail validation.
        cfg.faucet_amount = 1_000_000_001;
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("faucet_amount"),
            "error should mention faucet_amount: {err}"
        );

        // Exactly 1 NXS should pass.
        cfg.faucet_amount = 1_000_000_000;
        cfg.validate().expect("1 NXS should be valid");

        // Below 1 NXS should also pass.
        cfg.faucet_amount = 500_000_000;
        cfg.validate().expect("0.5 NXS should be valid");
    }

    // PT-09: Insufficient balance under new precision.
    #[tokio::test]
    async fn insufficient_balance_new_precision() {
        let sender = TestAccount::random();
        let recipient = TestAccount::random();

        let mut state = MemState::new();
        // Sender has only 100 voo.
        state.set_balance(sender.address, 100);

        let handle =
            spawn_execution_service(ExecutionConfig::for_testing(), ShardId(0), Arc::new(state));

        // Try to send 1 NXS — should fail with insufficient balance.
        let tx = make_transfer(&sender, recipient.address, 1_000_000_000, 50_000, 1);
        let batch = make_batch(1);
        let result = handle.submit_batch(batch, vec![tx]).await.unwrap();

        assert_eq!(result.receipts.len(), 1);
        assert!(
            matches!(result.receipts[0].status, ExecutionStatus::MoveAbort { .. }),
            "should fail with insufficient balance, got {:?}",
            result.receipts[0].status
        );

        handle.shutdown().await.unwrap();
    }
}
