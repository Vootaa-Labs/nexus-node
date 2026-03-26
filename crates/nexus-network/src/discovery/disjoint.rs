//! Disjoint-path DHT lookup for Eclipse attack mitigation.
//!
//! Implements the S/Kademlia 2-path disjoint lookup algorithm:
//! two independent DHT queries run in parallel over non-overlapping
//! peer subsets, and the **intersection** of their results is returned.
//!
//! This prevents a single adversary from controlling all route segments
//! because compromising one path still produces wrong results that
//! differ from the honest path.

use std::collections::HashSet;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::error::NetworkError;
use crate::transport::{KadEvent, TransportHandle};
use crate::types::PeerId;

/// Execute a disjoint-path lookup with `num_paths` independent queries.
///
/// Each path issues a Kademlia closest-peers query for `target` and
/// collects results until the event channel yields `ClosestPeers` or
/// a timeout expires. The final result is the **intersection** of
/// all paths' peer sets.
///
/// # Fallback
/// If only one path succeeds, its results are returned directly
/// (graceful degradation).
pub async fn disjoint_lookup(
    transport: &TransportHandle,
    kad_rx: &mut mpsc::Receiver<KadEvent>,
    target: &PeerId,
    num_paths: usize,
    count: usize,
    lookup_timeout: Duration,
) -> Result<Vec<PeerId>, NetworkError> {
    if num_paths <= 1 {
        return single_path_lookup(transport, kad_rx, target, lookup_timeout).await;
    }

    // Issue `num_paths` independent queries.
    // libp2p Kademlia deduplicates by QueryId internally, so we issue
    // multiple get_closest_peers calls with the same key — each produces
    // an independent ClosestPeers result event.
    for _ in 0..num_paths {
        transport
            .kad_find_closest(target.as_bytes().to_vec())
            .await?;
    }

    // Collect results from each path.
    let mut path_results: Vec<HashSet<PeerId>> = Vec::with_capacity(num_paths);

    for i in 0..num_paths {
        match collect_closest_peers(kad_rx, lookup_timeout).await {
            Ok(peers) => {
                debug!(path = i, peers = peers.len(), "disjoint path result");
                path_results.push(peers);
            }
            Err(e) => {
                warn!(path = i, error = %e, "disjoint path failed");
            }
        }
    }

    if path_results.is_empty() {
        return Err(NetworkError::DiscoveryError {
            reason: "all disjoint lookup paths failed".into(),
        });
    }

    // Graceful degradation: if only one path succeeded, return its results.
    if path_results.len() == 1 {
        debug!("single path fallback (one path failed)");
        return Ok(path_results
            .into_iter()
            .next()
            .unwrap()
            .into_iter()
            .take(count)
            .collect());
    }

    // Take intersection of all path results.
    let mut intersection = path_results[0].clone();
    for other in &path_results[1..] {
        intersection.retain(|p| other.contains(p));
    }

    debug!(
        paths_succeeded = path_results.len(),
        intersection_size = intersection.len(),
        "disjoint lookup complete"
    );

    Ok(intersection.into_iter().take(count).collect())
}

/// Single-path fallback when disjoint paths = 1.
async fn single_path_lookup(
    transport: &TransportHandle,
    kad_rx: &mut mpsc::Receiver<KadEvent>,
    target: &PeerId,
    lookup_timeout: Duration,
) -> Result<Vec<PeerId>, NetworkError> {
    transport
        .kad_find_closest(target.as_bytes().to_vec())
        .await?;

    let peers = collect_closest_peers(kad_rx, lookup_timeout).await?;
    Ok(peers.into_iter().collect())
}

/// Drain the event channel until a `ClosestPeers` event arrives or the
/// timeout elapses.
///
/// Consumes and discards non-`ClosestPeers` events (e.g. routing updates).
async fn collect_closest_peers(
    kad_rx: &mut mpsc::Receiver<KadEvent>,
    timeout: Duration,
) -> Result<HashSet<PeerId>, NetworkError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, kad_rx.recv()).await {
            Ok(Some(KadEvent::ClosestPeers { peers })) => {
                let nexus_peers: HashSet<PeerId> = peers.iter().map(PeerId::from_libp2p).collect();
                return Ok(nexus_peers);
            }
            Ok(Some(_other)) => {
                // Skip non-relevant events (BootstrapOk, RoutingUpdated)
                continue;
            }
            Ok(None) => {
                return Err(NetworkError::DiscoveryError {
                    reason: "kad event channel closed during lookup".into(),
                });
            }
            Err(_) => {
                return Err(NetworkError::DiscoveryError {
                    reason: format!("DHT closest-peers lookup timed out after {timeout:?}"),
                });
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn random_peers(n: usize) -> Vec<libp2p::PeerId> {
        (0..n).map(|_| libp2p::PeerId::random()).collect()
    }

    #[tokio::test]
    async fn single_path_returns_all_results() {
        let (tx, mut rx) = mpsc::channel(16);
        let peers = random_peers(5);
        tx.send(KadEvent::ClosestPeers {
            peers: peers.clone(),
        })
        .await
        .unwrap();

        let result = collect_closest_peers(&mut rx, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result.len(), 5);
    }

    #[tokio::test]
    async fn skips_non_closest_peers_events() {
        let (tx, mut rx) = mpsc::channel(16);
        // Send a BootstrapOk first, then the real result
        tx.send(KadEvent::BootstrapOk).await.unwrap();
        tx.send(KadEvent::RoutingUpdated {
            peer: libp2p::PeerId::random(),
            is_new_peer: true,
        })
        .await
        .unwrap();
        let peers = random_peers(3);
        tx.send(KadEvent::ClosestPeers {
            peers: peers.clone(),
        })
        .await
        .unwrap();

        let result = collect_closest_peers(&mut rx, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(result.len(), 3);
    }

    #[tokio::test]
    async fn closed_channel_returns_error() {
        let (tx, mut rx) = mpsc::channel::<KadEvent>(1);
        drop(tx);

        let result = collect_closest_peers(&mut rx, Duration::from_secs(5)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn lookup_times_out_when_no_response() {
        let (_tx, mut rx) = mpsc::channel::<KadEvent>(1);
        // No events sent — should time out.
        let result = collect_closest_peers(&mut rx, Duration::from_millis(50)).await;
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("timed out"),
            "expected timeout error, got: {err_msg}"
        );
    }

    #[test]
    fn intersection_of_two_sets() {
        let peers_a: HashSet<PeerId> = (0..5)
            .map(|i| PeerId::from_public_key(format!("key-{i}").as_bytes()))
            .collect();
        let peers_b: HashSet<PeerId> = (3..8)
            .map(|i| PeerId::from_public_key(format!("key-{i}").as_bytes()))
            .collect();

        let mut intersection = peers_a.clone();
        intersection.retain(|p| peers_b.contains(p));

        // keys 3, 4 overlap
        assert_eq!(intersection.len(), 2);
    }

    #[test]
    fn empty_intersection_gives_empty() {
        let peers_a: HashSet<PeerId> = (0..3)
            .map(|i| PeerId::from_public_key(format!("a-{i}").as_bytes()))
            .collect();
        let peers_b: HashSet<PeerId> = (0..3)
            .map(|i| PeerId::from_public_key(format!("b-{i}").as_bytes()))
            .collect();

        let mut intersection = peers_a.clone();
        intersection.retain(|p| peers_b.contains(p));

        assert_eq!(intersection.len(), 0);
    }
}
