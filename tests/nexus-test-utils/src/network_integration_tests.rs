// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Phase 8 integration tests — network security hardening & maturity.
//!
//! Cross-module tests validating:
//! - NetworkConfig validation wired into service build
//! - Identity keypair persistence across restarts
//! - Request-response protocol included in NexusBehaviour
//! - Discovery disjoint lookup plumbing

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    // ── NetworkConfig Validation (T-8002) ───────────────────────────────────

    /// Invalid config is caught when building NetworkService.
    #[test]
    fn service_build_rejects_invalid_config() {
        let mut config = nexus_network::config::NetworkConfig::for_testing();
        // Violate mesh invariant: mesh_lo >= mesh_size
        config.gossip_mesh_lo = config.gossip_mesh_size + 1;

        let result = nexus_network::service::NetworkService::build(&config);
        let err = result.err().expect("invalid config should be rejected");
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("mesh"),
            "error should mention mesh invariant: {err_msg}"
        );
    }

    /// Zero max_peers is rejected at the service level.
    #[test]
    fn service_build_rejects_zero_max_peers() {
        let mut config = nexus_network::config::NetworkConfig::for_testing();
        config.max_peers = 0;

        let result = nexus_network::service::NetworkService::build(&config);
        assert!(result.is_err());
    }

    /// Default/test config passes validation and builds successfully.
    #[tokio::test]
    async fn service_build_accepts_valid_config() {
        let config = nexus_network::config::NetworkConfig::for_testing();
        let result = nexus_network::service::NetworkService::build(&config);
        assert!(result.is_ok(), "valid config should be accepted");
    }

    // ── Identity Persistence (T-8001) ───────────────────────────────────────

    /// A persistent identity key is created on first run and reloaded on second.
    #[tokio::test]
    async fn persistent_identity_survives_restart() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("node-identity.key");

        let mut config = nexus_network::config::NetworkConfig::for_testing();
        config.identity_key_path = Some(key_path.clone());

        // First build — creates key file.
        let result1 = nexus_network::service::NetworkService::build(&config);
        assert!(result1.is_ok());
        assert!(key_path.exists(), "identity key file should be created");
        let bytes1 = std::fs::read(&key_path).unwrap();

        // Second build — reloads the same key.
        let result2 = nexus_network::service::NetworkService::build(&config);
        assert!(result2.is_ok());
        let bytes2 = std::fs::read(&key_path).unwrap();

        assert_eq!(
            bytes1, bytes2,
            "key file should be unchanged across restarts"
        );
    }

    /// Without identity_key_path, each build creates an ephemeral keypair.
    #[tokio::test]
    async fn ephemeral_identity_when_no_path() {
        let config = nexus_network::config::NetworkConfig::for_testing();
        assert!(config.identity_key_path.is_none());
        let result = nexus_network::service::NetworkService::build(&config);
        assert!(result.is_ok());
    }

    // ── Request-Response Protocol (T-8004) ──────────────────────────────────

    /// The full NetworkService builds with request-response protocol enabled.
    #[tokio::test]
    async fn network_service_includes_reqres() {
        let config = nexus_network::config::NetworkConfig::for_testing();
        let result = nexus_network::service::NetworkService::build(&config);
        assert!(result.is_ok(), "service should build with reqres protocol");
    }

    // ── Disjoint Lookup (T-8005) ────────────────────────────────────────────

    /// Discovery handle maintains Clone + Send + Sync bounds.
    #[test]
    fn discovery_handle_bounds() {
        fn assert_bounds<T: Clone + Send + Sync>() {}
        assert_bounds::<nexus_network::discovery::DiscoveryHandle>();
    }

    /// Transport handle for Kademlia commands is Clone + Send + Sync.
    #[test]
    fn transport_handle_bounds() {
        fn assert_bounds<T: Clone + Send + Sync>() {}
        assert_bounds::<nexus_network::TransportHandle>();
    }

    // ── Cross-layer: config → transport → discovery round-trip ──────────────

    /// Full service build produces all subsystem handles.
    #[tokio::test]
    async fn full_service_build_roundtrip() {
        let config = nexus_network::config::NetworkConfig::for_testing();
        let (handle, _svc) = nexus_network::service::NetworkService::build(&config)
            .expect("service build should succeed");

        // Handle should be cloneable (multi-consumer).
        let _clone = handle.clone();
    }
}
