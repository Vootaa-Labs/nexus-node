// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! QUIC transport configuration and libp2p Swarm construction.
//!
//! Builds a production-ready [`Swarm`] with QUIC transport, Noise encryption,
//! and the Nexus-specific composite behaviour.

use libp2p::identity::Keypair;
use libp2p::{gossipsub, identify, kad, request_response, Swarm, SwarmBuilder};
use std::time::Duration;
use tracing::info;

use crate::config::NetworkConfig;
use crate::error::{NetworkError, NetworkResult};

// ── Request-Response Codec ───────────────────────────────────────────────────

/// Protocol name for Nexus unicast request-response.
pub const NEXUS_RR_PROTOCOL: &str = "/nexus/rr/1.0.0";

/// Codec for Nexus request-response exchanges.
///
/// Messages are opaque byte vectors. Higher-level framing (BCS envelope)
/// is handled by the caller, keeping the codec simple and protocol-agnostic.
#[derive(Debug, Clone, Default)]
pub struct NexusCodec;

/// Maximum request/response size (4 MB; PQ-signed batches can be large).
const MAX_RR_SIZE: u64 = 4 * 1024 * 1024;

#[async_trait::async_trait]
impl request_response::Codec for NexusCodec {
    type Protocol = String;
    type Request = Vec<u8>;
    type Response = Vec<u8>;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, MAX_RR_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: futures::AsyncRead + Unpin + Send,
    {
        read_length_prefixed(io, MAX_RR_SIZE).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        resp: Self::Response,
    ) -> std::io::Result<()>
    where
        T: futures::AsyncWrite + Unpin + Send,
    {
        write_length_prefixed(io, &resp).await
    }
}

/// Read a length-prefixed message from an async reader.
async fn read_length_prefixed<T: futures::AsyncRead + Unpin + Send>(
    io: &mut T,
    max_size: u64,
) -> std::io::Result<Vec<u8>> {
    use futures::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as u64;
    if len > max_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("message too large: {len} bytes (max {max_size})"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    io.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a length-prefixed message to an async writer.
async fn write_length_prefixed<T: futures::AsyncWrite + Unpin + Send>(
    io: &mut T,
    data: &[u8],
) -> std::io::Result<()> {
    use futures::AsyncWriteExt;
    let len: u32 = data.len().try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("message too large to frame: {} bytes", data.len()),
        )
    })?;
    let len = len.to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(data).await?;
    Ok(())
}

// ── Composite Behaviour ──────────────────────────────────────────────────────

#[allow(missing_docs)]
mod behaviour {
    use libp2p::swarm::NetworkBehaviour;
    use libp2p::{gossipsub, identify, kad, request_response};

    /// The composite libp2p `NetworkBehaviour` used by Nexus.
    ///
    /// Combines GossipSub (topic-based broadcast), Kademlia (DHT peer discovery),
    /// Identify (protocol negotiation), and Request-Response (unicast messaging).
    #[derive(NetworkBehaviour)]
    pub struct NexusBehaviour {
        /// GossipSub 1.1 publish/subscribe.
        pub gossipsub: gossipsub::Behaviour,
        /// Kademlia DHT for peer discovery.
        pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
        /// Identify protocol for exchanging peer metadata.
        pub identify: identify::Behaviour,
        /// Request-Response unicast protocol.
        pub reqres: request_response::Behaviour<super::NexusCodec>,
    }
}

pub use behaviour::{NexusBehaviour, NexusBehaviourEvent};

/// Event type emitted by [`NexusBehaviour`].
pub type BehaviourEvent = NexusBehaviourEvent;

// ── Swarm Builder ────────────────────────────────────────────────────────────

/// Build a libp2p `Swarm` configured for Nexus P2P networking.
///
/// Uses QUIC transport with Noise encryption. If `config.identity_key_path`
/// is set, the keypair is loaded from disk (or generated and saved on first
/// run). Otherwise an ephemeral keypair is used.
pub fn build_swarm(config: &NetworkConfig) -> NetworkResult<Swarm<NexusBehaviour>> {
    let local_key = load_or_generate_keypair(config)?;
    let local_peer_id = local_key.public().to_peer_id();
    info!(%local_peer_id, "building swarm with QUIC transport");

    // Build behaviour before the swarm so we can return errors properly.
    let behaviour = build_behaviour(&local_key, config)?;

    let swarm = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_quic()
        .with_behaviour(|_key| behaviour)
        .map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!(
                "behaviour build failed: {e}"
            )))
        })?
        .with_swarm_config(|cfg| {
            cfg.with_idle_connection_timeout(Duration::from_millis(
                config.connection_idle_timeout_ms,
            ))
        })
        .build();

    Ok(swarm)
}

/// Construct the composite [`NexusBehaviour`].
fn build_behaviour(key: &Keypair, config: &NetworkConfig) -> NetworkResult<NexusBehaviour> {
    // ── GossipSub ────────────────────────────────────────────────────────
    let mesh_outbound = config.gossip_mesh_lo / 2;
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .mesh_n(config.gossip_mesh_size)
        .mesh_n_low(config.gossip_mesh_lo)
        .mesh_n_high(config.gossip_mesh_hi)
        .mesh_outbound_min(mesh_outbound)
        .max_transmit_size(4 * 1024 * 1024) // 4 MB (PQ-signed batches can be large)
        .heartbeat_interval(Duration::from_secs(1))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()
        .map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!(
                "invalid gossipsub config: {e}"
            )))
        })?;

    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(key.clone()),
        gossipsub_config,
    )
    .map_err(|e| {
        NetworkError::Io(std::io::Error::other(format!(
            "invalid gossipsub behaviour: {e}"
        )))
    })?;

    // ── Kademlia ─────────────────────────────────────────────────────────
    let local_peer_id = key.public().to_peer_id();
    let store = kad::store::MemoryStore::new(local_peer_id);
    let mut kademlia = kad::Behaviour::new(local_peer_id, store);
    kademlia.set_mode(Some(kad::Mode::Server));

    // ── Identify ─────────────────────────────────────────────────────────
    let identify = identify::Behaviour::new(identify::Config::new(
        "/nexus/id/1.0".to_string(),
        key.public(),
    ));

    // ── Request-Response ─────────────────────────────────────────────────
    let rr_config = request_response::Config::default();
    let reqres = request_response::Behaviour::new(
        [(
            NEXUS_RR_PROTOCOL.to_string(),
            request_response::ProtocolSupport::Full,
        )],
        rr_config,
    );

    let behaviour = NexusBehaviour {
        gossipsub,
        kademlia,
        identify,
        reqres,
    };

    Ok(behaviour)
}

// ── Identity Keypair Persistence ─────────────────────────────────────────────

/// Load an Ed25519 keypair from disk, or generate a new one and save it.
///
/// If `config.identity_key_path` is `None`, generates an ephemeral keypair.
/// When saving, the file is written with restrictive permissions (0600 on Unix).
fn load_or_generate_keypair(config: &NetworkConfig) -> NetworkResult<Keypair> {
    let path = match &config.identity_key_path {
        Some(p) => p,
        None => {
            info!("no identity_key_path set — using ephemeral keypair");
            return Ok(Keypair::generate_ed25519());
        }
    };

    if path.exists() {
        // Load existing keypair
        let bytes = std::fs::read(path).map_err(|e| {
            NetworkError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read identity key file {}: {e}", path.display()),
            ))
        })?;

        let keypair = Keypair::from_protobuf_encoding(&bytes).map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!(
                "failed to decode identity key: {e}"
            )))
        })?;

        info!(path = %path.display(), "loaded persistent identity keypair");
        Ok(keypair)
    } else {
        // Generate new keypair and save
        let keypair = Keypair::generate_ed25519();
        let encoded = keypair.to_protobuf_encoding().map_err(|e| {
            NetworkError::Io(std::io::Error::other(format!(
                "failed to encode identity key: {e}"
            )))
        })?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                NetworkError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "failed to create directory for identity key {}: {e}",
                        parent.display()
                    ),
                ))
            })?;
        }

        std::fs::write(path, &encoded).map_err(|e| {
            NetworkError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to write identity key file {}: {e}", path.display()),
            ))
        })?;

        // Set restrictive permissions on Unix (owner-only read/write)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| {
                NetworkError::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "failed to set permissions on identity key {}: {e}",
                        path.display()
                    ),
                ))
            })?;
        }

        info!(path = %path.display(), "generated and saved new identity keypair");
        Ok(keypair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_swarm_succeeds_with_test_config() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = NetworkConfig::for_testing();
            let swarm = build_swarm(&config);
            assert!(swarm.is_ok(), "swarm should build: {:?}", swarm.err());
        });
    }

    #[test]
    fn behaviour_contains_all_protocols() {
        let key = Keypair::generate_ed25519();
        let config = NetworkConfig::for_testing();
        let behaviour = build_behaviour(&key, &config).expect("should build");
        // Smoke test: accessing fields shouldn't panic
        let _ = &behaviour.gossipsub;
        let _ = &behaviour.kademlia;
        let _ = &behaviour.identify;
    }

    #[test]
    fn ephemeral_keypair_when_no_path() {
        let config = NetworkConfig::for_testing();
        assert!(config.identity_key_path.is_none());
        let kp = load_or_generate_keypair(&config).expect("should generate");
        assert!(kp.public().to_peer_id().to_string().starts_with("12D3"));
    }

    #[test]
    fn persistent_keypair_creates_and_reloads() {
        let dir = std::env::temp_dir().join("nexus-identity-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("identity.key");

        let mut config = NetworkConfig::for_testing();
        config.identity_key_path = Some(key_path.clone());

        // First call: generates and saves
        let kp1 = load_or_generate_keypair(&config).expect("should generate");
        assert!(key_path.exists(), "key file should be created");

        // Verify permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "key file should be owner-only");
        }

        // Second call: loads the same keypair
        let kp2 = load_or_generate_keypair(&config).expect("should reload");
        assert_eq!(
            kp1.public().to_peer_id(),
            kp2.public().to_peer_id(),
            "PeerId should be stable across restarts"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_key_file_returns_error() {
        let dir = std::env::temp_dir().join("nexus-identity-corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("identity.key");
        std::fs::write(&key_path, b"not-a-valid-protobuf").unwrap();

        let mut config = NetworkConfig::for_testing();
        config.identity_key_path = Some(key_path);

        let result = load_or_generate_keypair(&config);
        assert!(result.is_err(), "corrupt key should fail");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
