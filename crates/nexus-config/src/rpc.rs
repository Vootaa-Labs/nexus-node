//! RPC / API gateway configuration.
//!
//! Addresses and limits for gRPC, REST, GraphQL, WebSocket, and MCP
//! API endpoints exposed by the Nexus node.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus RPC subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RpcConfig {
    /// gRPC listen address. Default: `0.0.0.0:50051`.
    pub grpc_listen_addr: SocketAddr,

    /// REST API listen address. Default: `0.0.0.0:8080`.
    pub rest_listen_addr: SocketAddr,

    /// GraphQL listen address. Default: `0.0.0.0:8081`.
    pub graphql_listen_addr: SocketAddr,

    /// WebSocket listen address. Default: `0.0.0.0:8082`.
    pub ws_listen_addr: SocketAddr,

    /// MCP (Model Context Protocol) listen address. Default: `0.0.0.0:8083`.
    pub mcp_listen_addr: SocketAddr,

    /// Path to the TLS certificate file. `None` disables TLS.
    pub tls_cert_path: Option<PathBuf>,

    /// Path to the TLS private key file.
    pub tls_key_path: Option<PathBuf>,

    /// Global request rate limit (requests per second). Default: 1000.
    pub rate_limit_rps: u32,

    /// Whether global per-IP RPC rate limiting is enabled.
    pub rate_limit_enabled: bool,

    /// Per-IP request rate limit (requests per second). Default: 100.
    pub rate_limit_per_ip_rps: u32,

    /// Maximum concurrent WebSocket connections. Default: 10_000.
    pub max_ws_connections: usize,

    /// GraphQL maximum query depth. Default: 10.
    pub graphql_max_depth: usize,

    /// gRPC maximum message size in bytes. Default: 4_194_304 (4 MiB).
    pub grpc_max_message_size: usize,

    /// Enable the `/v2/faucet/mint` endpoint (dev/testnet only). Default: false.
    pub faucet_enabled: bool,

    /// Amount of smallest-unit tokens dispensed per faucet request. Default: 10^18 (1 NXS).
    pub faucet_amount: u64,

    /// Optional API keys for authenticated access.
    ///
    /// When non-empty, mutating endpoints (POST) require an `x-api-key` header
    /// matching one of these values.  When empty, authentication is disabled.
    pub api_keys: Vec<String>,

    /// Per-address faucet rate limit (requests per hour). Default: 10.
    pub faucet_per_addr_limit_per_hour: u32,

    /// Allowed CORS origins for the REST API.\n    ///\n    /// When empty, **no cross-origin requests are allowed** (fail-closed,\n    /// SEC-M14).  To allow all origins during local development, pass\n    /// `[\"*\"]`.  For production, list explicit origins such as\n    /// `[\"https://explorer.nexus.dev\"]`.
    pub cors_allowed_origins: Vec<String>,

    /// Assert that TLS is terminated by a trusted reverse proxy (e.g. nginx,
    /// envoy, cloud load balancer) in front of this node.
    ///
    /// When `true`, the node is allowed to bind non-loopback addresses
    /// without native TLS (`tls_cert_path` / `tls_key_path`). The operator
    /// takes responsibility for ensuring end-to-end encryption.
    ///
    /// Default: `false`.
    pub reverse_proxy_tls: bool,

    // ── Quota tier configuration (D-2) ──────────────────────────────
    /// Per-IP request rate limit for **anonymous** callers (no API key)
    /// on query / intent / MCP paths (requests per minute). Default: 60.
    pub query_rate_limit_anonymous_rpm: u32,

    /// Per-IP request rate limit for **authenticated** callers (valid API key)
    /// on query / intent / MCP paths (requests per minute). Default: 600.
    pub query_rate_limit_authenticated_rpm: u32,

    /// Per-IP request rate limit for **whitelisted** callers on
    /// query / intent / MCP paths (requests per minute). Default: 3000.
    pub query_rate_limit_whitelisted_rpm: u32,

    /// API keys that belong to the whitelisted tier (higher quota).
    ///
    /// These should be a subset of `api_keys`.  Keys present in `api_keys`
    /// but absent here receive the `authenticated` tier.
    pub whitelisted_api_keys: Vec<String>,

    // ── Per-endpoint-class quota (E-2) ──────────────────────────────
    /// Per-IP intent-submit rate limit for **anonymous** callers
    /// (requests per minute). Default: 30.
    pub intent_rate_limit_anonymous_rpm: u32,

    /// Per-IP intent-submit rate limit for **authenticated** callers
    /// (requests per minute). Default: 300.
    pub intent_rate_limit_authenticated_rpm: u32,

    /// Per-IP intent-submit rate limit for **whitelisted** callers
    /// (requests per minute). Default: 1500.
    pub intent_rate_limit_whitelisted_rpm: u32,

    /// Per-IP MCP rate limit for **anonymous** callers
    /// (requests per minute). Default: 30.
    pub mcp_rate_limit_anonymous_rpm: u32,

    /// Per-IP MCP rate limit for **authenticated** callers
    /// (requests per minute). Default: 300.
    pub mcp_rate_limit_authenticated_rpm: u32,

    /// Per-IP MCP rate limit for **whitelisted** callers
    /// (requests per minute). Default: 1500.
    pub mcp_rate_limit_whitelisted_rpm: u32,

    // ── View query budget and timeout (D-3) ─────────────────────────
    /// Maximum gas budget allowed for a single read-only view query.
    /// Queries whose estimated gas exceeds this budget are rejected.
    /// Default: 10_000_000 (10 M gas units).
    pub query_gas_budget: u64,

    /// Timeout for a single read-only view query (milliseconds).
    /// Default: 5000 (5 seconds).
    pub query_timeout_ms: u64,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            grpc_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 50051)),
            rest_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080)),
            graphql_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8081)),
            ws_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8082)),
            mcp_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8083)),
            tls_cert_path: None,
            tls_key_path: None,
            rate_limit_rps: 1000,
            rate_limit_enabled: true,
            rate_limit_per_ip_rps: 100,
            max_ws_connections: 10_000,
            graphql_max_depth: 10,
            grpc_max_message_size: 4_194_304, // 4 MiB
            faucet_enabled: false,
            faucet_amount: 1_000_000_000, // 10^9 voo = 1 NXS
            api_keys: vec![],
            faucet_per_addr_limit_per_hour: 10,
            cors_allowed_origins: vec![],
            reverse_proxy_tls: false,
            query_rate_limit_anonymous_rpm: 60,
            query_rate_limit_authenticated_rpm: 600,
            query_rate_limit_whitelisted_rpm: 3_000,
            whitelisted_api_keys: vec![],
            intent_rate_limit_anonymous_rpm: 30,
            intent_rate_limit_authenticated_rpm: 300,
            intent_rate_limit_whitelisted_rpm: 1_500,
            mcp_rate_limit_anonymous_rpm: 30,
            mcp_rate_limit_authenticated_rpm: 300,
            mcp_rate_limit_whitelisted_rpm: 1_500,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
        }
    }
}

impl RpcConfig {
    /// Validate the RPC configuration.
    ///
    /// Returns an error if any constraint is violated.
    pub fn validate(&self) -> Result<(), String> {
        if self.rate_limit_enabled && self.rate_limit_per_ip_rps == 0 {
            return Err("rate_limit_per_ip_rps must be > 0".into());
        }
        if self.rate_limit_enabled && self.rate_limit_rps == 0 {
            return Err("rate_limit_rps must be > 0".into());
        }
        if self.max_ws_connections == 0 {
            return Err("max_ws_connections must be > 0".into());
        }
        if self.graphql_max_depth == 0 {
            return Err("graphql_max_depth must be > 0".into());
        }
        if self.grpc_max_message_size == 0 {
            return Err("grpc_max_message_size must be > 0".into());
        }
        // API keys must not be empty strings.
        for (i, key) in self.api_keys.iter().enumerate() {
            if key.is_empty() {
                return Err(format!("api_keys[{i}] must not be empty"));
            }
            if key.len() < 16 {
                return Err(format!(
                    "api_keys[{i}] too short ({} bytes); minimum 16",
                    key.len()
                ));
            }
        }
        // TLS: if either cert or key is set, both must be.
        match (&self.tls_cert_path, &self.tls_key_path) {
            (Some(_), None) | (None, Some(_)) => {
                return Err("tls_cert_path and tls_key_path must be set together".into());
            }
            _ => {}
        }
        // API keys require TLS to avoid cleartext credential transmission.
        if !self.api_keys.is_empty() && self.tls_cert_path.is_none() {
            return Err("api_keys require TLS: set tls_cert_path and tls_key_path".into());
        }

        // SEC-C2: enforce TLS boundary on non-loopback addresses.
        // In production, RPC must not serve plaintext to the network.
        let all_addrs = [
            self.grpc_listen_addr,
            self.rest_listen_addr,
            self.graphql_listen_addr,
            self.ws_listen_addr,
            self.mcp_listen_addr,
        ];
        let has_non_loopback = all_addrs.iter().any(|a| !a.ip().is_loopback());
        let has_native_tls = self.tls_cert_path.is_some() && self.tls_key_path.is_some();
        if has_non_loopback && !has_native_tls && !self.reverse_proxy_tls {
            return Err("SEC-C2: RPC binds to a non-loopback address without TLS. \
                 Either set tls_cert_path + tls_key_path for native TLS, \
                 or set reverse_proxy_tls = true if a trusted reverse proxy \
                 terminates TLS upstream."
                .into());
        }
        // Faucet amount must not exceed 1 NXS (10^9 voo).
        if self.faucet_amount > 1_000_000_000 {
            return Err(format!(
                "faucet_amount must be <= 10^9 voo (1 NXS), got {}",
                self.faucet_amount
            ));
        }

        // D-2: quota tier validation
        if self.query_rate_limit_anonymous_rpm == 0 {
            return Err("query_rate_limit_anonymous_rpm must be > 0".into());
        }
        if self.query_rate_limit_authenticated_rpm == 0 {
            return Err("query_rate_limit_authenticated_rpm must be > 0".into());
        }
        if self.query_rate_limit_whitelisted_rpm == 0 {
            return Err("query_rate_limit_whitelisted_rpm must be > 0".into());
        }
        // D-3: query budget / timeout validation
        if self.query_gas_budget == 0 {
            return Err("query_gas_budget must be > 0".into());
        }
        if self.query_timeout_ms == 0 {
            return Err("query_timeout_ms must be > 0".into());
        }

        Ok(())
    }

    /// Minimal configuration suitable for tests.
    ///
    /// All endpoints bind to `127.0.0.1:0` (OS-assigned port), no TLS.
    pub fn for_testing() -> Self {
        Self {
            grpc_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            rest_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            graphql_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            ws_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            mcp_listen_addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            tls_cert_path: None,
            tls_key_path: None,
            rate_limit_rps: 10_000,
            rate_limit_enabled: false,
            rate_limit_per_ip_rps: 10_000,
            max_ws_connections: 64,
            graphql_max_depth: 5,
            grpc_max_message_size: 1_048_576, // 1 MiB
            faucet_enabled: true,             // Tests always have faucet
            faucet_amount: 1_000_000_000,
            api_keys: vec![],
            faucet_per_addr_limit_per_hour: 10_000,
            cors_allowed_origins: vec!["*".to_owned()], // Tests allow all origins
            reverse_proxy_tls: false,
            query_rate_limit_anonymous_rpm: 60,
            query_rate_limit_authenticated_rpm: 600,
            query_rate_limit_whitelisted_rpm: 3_000,
            whitelisted_api_keys: vec![],
            intent_rate_limit_anonymous_rpm: 30,
            intent_rate_limit_authenticated_rpm: 300,
            intent_rate_limit_whitelisted_rpm: 1_500,
            mcp_rate_limit_anonymous_rpm: 30,
            mcp_rate_limit_authenticated_rpm: 300,
            mcp_rate_limit_whitelisted_rpm: 1_500,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = RpcConfig::default();
        assert_eq!(cfg.grpc_listen_addr.port(), 50051);
        assert_eq!(cfg.rest_listen_addr.port(), 8080);
        assert_eq!(cfg.rate_limit_rps, 1000);
        assert_eq!(cfg.graphql_max_depth, 10);
        assert_eq!(cfg.grpc_max_message_size, 4_194_304);
        assert!(cfg.tls_cert_path.is_none());
    }

    #[test]
    fn testing_config_uses_localhost() {
        let cfg = RpcConfig::for_testing();
        assert_eq!(cfg.grpc_listen_addr.port(), 0);
        assert_eq!(cfg.rest_listen_addr.port(), 0);
        assert!(cfg.grpc_listen_addr.ip().is_loopback());
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = RpcConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: RpcConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.grpc_listen_addr, cfg.grpc_listen_addr);
        assert_eq!(restored.rate_limit_rps, cfg.rate_limit_rps);
    }

    #[test]
    fn validate_default_rejects_non_loopback_without_tls() {
        // SEC-C2: default binds to 0.0.0.0 without TLS — must be rejected.
        let err = RpcConfig::default().validate().unwrap_err();
        assert!(err.contains("SEC-C2"), "expected SEC-C2 error, got: {err}");
    }

    #[test]
    fn validate_non_loopback_with_reverse_proxy_ok() {
        let cfg = RpcConfig {
            reverse_proxy_tls: true,
            ..RpcConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_non_loopback_with_native_tls_ok() {
        let cfg = RpcConfig {
            tls_cert_path: Some("/tmp/cert.pem".into()),
            tls_key_path: Some("/tmp/key.pem".into()),
            ..RpcConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_for_testing_ok() {
        RpcConfig::for_testing().validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_rate_limit() {
        let cfg = RpcConfig {
            rate_limit_per_ip_rps: 0,
            ..RpcConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_short_api_key() {
        let cfg = RpcConfig {
            api_keys: vec!["short".to_string()],
            ..RpcConfig::default()
        };
        assert!(cfg.validate().unwrap_err().contains("too short"));
    }

    #[test]
    fn validate_rejects_half_tls() {
        let cfg = RpcConfig {
            tls_cert_path: Some("/tmp/cert.pem".into()),
            ..RpcConfig::default()
        };
        assert!(cfg.validate().unwrap_err().contains("together"));
    }

    #[test]
    fn validate_rejects_api_keys_without_tls() {
        let cfg = RpcConfig {
            api_keys: vec!["a]b]c]d]e]f]g]h]i]j]k]l]m]n]o]p]".to_string()],
            ..RpcConfig::default()
        };
        assert!(cfg.validate().unwrap_err().contains("require TLS"));
    }

    #[test]
    fn validate_accepts_api_keys_with_tls() {
        let cfg = RpcConfig {
            api_keys: vec!["a]b]c]d]e]f]g]h]i]j]k]l]m]n]o]p]".to_string()],
            tls_cert_path: Some("/tmp/cert.pem".into()),
            tls_key_path: Some("/tmp/key.pem".into()),
            ..RpcConfig::default()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_excessive_faucet_amount() {
        let cfg = RpcConfig {
            faucet_amount: u64::MAX,
            ..RpcConfig::for_testing()
        };
        assert!(cfg.validate().unwrap_err().contains("faucet_amount"));
    }
}
