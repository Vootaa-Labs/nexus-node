// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Validator discovery — seeds the peer table from genesis and triggers
//! DHT bootstrap for ongoing peer discovery.
//!
//! **T-7005**: Load the genesis validator set into the discovery layer so
//! that the node knows about its peers *before* Kademlia bootstrap completes.
//! Then, if boot-nodes are configured, initiate DHT bootstrap for full
//! routing-table population.

use anyhow::Context;
use nexus_config::genesis::GenesisConfig;
use nexus_network::config::NetworkConfig;
use nexus_network::discovery::{DiscoveryHandle, NodeRecord};
use nexus_network::types::PeerId;
use tracing::{debug, info, warn};

/// Result of the validator discovery phase.
#[derive(Debug)]
pub struct ValidatorDiscoveryResult {
    /// Number of genesis validators seeded into the peer table.
    pub validators_seeded: usize,
    /// Number of boot-node addresses registered with Kademlia.
    pub boot_nodes_added: usize,
    /// Whether a Kademlia bootstrap round was initiated.
    pub bootstrap_initiated: bool,
}

/// Seed genesis validator identities into the discovery layer and
/// trigger Kademlia bootstrap.
///
/// This should be called **after** `NetworkService::build()` and once the
/// network event loop is running, because bootstrapping sends commands through
/// the transport task and awaits its reply.
pub async fn discover_validators(
    discovery: &DiscoveryHandle,
    genesis: &GenesisConfig,
    net_config: &NetworkConfig,
) -> anyhow::Result<ValidatorDiscoveryResult> {
    // 1. Seed genesis validators into peer table
    let validators_seeded = seed_genesis_validators(discovery, genesis)?;

    // 2. Register boot-node addresses with Kademlia
    let boot_nodes_added = register_boot_nodes(discovery, net_config).await?;

    // 3. Trigger Kademlia bootstrap if boot nodes are present
    let bootstrap_initiated = if boot_nodes_added > 0 {
        match discovery.bootstrap().await {
            Ok(()) => {
                info!("kademlia bootstrap initiated");
                true
            }
            Err(e) => {
                warn!(error = %e, "kademlia bootstrap failed — continuing without DHT");
                false
            }
        }
    } else {
        debug!("no boot nodes configured — skipping Kademlia bootstrap");
        false
    };

    info!(
        validators_seeded,
        boot_nodes_added, bootstrap_initiated, "validator discovery complete"
    );

    Ok(ValidatorDiscoveryResult {
        validators_seeded,
        boot_nodes_added,
        bootstrap_initiated,
    })
}

/// Convert genesis validator entries to `NodeRecord`s and insert into
/// the discovery peer table.
fn seed_genesis_validators(
    discovery: &DiscoveryHandle,
    genesis: &GenesisConfig,
) -> anyhow::Result<usize> {
    let mut count = 0;

    for (i, entry) in genesis.validators.iter().enumerate() {
        let dilithium_bytes = hex::decode(&entry.dilithium_verify_key_hex)
            .with_context(|| format!("invalid dilithium hex for validator {i}"))?;

        let libp2p_peer = entry
            .network_peer_id
            .parse::<libp2p::PeerId>()
            .with_context(|| {
                format!(
                    "invalid network_peer_id for validator {i}: {}",
                    entry.network_peer_id
                )
            })?;
        let peer_id = PeerId::from_libp2p(&libp2p_peer);

        let record = NodeRecord {
            peer_id,
            addresses: Vec::new(),
            dilithium_pubkey: dilithium_bytes,
            reputation: 0,
            last_seen: 0,
            validator_stake: Some(entry.stake.0 as u128),
        };

        discovery.seed_validator_record(record);
        count += 1;

        debug!(
            validator = %entry.name,
            peer = %peer_id,
            stake = entry.stake.0,
            "seeded genesis validator"
        );
    }

    Ok(count)
}

/// Parse boot-node multiaddresses and register them with the Kademlia
/// routing table.
///
/// Boot-node strings are expected in multiaddr format, optionally with
/// a `/p2p/<peer-id>` suffix. The `/p2p/` component is required for
/// Kademlia to associate the address with a peer identity.
async fn register_boot_nodes(
    discovery: &DiscoveryHandle,
    config: &NetworkConfig,
) -> anyhow::Result<usize> {
    let mut added = 0;

    for (i, raw) in config.boot_nodes.iter().enumerate() {
        let addr: libp2p::Multiaddr = raw
            .parse()
            .with_context(|| format!("boot_nodes[{i}]: invalid multiaddr '{raw}'"))?;

        // Extract the /p2p/<peer-id> component
        let libp2p_peer = extract_peer_id(&addr).with_context(|| {
            format!("boot_nodes[{i}]: multiaddr '{raw}' missing /p2p/<peer-id> suffix")
        })?;

        // Strip /p2p/ from the address for Kademlia (it wants addr without the peer component)
        let dial_addr = strip_p2p_suffix(&addr);

        discovery.add_boot_node(libp2p_peer, dial_addr).await?;

        // Actually dial the boot node — Kademlia's add_address only stores
        // the address; it does NOT open a connection.  We need at least one
        // live connection for the subsequent bootstrap query to succeed.
        match discovery.dial(addr.clone()).await {
            Ok(()) => {
                debug!(index = i, peer = %libp2p_peer, "dialed boot node");
            }
            Err(e) => {
                warn!(
                    index = i,
                    peer = %libp2p_peer,
                    error = %e,
                    "failed to dial boot node — bootstrap may be degraded"
                );
            }
        }

        added += 1;

        debug!(
            index = i,
            peer = %libp2p_peer,
            addr = %raw,
            "registered boot node"
        );
    }

    Ok(added)
}

/// Extract the libp2p `PeerId` from a `/p2p/<hash>` multiaddr component.
fn extract_peer_id(addr: &libp2p::Multiaddr) -> Option<libp2p::PeerId> {
    addr.iter().find_map(|proto| {
        if let libp2p::multiaddr::Protocol::P2p(peer_id) = proto {
            Some(peer_id)
        } else {
            None
        }
    })
}

/// Return the multiaddr with the `/p2p/` component removed.
fn strip_p2p_suffix(addr: &libp2p::Multiaddr) -> libp2p::Multiaddr {
    addr.iter()
        .filter(|proto| !matches!(proto, libp2p::multiaddr::Protocol::P2p(_)))
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_config::genesis::GenesisConfig;
    use nexus_network::{NetworkConfig, NetworkService};

    #[tokio::test]
    async fn seed_genesis_validators_populates_peer_table() {
        let genesis = GenesisConfig::for_testing();
        let config = NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).expect("build network");

        let count = seed_genesis_validators(&handle.discovery, &genesis).unwrap();

        assert_eq!(count, genesis.validators.len());
        assert_eq!(handle.discovery.known_validators(), count);

        // Each validator should produce a PeerId from its network_peer_id
        for entry in &genesis.validators {
            let libp2p_peer = entry
                .network_peer_id
                .parse::<libp2p::PeerId>()
                .expect("valid network_peer_id");
            let pid = PeerId::from_libp2p(&libp2p_peer);
            let rec = handle.discovery.get_record(&pid);
            assert!(
                rec.is_some(),
                "validator {} not found in peer table",
                entry.name
            );
            let rec = rec.unwrap();
            let dk = hex::decode(&entry.dilithium_verify_key_hex).unwrap();
            assert_eq!(rec.dilithium_pubkey, dk);
            assert_eq!(rec.validator_stake, Some(entry.stake.0 as u128));
        }
    }

    #[tokio::test]
    async fn seed_idempotent_no_overwrite() {
        let genesis = GenesisConfig::for_testing();
        let config = NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).expect("build network");

        // Seed twice
        let c1 = seed_genesis_validators(&handle.discovery, &genesis).unwrap();
        let c2 = seed_genesis_validators(&handle.discovery, &genesis).unwrap();

        assert_eq!(c1, c2);
        // Still same number of entries (not doubled)
        assert_eq!(
            handle.discovery.known_validators(),
            genesis.validators.len()
        );
    }

    #[tokio::test]
    async fn seed_uses_network_peer_id() {
        let mut genesis = GenesisConfig::for_testing();
        let config = NetworkConfig::for_testing();
        let (handle, _service) = NetworkService::build(&config).expect("build network");

        let libp2p_peer = libp2p::PeerId::random();
        genesis.validators[0].network_peer_id = libp2p_peer.to_string();

        seed_genesis_validators(&handle.discovery, &genesis).unwrap();

        let expected = PeerId::from_libp2p(&libp2p_peer);
        assert!(handle.discovery.get_record(&expected).is_some());
    }

    #[test]
    fn extract_peer_id_from_multiaddr() {
        let peer = libp2p::PeerId::random();
        let addr: libp2p::Multiaddr = format!("/ip4/127.0.0.1/udp/9100/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();

        let extracted = extract_peer_id(&addr);
        assert_eq!(extracted, Some(peer));
    }

    #[test]
    fn extract_peer_id_missing_returns_none() {
        let addr: libp2p::Multiaddr = "/ip4/127.0.0.1/udp/9100/quic-v1".parse().unwrap();
        assert!(extract_peer_id(&addr).is_none());
    }

    #[test]
    fn strip_p2p_suffix_removes_peer_component() {
        let peer = libp2p::PeerId::random();
        let full: libp2p::Multiaddr = format!("/ip4/127.0.0.1/udp/9100/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();

        let stripped = strip_p2p_suffix(&full);
        assert!(!stripped.to_string().contains("p2p"));
        assert!(stripped.to_string().contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn discover_validators_with_no_boot_nodes() {
        let genesis = GenesisConfig::for_testing();
        let config = NetworkConfig::for_testing(); // boot_nodes is empty
        let (handle, _service) = NetworkService::build(&config).expect("build network");

        let result = discover_validators(&handle.discovery, &genesis, &config)
            .await
            .unwrap();

        assert_eq!(result.validators_seeded, genesis.validators.len());
        assert_eq!(result.boot_nodes_added, 0);
        assert!(!result.bootstrap_initiated);
    }

    #[test]
    fn discover_validators_with_boot_nodes() {
        // Use a custom runtime so we control shutdown_timeout — orphaned
        // QUIC tasks from the dial to a non-listening port would otherwise
        // block the default `#[tokio::test]` runtime teardown for 30+ s.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let genesis = GenesisConfig::for_testing();
            let peer = libp2p::PeerId::random();
            let mut config = NetworkConfig::for_testing();
            config.boot_nodes = vec![format!("/ip4/127.0.0.1/udp/19100/quic-v1/p2p/{peer}")];

            let (handle, service) = NetworkService::build(&config).expect("build network");
            let shutdown = handle.transport.clone();
            let net_task = tokio::spawn(service.run());

            let result = discover_validators(&handle.discovery, &genesis, &config)
                .await
                .unwrap();

            assert_eq!(result.validators_seeded, genesis.validators.len());
            assert_eq!(result.boot_nodes_added, 1);
            assert!(result.bootstrap_initiated);

            // Shutdown transport, then drop remaining handles.
            shutdown.shutdown().await.expect("shutdown");
            drop(handle);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), net_task).await;
        });

        // Force-close any lingering QUIC background tasks within 1 s.
        rt.shutdown_timeout(std::time::Duration::from_secs(1));
    }
}
