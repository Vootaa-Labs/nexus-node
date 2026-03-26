//! Readiness chaos test suite (v0.1.5 — A-1).
//!
//! Exercises the readiness state machine under fault-injection
//! scenarios that go beyond the existing mock-level tests in
//! `readiness_tests.rs`.
//!
//! Scenarios covered:
//! - **Storage down** while other subsystems are healthy
//! - **Network degraded** mid-operation
//! - **Execution stalled** (heartbeat timeout)
//! - **Consensus halted** then recovered
//! - **Multi-subsystem cascading failure**
//! - **Rapid status flapping**
//! - **Concurrent writer contention**
//! - **Recovery from halted state**
//! - **Stall detection on all critical subsystems**

use std::thread;
use std::time::Duration;

use nexus_node::readiness::{NodeReadiness, NodeStatus, SubsystemStatus};

// ── Helpers ─────────────────────────────────────────────────────────────

/// Set all subsystems to Ready.
fn make_all_ready(nr: &NodeReadiness) {
    nr.storage_handle().set_ready();
    nr.network_handle().set_ready();
    nr.consensus_handle().set_ready();
    nr.execution_handle().set_ready();
    nr.genesis_handle().set_ready();
}

// ── Scenario 1: Storage goes down while node is healthy ─────────────────

#[test]
fn storage_down_while_healthy_yields_halted() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    // Simulate storage crash.
    nr.storage_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Halted);
    assert!(!nr.status().is_ready());

    // Other subsystems unaffected.
    assert_eq!(nr.network_handle().status(), SubsystemStatus::Ready);
    assert_eq!(nr.consensus_handle().status(), SubsystemStatus::Ready);
}

// ── Scenario 2: Network degrades mid-operation ──────────────────────────

#[test]
fn network_degrades_keeps_node_serving() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    nr.network_handle().set_degraded();
    assert_eq!(nr.status(), NodeStatus::Degraded);
    // Degraded is still ready — node can serve traffic.
    assert!(nr.status().is_ready());

    // Health snapshot should report the specific subsystem.
    let snap = nr.subsystem_snapshot();
    let network = snap.iter().find(|s| s.name == "network").unwrap();
    assert_eq!(network.status, "degraded");
}

#[test]
fn network_down_keeps_node_serving() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    nr.network_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Degraded);
    assert!(nr.status().is_ready());
}

// ── Scenario 3: Execution stall detection ───────────────────────────────

#[test]
fn execution_stall_detected_as_halted() {
    // Use 1ms threshold so the test finishes quickly.
    let nr = NodeReadiness::with_stall_threshold(1);
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    // Do NOT report any further progress on execution.
    // Let enough time elapse for the stall to trigger.
    thread::sleep(Duration::from_millis(5));
    assert_eq!(nr.status(), NodeStatus::Halted);
}

#[test]
fn execution_stall_avoided_by_progress_reports() {
    let nr = NodeReadiness::with_stall_threshold(50);
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    // Keep reporting progress to avoid stall.
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(10));
        nr.execution_handle().report_progress();
        nr.storage_handle().report_progress();
        nr.consensus_handle().report_progress();
        assert_eq!(nr.status(), NodeStatus::Healthy);
    }
}

// ── Scenario 4: Consensus stall detection ───────────────────────────────

#[test]
fn consensus_stall_detected_as_halted() {
    let nr = NodeReadiness::with_stall_threshold(1);
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    thread::sleep(Duration::from_millis(5));
    // Consensus is stalled — no progress reported.
    assert_eq!(nr.status(), NodeStatus::Halted);
}

// ── Scenario 5: Storage stall detection ─────────────────────────────────

#[test]
fn storage_stall_detected_as_halted() {
    let nr = NodeReadiness::with_stall_threshold(1);
    make_all_ready(&nr);

    // Report progress on everything except storage.
    thread::sleep(Duration::from_millis(5));
    nr.consensus_handle().report_progress();
    nr.execution_handle().report_progress();

    assert_eq!(nr.status(), NodeStatus::Halted);
}

// ── Scenario 6: Consensus halted then recovered ─────────────────────────

#[test]
fn consensus_halted_then_recovered() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);
    assert_eq!(nr.status(), NodeStatus::Healthy);

    // Consensus goes down.
    nr.consensus_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Halted);
    assert!(!nr.status().is_ready());

    // Consensus recovers.
    nr.consensus_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Healthy);
    assert!(nr.status().is_ready());
}

#[test]
fn consensus_syncing_then_catches_up() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    nr.consensus_handle().set_degraded();
    assert_eq!(nr.status(), NodeStatus::Syncing);
    assert!(!nr.status().is_ready());

    nr.consensus_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Healthy);
    assert!(nr.status().is_ready());
}

// ── Scenario 7: Multi-subsystem cascading failure ───────────────────────

#[test]
fn cascading_failure_storage_then_execution() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    // Storage degrades first.
    nr.storage_handle().set_degraded();
    assert_eq!(nr.status(), NodeStatus::Degraded);
    assert!(nr.status().is_ready());

    // Then execution goes down — should become Halted.
    nr.execution_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Halted);
    assert!(!nr.status().is_ready());
}

#[test]
fn all_critical_subsystems_down() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    nr.storage_handle().set_down();
    nr.consensus_handle().set_down();
    nr.execution_handle().set_down();
    nr.genesis_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Halted);
}

#[test]
fn network_and_execution_both_degraded() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    nr.network_handle().set_degraded();
    nr.execution_handle().set_degraded();
    // Both degraded — still Degraded (not Halted).
    assert_eq!(nr.status(), NodeStatus::Degraded);
    assert!(nr.status().is_ready());
}

// ── Scenario 8: Rapid status flapping ───────────────────────────────────

#[test]
fn rapid_flapping_stabilizes_correctly() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    // Rapidly flip storage between Ready and Down.
    for _ in 0..100 {
        nr.storage_handle().set_down();
        assert_eq!(nr.status(), NodeStatus::Halted);

        nr.storage_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Healthy);
    }
}

#[test]
fn rapid_consensus_flapping() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    for _ in 0..100 {
        nr.consensus_handle().set_degraded();
        assert_eq!(nr.status(), NodeStatus::Syncing);

        nr.consensus_handle().set_ready();
        assert_eq!(nr.status(), NodeStatus::Healthy);
    }
}

// ── Scenario 9: Concurrent writer contention ────────────────────────────

#[test]
fn concurrent_status_updates_from_multiple_threads() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    let nr1 = nr.clone();
    let nr2 = nr.clone();
    let nr3 = nr.clone();

    let t1 = thread::spawn(move || {
        for _ in 0..500 {
            nr1.storage_handle().set_down();
            nr1.storage_handle().set_ready();
        }
    });

    let t2 = thread::spawn(move || {
        for _ in 0..500 {
            nr2.consensus_handle().set_degraded();
            nr2.consensus_handle().set_ready();
        }
    });

    let t3 = thread::spawn(move || {
        for _ in 0..500 {
            // Reader — just reads status.
            let _ = nr3.status();
            let _ = nr3.subsystem_snapshot();
        }
    });

    t1.join().unwrap();
    t2.join().unwrap();
    t3.join().unwrap();

    // After all threads finish with their final set_ready calls,
    // the node should be Healthy. The lock-free atomics guarantee
    // no data races.
    assert_eq!(nr.status(), NodeStatus::Healthy);
}

// ── Scenario 10: Recovery from full halted state ────────────────────────

#[test]
fn full_recovery_from_halted() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    // Drive into halted via multiple failures.
    nr.storage_handle().set_down();
    nr.execution_handle().set_down();
    assert_eq!(nr.status(), NodeStatus::Halted);

    // Recover storage first — still halted (execution down).
    nr.storage_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Halted);

    // Recover execution — now healthy.
    nr.execution_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Healthy);
}

// ── Scenario 11: Bootstrapping transitions ──────────────────────────────

#[test]
fn partial_bootstrap_stays_bootstrapping() {
    let nr = NodeReadiness::new();
    // Only storage and genesis ready.
    nr.storage_handle().set_ready();
    nr.genesis_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Bootstrapping);

    // Execution comes online.
    nr.execution_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Bootstrapping); // consensus still starting

    // Consensus comes online.
    nr.consensus_handle().set_ready();
    // Network still starting — should be Degraded.
    assert_eq!(nr.status(), NodeStatus::Degraded);

    // Network ready → Healthy.
    nr.network_handle().set_ready();
    assert_eq!(nr.status(), NodeStatus::Healthy);
}

// ── Scenario 12: Subsystem snapshot includes progress metadata ──────────

#[test]
fn snapshot_shows_progress_for_active_subsystems() {
    let nr = NodeReadiness::new();
    make_all_ready(&nr);

    thread::sleep(Duration::from_millis(5));

    let snap = nr.subsystem_snapshot();
    for s in &snap {
        // All subsystems have been set_ready so their last_progress_ms
        // should be non-zero (indicating activity).
        assert!(s.last_progress_ms > 0, "{} has no progress", s.name);
    }
}

#[test]
fn snapshot_shows_zero_progress_for_starting_subsystem() {
    let nr = NodeReadiness::new();
    // Don't set any subsystem ready.
    let snap = nr.subsystem_snapshot();
    for s in &snap {
        assert_eq!(
            s.last_progress_ms, 0,
            "{} should have zero progress",
            s.name
        );
    }
}

// ── Scenario 13: Stall with progress on some but not all ────────────────

#[test]
fn mixed_stall_one_critical_stalled_others_active() {
    let nr = NodeReadiness::with_stall_threshold(1);
    make_all_ready(&nr);

    thread::sleep(Duration::from_millis(5));

    // Report progress on consensus and execution but NOT storage.
    nr.consensus_handle().report_progress();
    nr.execution_handle().report_progress();

    // Storage is stalled → Halted.
    assert_eq!(nr.status(), NodeStatus::Halted);
}

// ── Scenario 14: Genesis never stall-checked (one-time subsystem) ───────

#[test]
fn genesis_does_not_trigger_stall() {
    // Genesis is a one-time setup step; once it goes Ready, it should
    // not need ongoing progress. The stall check only monitors
    // storage, consensus, and execution.
    let nr = NodeReadiness::with_stall_threshold(1);
    make_all_ready(&nr);

    thread::sleep(Duration::from_millis(5));

    // Report progress on the three critical subsystems that are
    // stall-monitored.
    nr.storage_handle().report_progress();
    nr.consensus_handle().report_progress();
    nr.execution_handle().report_progress();

    // Genesis hasn't reported progress since set_ready, but this
    // should NOT cause Halted because genesis is not stall-checked.
    assert_eq!(nr.status(), NodeStatus::Healthy);
}
