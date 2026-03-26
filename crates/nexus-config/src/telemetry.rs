// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Telemetry and observability configuration.
//!
//! Controls logging verbosity, OpenTelemetry export targets, and
//! Prometheus metrics exposure.

use serde::{Deserialize, Serialize};

/// Configuration for the Nexus observability subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Log verbosity level. Accepts `tracing` directives such as
    /// `"info"`, `"debug"`, `"nexus_consensus=trace,info"`. Default: `"info"`.
    pub log_level: String,

    /// Optional OpenTelemetry OTLP exporter endpoint (e.g. `http://localhost:4317`).
    /// `None` disables OTLP export.
    pub otlp_endpoint: Option<String>,

    /// Whether to emit logs in JSON format. Default: false.
    pub json_logs: bool,

    /// Whether an embedded Prometheus metrics endpoint is enabled. Default: true.
    pub prometheus_enabled: bool,

    /// Listen address for the Prometheus metrics endpoint. Default: `0.0.0.0:9090`.
    pub prometheus_addr: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_owned(),
            otlp_endpoint: None,
            json_logs: false,
            prometheus_enabled: true,
            prometheus_addr: "0.0.0.0:9090".to_owned(),
        }
    }
}

impl TelemetryConfig {
    /// Minimal configuration suitable for tests.
    pub fn for_testing() -> Self {
        Self {
            log_level: "warn".to_owned(),
            otlp_endpoint: None,
            json_logs: false,
            prometheus_enabled: false,
            prometheus_addr: "127.0.0.1:0".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = TelemetryConfig::default();
        assert_eq!(cfg.log_level, "info");
        assert!(cfg.otlp_endpoint.is_none());
        assert!(!cfg.json_logs);
        assert!(cfg.prometheus_enabled);
    }

    #[test]
    fn testing_config_is_quiet() {
        let cfg = TelemetryConfig::for_testing();
        assert_eq!(cfg.log_level, "warn");
        assert!(!cfg.prometheus_enabled);
    }

    #[test]
    fn serialization_roundtrip() {
        let cfg = TelemetryConfig::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let restored: TelemetryConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.log_level, cfg.log_level);
        assert_eq!(restored.prometheus_enabled, cfg.prometheus_enabled);
    }
}
