// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Background cleanup task for expired sessions and old provenance records.
//!
//! Runs on a configurable interval, removing:
//! - Terminal sessions older than `session_ttl_ms`
//! - Provenance records older than `provenance_retention_ms`

use std::sync::Arc;

use nexus_intent::{RocksProvenanceStore, RocksSessionStore};
use nexus_storage::traits::StateStorage;

/// Configuration for the session/provenance cleanup task.
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// How often to run cleanup (milliseconds).
    pub interval_ms: u64,
    /// TTL for terminal sessions (milliseconds). Sessions in terminal state
    /// (Finalized/Aborted/Expired) older than this are deleted.
    /// Default: 24 hours.
    pub session_ttl_ms: u64,
    /// Retention period for provenance records (milliseconds).
    /// Records older than this are deleted.
    /// Default: 30 days. Set to 0 to disable provenance cleanup.
    pub provenance_retention_ms: u64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            interval_ms: 60 * 60 * 1_000,                       // 1 hour
            session_ttl_ms: 24 * 60 * 60 * 1_000,               // 24 hours
            provenance_retention_ms: 30 * 24 * 60 * 60 * 1_000, // 30 days
        }
    }
}

/// Spawn a background task that periodically cleans up expired sessions
/// and old provenance records.
///
/// Returns a `JoinHandle` for the spawned task.
pub fn spawn_cleanup_task<S: StateStorage + Send + Sync + 'static>(
    config: CleanupConfig,
    session_store: Arc<RocksSessionStore<S>>,
    provenance_store: Arc<RocksProvenanceStore<S>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = tokio::time::Duration::from_millis(config.interval_ms);
        let mut ticker = tokio::time::interval(interval);
        // Skip the first immediate tick.
        ticker.tick().await;

        loop {
            ticker.tick().await;

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            // Clean up expired sessions.
            if config.session_ttl_ms > 0 {
                let cutoff = now_ms.saturating_sub(config.session_ttl_ms);
                match session_store.cleanup_expired(cutoff).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(cleaned = n, "session cleanup: removed expired sessions")
                    }
                    Err(e) => tracing::warn!(error = %e, "session cleanup failed"),
                }
            }

            // Clean up old provenance records.
            if config.provenance_retention_ms > 0 {
                let cutoff = now_ms.saturating_sub(config.provenance_retention_ms);
                match provenance_store.cleanup_before(cutoff).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(cleaned = n, "provenance cleanup: removed old records"),
                    Err(e) => tracing::warn!(error = %e, "provenance cleanup failed"),
                }
            }
        }
    })
}
