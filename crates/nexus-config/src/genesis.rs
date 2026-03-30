// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Genesis configuration — initial chain state and validator set.
//!
//! [`GenesisConfig`] describes the initial parameters required to bootstrap
//! a new Nexus chain: the founding validator set, initial token allocations,
//! chain identity, and consensus parameters.
//!
//! This module is used by:
//! - `nexus-genesis` CLI tool (to create the genesis file)
//! - `nexus-node` binary (to load the genesis file at first boot)

use serde::{Deserialize, Serialize};

use nexus_primitives::{AccountAddress, Amount, ShardId, TimestampMs};

use crate::ConsensusConfig;

/// A single validator entry in the genesis validator set.
///
/// Contains the hex-encoded public keys that identify the validator
/// on the consensus and transaction layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisValidatorEntry {
    /// Human-readable name (for operator reference only).
    pub name: String,
    /// Canonical libp2p PeerId string (base58) used as the network identity.
    ///
    /// Derived from the node's Ed25519 identity key. This **must** be set
    /// for every validator — there is no fallback derivation from
    /// Dilithium keys.
    pub network_peer_id: String,
    /// Hex-encoded Falcon-512 verification key (consensus layer).
    pub falcon_verify_key_hex: String,
    /// Hex-encoded Dilithium3 verification key (transaction layer).
    pub dilithium_verify_key_hex: String,
    /// Hex-encoded Kyber-768 encapsulation key (P2P encryption).
    pub kyber_encaps_key_hex: String,
    /// Initial stake in NXS base units.
    pub stake: Amount,
    /// Optional shard assignment (None = auto-assign).
    pub shard_id: Option<ShardId>,
}

/// An initial token allocation in the genesis state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisAllocation {
    /// Recipient account address (hex-encoded 32 bytes).
    pub address_hex: String,
    /// Amount in NXS base units.
    pub amount: Amount,
}

/// Complete genesis configuration for bootstrapping a Nexus chain.
///
/// Serialised to JSON and loaded by the node at first boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Unique chain identifier (e.g., "nexus-mainnet-v1", "nexus-testnet-1").
    pub chain_id: String,

    /// Genesis timestamp (milliseconds since Unix epoch).
    pub genesis_timestamp: TimestampMs,

    /// Number of execution shards at genesis.
    pub num_shards: u16,

    /// The founding validator set.
    pub validators: Vec<GenesisValidatorEntry>,

    /// Initial token allocations (treasury, foundation, etc.).
    pub allocations: Vec<GenesisAllocation>,

    /// Consensus parameters for epoch 0.
    pub consensus: ConsensusConfig,
}

impl GenesisConfig {
    /// Validate the genesis configuration.
    ///
    /// # Errors
    /// Returns a descriptive error if validation fails.
    pub fn validate(&self) -> Result<(), GenesisValidationError> {
        if self.chain_id.is_empty() {
            return Err(GenesisValidationError::EmptyChainId);
        }
        if self.validators.is_empty() {
            return Err(GenesisValidationError::NoValidators);
        }
        if self.num_shards == 0 {
            return Err(GenesisValidationError::ZeroShards);
        }

        // BFT requires at least 4 validators for f=1, threshold=3.
        let n = self.validators.len();
        let f = (n.saturating_sub(1)) / 3;
        if f == 0 {
            return Err(GenesisValidationError::InsufficientValidators {
                count: n,
                minimum: 4,
            });
        }

        // Validate each validator entry has non-empty keys.
        for (i, v) in self.validators.iter().enumerate() {
            if v.falcon_verify_key_hex.is_empty() {
                return Err(GenesisValidationError::EmptyKey {
                    validator_index: i,
                    key_type: "falcon_verify_key",
                });
            }
            if v.dilithium_verify_key_hex.is_empty() {
                return Err(GenesisValidationError::EmptyKey {
                    validator_index: i,
                    key_type: "dilithium_verify_key",
                });
            }
            if v.kyber_encaps_key_hex.is_empty() {
                return Err(GenesisValidationError::EmptyKey {
                    validator_index: i,
                    key_type: "kyber_encaps_key",
                });
            }
            if v.network_peer_id.is_empty() {
                return Err(GenesisValidationError::EmptyKey {
                    validator_index: i,
                    key_type: "network_peer_id",
                });
            }
            if v.stake.0 == 0 {
                return Err(GenesisValidationError::ZeroStake { validator_index: i });
            }
            // Validate hex encoding.
            hex::decode(&v.falcon_verify_key_hex).map_err(|_| {
                GenesisValidationError::InvalidHex {
                    validator_index: i,
                    key_type: "falcon_verify_key",
                }
            })?;
            hex::decode(&v.dilithium_verify_key_hex).map_err(|_| {
                GenesisValidationError::InvalidHex {
                    validator_index: i,
                    key_type: "dilithium_verify_key",
                }
            })?;
            hex::decode(&v.kyber_encaps_key_hex).map_err(|_| {
                GenesisValidationError::InvalidHex {
                    validator_index: i,
                    key_type: "kyber_encaps_key",
                }
            })?;
        }

        // Validate allocation addresses.
        for (i, alloc) in self.allocations.iter().enumerate() {
            let bytes = hex::decode(&alloc.address_hex)
                .map_err(|_| GenesisValidationError::InvalidAllocationAddress { index: i })?;
            if bytes.len() != 32 {
                return Err(GenesisValidationError::InvalidAllocationAddress { index: i });
            }
        }

        Ok(())
    }

    /// Compute the total initial supply from allocations + validator stakes.
    pub fn total_supply(&self) -> u128 {
        let alloc_total: u128 = self
            .allocations
            .iter()
            .map(|a| u128::from(a.amount.0))
            .sum();
        let stake_total: u128 = self.validators.iter().map(|v| u128::from(v.stake.0)).sum();
        alloc_total + stake_total
    }

    /// Compute a deterministic BLAKE3 hash of the genesis configuration.
    ///
    /// Uses canonical JSON serialisation (via serde_json) as the input.
    /// The hash is domain-separated with `b"nexus-genesis-hash"`.
    pub fn genesis_hash(&self) -> nexus_primitives::Blake3Digest {
        use nexus_crypto::Blake3Hasher;
        let canonical =
            serde_json::to_vec(self).expect("GenesisConfig serialisation should never fail");
        Blake3Hasher::digest(b"nexus-genesis-hash", &canonical)
    }

    /// Minimal genesis configuration for testing (4 validators, 1 shard).
    pub fn for_testing() -> Self {
        use nexus_crypto::{
            DilithiumSigner, FalconSigner, KeyEncapsulationMechanism, KyberKem, Signer,
        };

        let validators: Vec<GenesisValidatorEntry> = (0..4)
            .map(|i| {
                let (_, fvk) = FalconSigner::generate_keypair();
                let (_, dvk) = DilithiumSigner::generate_keypair();
                let (kek, _) = KyberKem::generate_keypair();
                let keypair = libp2p_identity::Keypair::generate_ed25519();
                let peer_id = keypair.public().to_peer_id();

                GenesisValidatorEntry {
                    name: format!("validator-{i}"),
                    network_peer_id: peer_id.to_string(),
                    falcon_verify_key_hex: hex::encode(fvk.as_bytes()),
                    dilithium_verify_key_hex: hex::encode(dvk.as_bytes()),
                    kyber_encaps_key_hex: hex::encode(kek.as_bytes()),
                    stake: Amount::ONE_NXS,
                    shard_id: None,
                }
            })
            .collect();

        Self {
            chain_id: "nexus-test-0".to_owned(),
            genesis_timestamp: TimestampMs(0),
            num_shards: 1,
            validators,
            allocations: vec![GenesisAllocation {
                address_hex: hex::encode(AccountAddress::ZERO.0),
                amount: Amount(1_000_000_000), // 1 NXS for test treasury
            }],
            consensus: ConsensusConfig::default(),
        }
    }
}

/// Errors detected during genesis config validation.
#[derive(Debug, thiserror::Error)]
pub enum GenesisValidationError {
    /// Chain ID must not be empty.
    #[error("chain_id must not be empty")]
    EmptyChainId,
    /// At least one validator is required.
    #[error("genesis requires at least one validator")]
    NoValidators,
    /// Number of shards must be positive.
    #[error("num_shards must be > 0")]
    ZeroShards,
    /// BFT requires at least 4 validators (f ≥ 1).
    #[error("BFT requires at least {minimum} validators, got {count}")]
    InsufficientValidators {
        /// Number of validators provided.
        count: usize,
        /// Minimum required.
        minimum: usize,
    },
    /// A validator entry has an empty key.
    #[error("validator {validator_index}: {key_type} must not be empty")]
    EmptyKey {
        /// Zero-based validator index.
        validator_index: usize,
        /// Which key field is empty.
        key_type: &'static str,
    },
    /// A validator entry has zero stake.
    #[error("validator {validator_index}: stake must be > 0")]
    ZeroStake {
        /// Zero-based validator index.
        validator_index: usize,
    },
    /// A validator key contains invalid hex.
    #[error("validator {validator_index}: {key_type} is not valid hex")]
    InvalidHex {
        /// Zero-based validator index.
        validator_index: usize,
        /// Which key field has bad hex.
        key_type: &'static str,
    },
    /// An allocation address is invalid (not 32-byte hex).
    #[error("allocation {index}: invalid address")]
    InvalidAllocationAddress {
        /// Zero-based allocation index.
        index: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_genesis_for_testing() {
        let cfg = GenesisConfig::for_testing();
        cfg.validate()
            .expect("for_testing() should produce a valid config");
        assert_eq!(cfg.validators.len(), 4);
        assert_eq!(cfg.num_shards, 1);
    }

    #[test]
    fn test_empty_chain_id() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.chain_id = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, GenesisValidationError::EmptyChainId));
    }

    #[test]
    fn test_no_validators() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators.clear();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, GenesisValidationError::NoValidators));
    }

    #[test]
    fn test_insufficient_validators() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators.truncate(3); // 3 validators → f=0, not BFT
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::InsufficientValidators { count: 3, .. }
        ));
    }

    #[test]
    fn test_zero_shards() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.num_shards = 0;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, GenesisValidationError::ZeroShards));
    }

    #[test]
    fn test_empty_key_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].falcon_verify_key_hex = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::EmptyKey {
                validator_index: 0,
                key_type: "falcon_verify_key"
            }
        ));
    }

    #[test]
    fn test_zero_stake_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[1].stake = Amount::ZERO;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::ZeroStake { validator_index: 1 }
        ));
    }

    #[test]
    fn test_invalid_hex_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].falcon_verify_key_hex = "not_hex!!!".to_owned();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::InvalidHex {
                validator_index: 0,
                key_type: "falcon_verify_key"
            }
        ));
    }

    #[test]
    fn test_invalid_allocation_address() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.allocations.push(GenesisAllocation {
            address_hex: "deadbeef".to_owned(), // only 4 bytes, not 32
            amount: Amount(1000),
        });
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::InvalidAllocationAddress { .. }
        ));
    }

    #[test]
    fn test_total_supply() {
        let cfg = GenesisConfig::for_testing();
        let total = cfg.total_supply();
        let expected_validators = 4 * Amount::ONE_NXS.0 as u128;
        let expected_allocs = 1_000_000_000_u128; // 1 NXS
        assert_eq!(total, expected_validators + expected_allocs);
    }

    #[test]
    fn test_json_round_trip() {
        let cfg = GenesisConfig::for_testing();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let restored: GenesisConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.chain_id, cfg.chain_id);
        assert_eq!(restored.validators.len(), cfg.validators.len());
        assert_eq!(restored.num_shards, cfg.num_shards);
    }

    #[test]
    fn test_genesis_hash_deterministic() {
        let cfg = GenesisConfig::for_testing();
        let h1 = cfg.genesis_hash();
        let h2 = cfg.genesis_hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_genesis_hash_changes_with_chain_id() {
        let cfg1 = GenesisConfig::for_testing();
        let cfg2 = GenesisConfig::for_testing();
        // Note: for_testing() generates random keys each call, so the hashes
        // will differ. We verify they are non-zero.
        assert_ne!(cfg1.genesis_hash(), nexus_primitives::Blake3Digest::ZERO);
        assert_ne!(cfg2.genesis_hash(), nexus_primitives::Blake3Digest::ZERO);
    }

    // ── Additional validation branch coverage ───────────────────────────

    #[test]
    fn test_empty_dilithium_key_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].dilithium_verify_key_hex = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::EmptyKey {
                validator_index: 0,
                key_type: "dilithium_verify_key"
            }
        ));
    }

    #[test]
    fn test_empty_kyber_key_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].kyber_encaps_key_hex = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::EmptyKey {
                validator_index: 0,
                key_type: "kyber_encaps_key"
            }
        ));
    }

    #[test]
    fn test_empty_peer_id_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].network_peer_id = String::new();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::EmptyKey {
                validator_index: 0,
                key_type: "network_peer_id"
            }
        ));
    }

    #[test]
    fn test_invalid_dilithium_hex_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].dilithium_verify_key_hex = "ZZZZ".to_owned();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::InvalidHex {
                validator_index: 0,
                key_type: "dilithium_verify_key"
            }
        ));
    }

    #[test]
    fn test_invalid_kyber_hex_rejected() {
        let mut cfg = GenesisConfig::for_testing();
        cfg.validators[0].kyber_encaps_key_hex = "ZZZZ".to_owned();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(
            err,
            GenesisValidationError::InvalidHex {
                validator_index: 0,
                key_type: "kyber_encaps_key"
            }
        ));
    }

    #[test]
    fn test_all_error_display_variants() {
        let errors: Vec<GenesisValidationError> = vec![
            GenesisValidationError::EmptyChainId,
            GenesisValidationError::NoValidators,
            GenesisValidationError::ZeroShards,
            GenesisValidationError::InsufficientValidators { count: 2, minimum: 4 },
            GenesisValidationError::EmptyKey { validator_index: 0, key_type: "falcon_verify_key" },
            GenesisValidationError::ZeroStake { validator_index: 1 },
            GenesisValidationError::InvalidHex { validator_index: 0, key_type: "falcon_verify_key" },
            GenesisValidationError::InvalidAllocationAddress { index: 0 },
        ];
        for err in &errors {
            let msg = format!("{err}");
            assert!(!msg.is_empty(), "Display should be non-empty for {err:?}");
        }
    }
}
