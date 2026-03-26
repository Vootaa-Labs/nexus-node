//! Intent layer metrics (MVO Rule C — DEV-05 §3).
//!
//! Naming convention: `nexus_intent_<metric>_<unit>`.
//!
//! # Metric Catalogue
//!
//! | Name | Type | Description |
//! |------|------|-------------|
//! | `nexus_intent_compilations_total` | Counter | Successfully compiled intents |
//! | `nexus_intent_compilation_failures_total` | Counter | Failed compilations |
//! | `nexus_intent_compilation_latency_seconds` | Histogram | End-to-end compile time |
//! | `nexus_intent_steps_per_plan` | Histogram | Steps in each compiled plan |
//! | `nexus_intent_gas_estimated_total` | Counter | Cumulative estimated gas |
//! | `nexus_intent_cross_shard_plans_total` | Counter | Plans requiring HTLC |
//! | `nexus_intent_agent_tasks_total` | Counter | Agent-submitted intents |
//! | `nexus_intent_mailbox_depth` | Gauge | Service actor mailbox backlog |
//! | `nexus_intent_validation_failures_total` | Counter | Pre-compilation validation errors |

/// Intent layer metrics handle.
///
/// All metric operations are cheap — labels are pre-allocated.
/// `Clone + Send + Sync` via the global `metrics` recorder.
#[derive(Clone, Debug)]
pub struct IntentMetrics {
    /// Optional scope label (e.g. test isolation).
    scope: String,
}

impl IntentMetrics {
    /// Create a new metrics handle.
    pub fn new() -> Self {
        Self {
            scope: String::new(),
        }
    }

    /// Create a labelled metrics handle (useful for tests or multi-instance setups).
    pub fn with_scope(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
        }
    }

    fn labels(&self) -> Vec<(&'static str, String)> {
        if self.scope.is_empty() {
            vec![]
        } else {
            vec![("scope", self.scope.clone())]
        }
    }

    // ── Compilation counters ────────────────────────────────────────

    /// Record a successful compilation with timing and plan metadata.
    pub fn record_compilation(
        &self,
        elapsed_secs: f64,
        step_count: usize,
        estimated_gas: u64,
        is_cross_shard: bool,
    ) {
        let labels = self.labels();
        metrics::counter!("nexus_intent_compilations_total", &labels).increment(1);
        metrics::histogram!("nexus_intent_compilation_latency_seconds", &labels)
            .record(elapsed_secs);
        metrics::histogram!("nexus_intent_steps_per_plan", &labels).record(step_count as f64);
        metrics::counter!("nexus_intent_gas_estimated_total", &labels).increment(estimated_gas);
        if is_cross_shard {
            metrics::counter!("nexus_intent_cross_shard_plans_total", &labels).increment(1);
        }
    }

    /// Record a failed compilation.
    pub fn record_compilation_failure(&self, error_type: &str) {
        let mut labels = self.labels();
        labels.push(("error", error_type.to_owned()));
        metrics::counter!("nexus_intent_compilation_failures_total", &labels).increment(1);
    }

    /// Record a validation failure (before compilation).
    pub fn record_validation_failure(&self, error_type: &str) {
        let mut labels = self.labels();
        labels.push(("error", error_type.to_owned()));
        metrics::counter!("nexus_intent_validation_failures_total", &labels).increment(1);
    }

    // ── Agent counters ──────────────────────────────────────────────

    /// Record an agent-submitted intent.
    pub fn record_agent_task(&self) {
        let labels = self.labels();
        metrics::counter!("nexus_intent_agent_tasks_total", &labels).increment(1);
    }

    // ── Gauges ──────────────────────────────────────────────────────

    /// Set the service mailbox depth gauge.
    pub fn set_mailbox_depth(&self, depth: usize) {
        let labels = self.labels();
        metrics::gauge!("nexus_intent_mailbox_depth", &labels).set(depth as f64);
    }
}

impl Default for IntentMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metrics calls must not panic even without a recorder installed.
    #[test]
    fn record_compilation_no_panic() {
        let m = IntentMetrics::new();
        m.record_compilation(0.025, 3, 50_000, true);
    }

    #[test]
    fn record_failure_no_panic() {
        let m = IntentMetrics::new();
        m.record_compilation_failure("InvalidSignature");
    }

    #[test]
    fn record_validation_failure_no_panic() {
        let m = IntentMetrics::new();
        m.record_validation_failure("IntentTooLarge");
    }

    #[test]
    fn record_agent_task_no_panic() {
        let m = IntentMetrics::new();
        m.record_agent_task();
    }

    #[test]
    fn set_mailbox_depth_no_panic() {
        let m = IntentMetrics::new();
        m.set_mailbox_depth(42);
    }

    #[test]
    fn with_scope_labels() {
        let m = IntentMetrics::with_scope("test-1");
        // Just verify it doesn't panic and the scope is set.
        assert_eq!(m.scope, "test-1");
        m.record_compilation(0.01, 1, 10_000, false);
    }

    #[test]
    fn default_impl() {
        let m = IntentMetrics::default();
        assert!(m.scope.is_empty());
    }
}
