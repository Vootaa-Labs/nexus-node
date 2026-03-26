//! Node readiness state machine.
//!
//! Tracks the health of each subsystem and derives an aggregate node
//! status used by `/health` and `/ready` endpoints. All state is
//! lock-free via [`AtomicU8`] — writers (subsystem tasks) and readers
//! (RPC handlers) never contend on a mutex.
//!
//! # Status lifecycle
//!
//! ```text
//! Bootstrapping ──► Healthy ◄──► Degraded
//!       │                            │
//!       └──────────► Halted ◄────────┘
//!                      │
//!                Syncing (consensus catching up)
//! ```

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

// ── Subsystem status ────────────────────────────────────────────────────

/// Health state of an individual subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SubsystemStatus {
    /// Subsystem has not finished initialisation.
    Starting = 0,
    /// Subsystem is operating normally.
    Ready = 1,
    /// Subsystem is running but in a reduced-capability state.
    Degraded = 2,
    /// Subsystem is down or unreachable.
    Down = 3,
}

impl SubsystemStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Starting,
            1 => Self::Ready,
            2 => Self::Degraded,
            3 => Self::Down,
            _ => Self::Down,
        }
    }

    /// Return the human-readable label used in JSON responses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }
}

// ── Aggregate node status ───────────────────────────────────────────────

/// Top-level node health derived from subsystem states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Node is still starting up — one or more critical subsystems are
    /// in [`SubsystemStatus::Starting`].
    Bootstrapping,
    /// Consensus is catching up with the network.
    Syncing,
    /// All subsystems are [`SubsystemStatus::Ready`].
    Healthy,
    /// At least one non-critical subsystem is degraded, but the node
    /// can still serve traffic.
    Degraded,
    /// A critical subsystem is down — the node should not serve traffic.
    Halted,
}

impl NodeStatus {
    /// Return the status string compatible with `HealthResponse.status`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bootstrapping => "bootstrapping",
            Self::Syncing => "syncing",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Halted => "halted",
        }
    }

    /// Whether the node should accept external traffic.
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Healthy | Self::Degraded)
    }
}

// ── Subsystem handle ────────────────────────────────────────────────────

/// Write-side handle for a single subsystem. Subsystem tasks hold this
/// and update their status as their health changes.
#[derive(Clone)]
pub struct SubsystemHandle {
    inner: Arc<AtomicU8>,
    /// Epoch-millis timestamp of the last progress report. Shared
    /// reference so readers (health endpoint) can inspect staleness.
    last_progress_ms: Arc<AtomicU64>,
    /// Shared reference to the node-wide monotonic clock origin.
    epoch_origin: Arc<Instant>,
}

impl SubsystemHandle {
    /// Report that the subsystem is operating normally.
    pub fn set_ready(&self) {
        self.inner
            .store(SubsystemStatus::Ready as u8, Ordering::Release);
        self.touch_progress();
    }

    /// Report degraded operation.
    pub fn set_degraded(&self) {
        self.inner
            .store(SubsystemStatus::Degraded as u8, Ordering::Release);
        self.touch_progress();
    }

    /// Report that the subsystem is down.
    pub fn set_down(&self) {
        self.inner
            .store(SubsystemStatus::Down as u8, Ordering::Release);
        self.touch_progress();
    }

    /// Record forward progress without changing the health status.
    ///
    /// Subsystem loops should call this periodically so the readiness
    /// tracker can distinguish "healthy and active" from "healthy but
    /// stalled".
    pub fn report_progress(&self) {
        self.touch_progress();
    }

    /// Read current status.
    pub fn status(&self) -> SubsystemStatus {
        SubsystemStatus::from_u8(self.inner.load(Ordering::Acquire))
    }

    /// Milliseconds elapsed since last progress report.
    pub fn ms_since_last_progress(&self) -> u64 {
        let now = self.epoch_origin.elapsed().as_millis() as u64;
        let last = self.last_progress_ms.load(Ordering::Acquire);
        now.saturating_sub(last)
    }

    fn touch_progress(&self) {
        // Store at least 1 so 0 always means "never reported".
        let now = self.epoch_origin.elapsed().as_millis() as u64;
        self.last_progress_ms.store(now.max(1), Ordering::Release);
    }
}

// ── Node readiness tracker ──────────────────────────────────────────────

/// Named subsystem entry for snapshot reporting.
#[derive(Debug, Clone, Serialize)]
pub struct SubsystemSnapshot {
    /// Human-readable subsystem name.
    pub name: &'static str,
    /// Current status string.
    pub status: &'static str,
    /// Milliseconds since this subsystem last reported progress.
    /// `0` means the subsystem has never reported progress (still starting).
    pub last_progress_ms: u64,
}

/// Shared readiness tracker for the entire node.
///
/// Created once during startup and shared (via `Arc`) with subsystem
/// tasks (writers) and the query backend (reader).
#[derive(Clone)]
pub struct NodeReadiness {
    storage: Arc<AtomicU8>,
    network: Arc<AtomicU8>,
    consensus: Arc<AtomicU8>,
    execution: Arc<AtomicU8>,
    genesis: Arc<AtomicU8>,
    // Per-subsystem last-progress timestamps (epoch millis from clock origin).
    storage_progress: Arc<AtomicU64>,
    network_progress: Arc<AtomicU64>,
    consensus_progress: Arc<AtomicU64>,
    execution_progress: Arc<AtomicU64>,
    genesis_progress: Arc<AtomicU64>,
    /// Monotonic clock origin shared across all handles.
    clock_origin: Arc<Instant>,
    /// Threshold in milliseconds — a subsystem with no progress for
    /// longer than this is considered stalled. Default `30_000` (30 s).
    stall_threshold_ms: u64,
}

/// Default stall-detection threshold (30 seconds).
const DEFAULT_STALL_THRESHOLD_MS: u64 = 30_000;

impl NodeReadiness {
    /// Create a new tracker with all subsystems in [`SubsystemStatus::Starting`].
    pub fn new() -> Self {
        Self {
            storage: Arc::new(AtomicU8::new(SubsystemStatus::Starting as u8)),
            network: Arc::new(AtomicU8::new(SubsystemStatus::Starting as u8)),
            consensus: Arc::new(AtomicU8::new(SubsystemStatus::Starting as u8)),
            execution: Arc::new(AtomicU8::new(SubsystemStatus::Starting as u8)),
            genesis: Arc::new(AtomicU8::new(SubsystemStatus::Starting as u8)),
            storage_progress: Arc::new(AtomicU64::new(0)),
            network_progress: Arc::new(AtomicU64::new(0)),
            consensus_progress: Arc::new(AtomicU64::new(0)),
            execution_progress: Arc::new(AtomicU64::new(0)),
            genesis_progress: Arc::new(AtomicU64::new(0)),
            clock_origin: Arc::new(Instant::now()),
            stall_threshold_ms: DEFAULT_STALL_THRESHOLD_MS,
        }
    }

    /// Create a tracker with a custom stall-detection threshold.
    pub fn with_stall_threshold(stall_threshold_ms: u64) -> Self {
        let mut nr = Self::new();
        nr.stall_threshold_ms = stall_threshold_ms;
        nr
    }

    /// Obtain the write-side handle for storage.
    pub fn storage_handle(&self) -> SubsystemHandle {
        SubsystemHandle {
            inner: Arc::clone(&self.storage),
            last_progress_ms: Arc::clone(&self.storage_progress),
            epoch_origin: Arc::clone(&self.clock_origin),
        }
    }

    /// Obtain the write-side handle for the network layer.
    pub fn network_handle(&self) -> SubsystemHandle {
        SubsystemHandle {
            inner: Arc::clone(&self.network),
            last_progress_ms: Arc::clone(&self.network_progress),
            epoch_origin: Arc::clone(&self.clock_origin),
        }
    }

    /// Obtain the write-side handle for the consensus engine.
    pub fn consensus_handle(&self) -> SubsystemHandle {
        SubsystemHandle {
            inner: Arc::clone(&self.consensus),
            last_progress_ms: Arc::clone(&self.consensus_progress),
            epoch_origin: Arc::clone(&self.clock_origin),
        }
    }

    /// Obtain the write-side handle for the execution service.
    pub fn execution_handle(&self) -> SubsystemHandle {
        SubsystemHandle {
            inner: Arc::clone(&self.execution),
            last_progress_ms: Arc::clone(&self.execution_progress),
            epoch_origin: Arc::clone(&self.clock_origin),
        }
    }

    /// Obtain the write-side handle for genesis loading.
    pub fn genesis_handle(&self) -> SubsystemHandle {
        SubsystemHandle {
            inner: Arc::clone(&self.genesis),
            last_progress_ms: Arc::clone(&self.genesis_progress),
            epoch_origin: Arc::clone(&self.clock_origin),
        }
    }

    // ── Read-side API ───────────────────────────────────────────────

    fn read(&self, atom: &AtomicU8) -> SubsystemStatus {
        SubsystemStatus::from_u8(atom.load(Ordering::Acquire))
    }

    fn progress_ms(&self, atom: &AtomicU64) -> u64 {
        let last = atom.load(Ordering::Acquire);
        if last == 0 {
            return 0; // never reported
        }
        let now = self.clock_origin.elapsed().as_millis() as u64;
        now.saturating_sub(last)
    }

    /// Whether a critical subsystem is stalled — reports Ready but has
    /// not made progress within the configured threshold.
    fn is_stalled(&self, status_atom: &AtomicU8, progress_atom: &AtomicU64) -> bool {
        let status = self.read(status_atom);
        if status != SubsystemStatus::Ready {
            return false; // only Ready subsystems can be stalled
        }
        let last = progress_atom.load(Ordering::Acquire);
        if last == 0 {
            return false; // never reported — still in setup
        }
        let elapsed = self.clock_origin.elapsed().as_millis() as u64;
        elapsed.saturating_sub(last) > self.stall_threshold_ms
    }

    /// Derive the aggregate [`NodeStatus`] from current subsystem states.
    ///
    /// Derivation rules:
    /// 1. Any critical subsystem (`storage`, `consensus`, `execution`,
    ///    `genesis`) **down** → `Halted`.
    /// 2. Any critical subsystem **stalled** (Ready but no progress
    ///    beyond the threshold) → `Halted`.
    /// 3. Any critical subsystem still **starting** → `Bootstrapping`.
    /// 4. Consensus **degraded** (syncing) → `Syncing`.
    /// 5. Network **degraded** or **down** → `Degraded` (non-critical).
    /// 6. Otherwise → `Healthy`.
    pub fn status(&self) -> NodeStatus {
        let storage = self.read(&self.storage);
        let network = self.read(&self.network);
        let consensus = self.read(&self.consensus);
        let execution = self.read(&self.execution);
        let genesis = self.read(&self.genesis);

        // Critical subsystem down → halted.
        if storage == SubsystemStatus::Down
            || consensus == SubsystemStatus::Down
            || execution == SubsystemStatus::Down
            || genesis == SubsystemStatus::Down
        {
            return NodeStatus::Halted;
        }

        // Critical subsystem stalled → halted.
        if self.is_stalled(&self.storage, &self.storage_progress)
            || self.is_stalled(&self.consensus, &self.consensus_progress)
            || self.is_stalled(&self.execution, &self.execution_progress)
        {
            return NodeStatus::Halted;
        }

        // Any critical subsystem still starting → bootstrapping.
        if storage == SubsystemStatus::Starting
            || consensus == SubsystemStatus::Starting
            || execution == SubsystemStatus::Starting
            || genesis == SubsystemStatus::Starting
        {
            return NodeStatus::Bootstrapping;
        }

        // Consensus degraded → syncing (catching up with the network).
        if consensus == SubsystemStatus::Degraded {
            return NodeStatus::Syncing;
        }

        // Non-critical subsystem degraded or down → degraded.
        if network == SubsystemStatus::Degraded
            || network == SubsystemStatus::Down
            || network == SubsystemStatus::Starting
        {
            return NodeStatus::Degraded;
        }

        // Execution or storage degraded → degraded.
        if execution == SubsystemStatus::Degraded || storage == SubsystemStatus::Degraded {
            return NodeStatus::Degraded;
        }

        NodeStatus::Healthy
    }

    /// Return per-subsystem snapshot for inclusion in health responses.
    pub fn subsystem_snapshot(&self) -> Vec<SubsystemSnapshot> {
        vec![
            SubsystemSnapshot {
                name: "storage",
                status: self.read(&self.storage).as_str(),
                last_progress_ms: self.progress_ms(&self.storage_progress),
            },
            SubsystemSnapshot {
                name: "network",
                status: self.read(&self.network).as_str(),
                last_progress_ms: self.progress_ms(&self.network_progress),
            },
            SubsystemSnapshot {
                name: "consensus",
                status: self.read(&self.consensus).as_str(),
                last_progress_ms: self.progress_ms(&self.consensus_progress),
            },
            SubsystemSnapshot {
                name: "execution",
                status: self.read(&self.execution).as_str(),
                last_progress_ms: self.progress_ms(&self.execution_progress),
            },
            SubsystemSnapshot {
                name: "genesis",
                status: self.read(&self.genesis).as_str(),
                last_progress_ms: self.progress_ms(&self.genesis_progress),
            },
        ]
    }
}

impl Default for NodeReadiness {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_status_is_bootstrapping() {
        let nr = NodeReadiness::new();
        assert_eq!(nr.status(), NodeStatus::Bootstrapping);
        assert!(!nr.status().is_ready());
    }

    #[test]
    fn all_ready_yields_healthy() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Healthy);
        assert!(nr.status().is_ready());
    }

    #[test]
    fn network_down_yields_degraded() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_down();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Degraded);
        assert!(nr.status().is_ready());
    }

    #[test]
    fn consensus_degraded_yields_syncing() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_degraded();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Syncing);
        assert!(!nr.status().is_ready());
    }

    #[test]
    fn storage_down_yields_halted() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_down();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Halted);
        assert!(!nr.status().is_ready());
    }

    #[test]
    fn execution_down_yields_halted() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_down();
        nr.genesis_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Halted);
    }

    #[test]
    fn genesis_down_yields_halted() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_down();
        assert_eq!(nr.status(), NodeStatus::Halted);
    }

    #[test]
    fn subsystem_snapshot_reflects_current_state() {
        let nr = NodeReadiness::new();
        nr.storage_handle().set_ready();
        nr.network_handle().set_degraded();
        let snap = nr.subsystem_snapshot();
        assert_eq!(snap.len(), 5);
        assert_eq!(snap[0].name, "storage");
        assert_eq!(snap[0].status, "ready");
        assert_eq!(snap[1].name, "network");
        assert_eq!(snap[1].status, "degraded");
    }

    #[test]
    fn handle_clone_shares_state() {
        let nr = NodeReadiness::new();
        let h1 = nr.storage_handle();
        let h2 = nr.storage_handle();
        h1.set_ready();
        assert_eq!(h2.status(), SubsystemStatus::Ready);
    }

    #[test]
    fn report_progress_updates_timestamp() {
        let nr = NodeReadiness::new();
        let h = nr.storage_handle();
        h.set_ready();
        // After set_ready the progress should have been reported.
        assert!(h.ms_since_last_progress() < 1_000);
    }

    #[test]
    fn stall_detection_with_short_threshold() {
        // Use a very short threshold so the test can trigger a stall
        // without actually waiting 30 seconds.
        let nr = NodeReadiness::with_stall_threshold(0);
        let h = nr.storage_handle();
        h.set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();

        // Immediately after set_ready, the subsystem should be stalled
        // because threshold is 0 and at least 0 ms have passed.
        // We need to let at least 1 ms elapse for the subtraction to catch it.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(nr.status(), NodeStatus::Halted);
    }

    #[test]
    fn stall_does_not_trigger_on_fresh_progress() {
        let nr = NodeReadiness::with_stall_threshold(60_000);
        nr.storage_handle().set_ready();
        nr.network_handle().set_ready();
        nr.consensus_handle().set_ready();
        nr.execution_handle().set_ready();
        nr.genesis_handle().set_ready();
        // With a 60 s threshold, nothing should be stalled.
        assert_eq!(nr.status(), NodeStatus::Healthy);
    }
}
