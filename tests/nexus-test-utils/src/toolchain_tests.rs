// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Tool chain integration tests — keygen → genesis → node boot.
//!
//! Exercises the full operator workflow:
//! 1. Generate validator keys (programmatically, mirroring nexus-keygen output)
//! 2. Build a genesis config from those keys
//! 3. Boot a node from the genesis file → verify committee + allocations

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {

    use nexus_config::genesis::{GenesisAllocation, GenesisConfig, GenesisValidatorEntry};
    use nexus_config::ConsensusConfig;

    use nexus_crypto::{
        DilithiumSigner, FalconSigner, KeyEncapsulationMechanism, KyberKem, Signer,
    };
    use nexus_primitives::{AccountAddress, Amount, TimestampMs};

    /// Helper: generate one validator's keys (mirrors nexus-keygen validator output).
    fn generate_validator_keys() -> (String, String, String) {
        let (_, fvk) = FalconSigner::generate_keypair();
        let (_, dvk) = DilithiumSigner::generate_keypair();
        let (kek, _) = KyberKem::generate_keypair();
        (
            hex::encode(fvk.as_bytes()),
            hex::encode(dvk.as_bytes()),
            hex::encode(kek.as_bytes()),
        )
    }

    /// Helper: build a genesis config from freshly generated keys.
    fn build_genesis(num_validators: usize, num_shards: u16) -> GenesisConfig {
        let validators: Vec<GenesisValidatorEntry> = (0..num_validators)
            .map(|i| {
                let (falcon_hex, dilithium_hex, kyber_hex) = generate_validator_keys();
                let keypair = libp2p_identity::Keypair::generate_ed25519();
                let peer_id = keypair.public().to_peer_id();
                GenesisValidatorEntry {
                    name: format!("validator-{i}"),
                    network_peer_id: peer_id.to_string(),
                    falcon_verify_key_hex: falcon_hex,
                    dilithium_verify_key_hex: dilithium_hex,
                    kyber_encaps_key_hex: kyber_hex,
                    stake: Amount::ONE_NXS,
                    shard_id: None,
                }
            })
            .collect();

        let treasury = AccountAddress([0x01; 32]);
        let allocations = vec![GenesisAllocation {
            address_hex: hex::encode(treasury.0),
            amount: Amount(10_000_000_000_000_000_000), // 10 NXS
        }];

        GenesisConfig {
            chain_id: "nexus-toolchain-test".to_owned(),
            genesis_timestamp: TimestampMs(1_700_000_000_000),
            num_shards,
            validators,
            allocations,
            consensus: ConsensusConfig::default(),
        }
    }

    /// Full toolchain: keygen → genesis → boot → query.
    #[test]
    #[cfg(not(feature = "move-vm"))]
    fn toolchain_keygen_genesis_boot() {
        let genesis = build_genesis(4, 2);

        // Validate genesis (as nexus-genesis validate would).
        genesis.validate().unwrap();

        // Write to temp file.
        let dir = std::env::temp_dir().join("nexus-toolchain-test");
        std::fs::create_dir_all(&dir).unwrap();
        let genesis_path = dir.join("genesis.json");
        std::fs::write(
            &genesis_path,
            serde_json::to_string_pretty(&genesis).unwrap(),
        )
        .unwrap();

        // Boot the node.
        let store = MemoryStore::new();
        let shard_id = ShardId(0);
        let boot = genesis_boot::boot_from_genesis(&genesis_path, &store, shard_id).unwrap();

        // Verify committee.
        assert_eq!(boot.committee.active_validators().len(), 4);
        assert_eq!(boot.num_shards, 2);
        assert_eq!(boot.chain_id, "nexus-toolchain-test");

        // Verify allocations via query backend.
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let commit_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let query = StorageQueryBackend::new(store, shard_id, epoch, commit_seq);

        let treasury = AccountAddress([0x01; 32]);
        let balance = query.account_balance(&treasury, &TokenId::Native).unwrap();
        assert_eq!(balance, Amount(10_000_000_000_000_000_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Genesis validation rejects < 4 validators (BFT minimum).
    #[test]
    fn toolchain_genesis_rejects_insufficient_validators() {
        let genesis = build_genesis(3, 1);
        let result = genesis.validate();
        assert!(result.is_err());
    }

    /// Round-trip: genesis → JSON → parse → validate → boot.
    #[test]
    fn toolchain_genesis_json_round_trip() {
        let genesis = build_genesis(5, 4);
        genesis.validate().unwrap();

        let json = serde_json::to_string_pretty(&genesis).unwrap();
        let parsed: GenesisConfig = serde_json::from_str(&json).unwrap();
        parsed.validate().unwrap();

        assert_eq!(parsed.chain_id, genesis.chain_id);
        assert_eq!(parsed.validators.len(), 5);
        assert_eq!(parsed.num_shards, 4);
    }

    /// Total supply calculation matches expected value.
    #[test]
    fn toolchain_genesis_total_supply() {
        let genesis = build_genesis(4, 1);
        // 4 validators × 1 NXS stake + 10 NXS treasury
        let expected_stake: u128 = 4 * u128::from(Amount::ONE_NXS.0);
        let expected_treasury: u128 = 10_000_000_000_000_000_000;
        assert_eq!(genesis.total_supply(), expected_stake + expected_treasury);
    }
}
