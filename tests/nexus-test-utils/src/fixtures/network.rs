// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Network-layer test fixtures: peer IDs, messages, configs.

use nexus_crypto::DilithiumSigner;
use nexus_crypto::Signer;
use nexus_network::PeerId;
use nexus_primitives::Blake3Digest;

/// Create a [`PeerId`] derived from a fresh Dilithium keypair.
///
/// Every call generates a **unique** peer identity.
pub fn make_peer_id() -> PeerId {
    let (_sk, vk) = DilithiumSigner::generate_keypair();
    let pk_bytes = serde_json::to_vec(&vk).expect("serialize vk");
    PeerId::from_public_key(&pk_bytes)
}

/// Create a deterministic [`PeerId`] from a single fill byte.
///
/// Two calls with the same `fill` value return equal peer IDs.
pub fn make_deterministic_peer_id(fill: u8) -> PeerId {
    PeerId::from_digest(Blake3Digest::from_bytes([fill; 32]))
}

/// Create `n` unique peer IDs. Each is derived from a fresh keypair.
pub fn make_peer_ids(n: usize) -> Vec<PeerId> {
    (0..n).map(|_| make_peer_id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_peer_id_stable() {
        let a = make_deterministic_peer_id(1);
        let b = make_deterministic_peer_id(1);
        assert_eq!(a, b);
    }

    #[test]
    fn deterministic_peer_ids_distinct() {
        let a = make_deterministic_peer_id(1);
        let b = make_deterministic_peer_id(2);
        assert_ne!(a, b);
    }

    #[test]
    fn random_peer_ids_unique() {
        let ids = make_peer_ids(5);
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j]);
            }
        }
    }
}
