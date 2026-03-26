// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Route optimizer — Dijkstra-based cross-shard cost minimisation.
//!
//! When an intent touches multiple shards, the optimizer computes
//! the minimum-cost routing through the shard topology.
//!
//! # Model
//!
//! The shard topology is a complete graph where each edge has a
//! latency cost.  Cross-shard hops incur a base cost plus a
//! per-hop gas penalty.  The optimizer finds the shortest path
//! from `source_shard` to `target_shard` using Dijkstra's algorithm.
//!
//! For small shard counts (≤ 256) this is always fast.

use nexus_primitives::ShardId;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Configuration for the route optimizer.
#[derive(Debug, Clone)]
pub struct RouteOptimizerConfig {
    /// Total number of shards in the network.
    pub shard_count: u16,
    /// Base cost for any cross-shard hop.
    pub base_hop_cost: u64,
    /// Per-hop gas penalty.
    pub gas_penalty_per_hop: u64,
}

impl Default for RouteOptimizerConfig {
    fn default() -> Self {
        Self {
            shard_count: 16,
            base_hop_cost: 100,
            gas_penalty_per_hop: 5_000,
        }
    }
}

/// Cross-shard route optimizer.
///
/// Uses Dijkstra's algorithm over a weighted shard graph.
/// Edge weights can be customised per shard pair; by default all
/// edges have uniform `base_hop_cost`.
pub struct RouteOptimizer {
    config: RouteOptimizerConfig,
    /// `adjacency[src][dst]` = edge weight (0 = self, None = no override).
    /// If not set, `base_hop_cost` is used.
    overrides: Vec<Vec<Option<u64>>>,
}

/// Result of a route computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Ordered shard path from source to destination (inclusive).
    pub path: Vec<ShardId>,
    /// Total cost of the route.
    pub total_cost: u64,
    /// Number of hops (path.len() - 1).
    pub hop_count: usize,
    /// Total gas penalty for cross-shard coordination.
    pub gas_penalty: u64,
}

impl RouteOptimizer {
    /// Create a new optimizer with the given configuration.
    pub fn new(config: RouteOptimizerConfig) -> Self {
        let n = config.shard_count as usize;
        Self {
            overrides: vec![vec![None; n]; n],
            config,
        }
    }

    /// Override the cost of a specific shard-to-shard edge.
    pub fn set_edge_cost(&mut self, from: ShardId, to: ShardId, cost: u64) {
        let f = from.0 as usize;
        let t = to.0 as usize;
        if f < self.config.shard_count as usize && t < self.config.shard_count as usize {
            self.overrides[f][t] = Some(cost);
        }
    }

    /// Compute the cheapest route between two shards.
    ///
    /// Returns `None` if `from == to` (no routing needed) or if either
    /// shard is out of range.
    pub fn find_route(&self, from: ShardId, to: ShardId) -> Option<Route> {
        let n = self.config.shard_count as usize;
        let f = from.0 as usize;
        let t = to.0 as usize;

        if f >= n || t >= n {
            return None;
        }
        if f == t {
            return Some(Route {
                path: vec![from],
                total_cost: 0,
                hop_count: 0,
                gas_penalty: 0,
            });
        }

        // Dijkstra.
        let mut dist = vec![u64::MAX; n];
        let mut prev: Vec<Option<usize>> = vec![None; n];
        dist[f] = 0;

        let mut heap = BinaryHeap::new();
        heap.push(State { cost: 0, node: f });

        while let Some(State { cost, node }) = heap.pop() {
            if cost > dist[node] {
                continue;
            }
            if node == t {
                break;
            }

            for next in 0..n {
                if next == node {
                    continue;
                }
                let edge_cost = self.overrides[node][next].unwrap_or(self.config.base_hop_cost);
                let next_cost = cost.saturating_add(edge_cost);
                if next_cost < dist[next] {
                    dist[next] = next_cost;
                    prev[next] = Some(node);
                    heap.push(State {
                        cost: next_cost,
                        node: next,
                    });
                }
            }
        }

        if dist[t] == u64::MAX {
            return None; // Unreachable (shouldn't happen in complete graph).
        }

        // Reconstruct path.
        let mut path = Vec::new();
        let mut current = t;
        while let Some(p) = prev[current] {
            path.push(ShardId(current as u16));
            current = p;
        }
        path.push(from);
        path.reverse();

        let hop_count = path.len() - 1;
        let gas_penalty = (hop_count as u64).saturating_mul(self.config.gas_penalty_per_hop);

        Some(Route {
            path,
            total_cost: dist[t],
            hop_count,
            gas_penalty,
        })
    }

    /// Access the current configuration.
    pub fn config(&self) -> &RouteOptimizerConfig {
        &self.config
    }
}

/// Dijkstra priority queue node (min-heap via reversed Ord).
#[derive(Eq, PartialEq)]
struct State {
    cost: u64,
    node: usize,
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed for min-heap.
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| self.node.cmp(&other.node))
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_optimizer() -> RouteOptimizer {
        RouteOptimizer::new(RouteOptimizerConfig::default())
    }

    #[test]
    fn same_shard_zero_cost() {
        let opt = default_optimizer();
        let route = opt.find_route(ShardId(3), ShardId(3)).unwrap();
        assert_eq!(route.total_cost, 0);
        assert_eq!(route.hop_count, 0);
        assert_eq!(route.gas_penalty, 0);
        assert_eq!(route.path, vec![ShardId(3)]);
    }

    #[test]
    fn direct_hop_single_cost() {
        let opt = default_optimizer();
        let route = opt.find_route(ShardId(0), ShardId(5)).unwrap();
        // Direct hop: cost = base_hop_cost (100).
        assert_eq!(route.total_cost, 100);
        assert_eq!(route.hop_count, 1);
        assert_eq!(route.gas_penalty, 5_000);
        assert_eq!(route.path, vec![ShardId(0), ShardId(5)]);
    }

    #[test]
    fn uniform_graph_always_direct() {
        let opt = default_optimizer();
        // In a uniform graph, Dijkstra always picks direct hop.
        for src in 0..16u16 {
            for dst in 0..16u16 {
                let route = opt.find_route(ShardId(src), ShardId(dst)).unwrap();
                if src == dst {
                    assert_eq!(route.hop_count, 0);
                } else {
                    assert_eq!(route.hop_count, 1);
                }
            }
        }
    }

    #[test]
    fn custom_edge_costs_prefer_relay() {
        let mut opt = RouteOptimizer::new(RouteOptimizerConfig {
            shard_count: 4,
            base_hop_cost: 200,
            gas_penalty_per_hop: 1_000,
        });
        // Make direct 0→3 very expensive.
        opt.set_edge_cost(ShardId(0), ShardId(3), 1000);
        // Make 0→1→3 cheaper: 50 + 50 = 100 < 1000.
        opt.set_edge_cost(ShardId(0), ShardId(1), 50);
        opt.set_edge_cost(ShardId(1), ShardId(3), 50);

        let route = opt.find_route(ShardId(0), ShardId(3)).unwrap();
        assert_eq!(route.total_cost, 100);
        assert_eq!(route.hop_count, 2);
        assert_eq!(route.path, vec![ShardId(0), ShardId(1), ShardId(3)]);
        assert_eq!(route.gas_penalty, 2_000);
    }

    #[test]
    fn out_of_range_shard() {
        let opt = default_optimizer();
        assert!(opt.find_route(ShardId(0), ShardId(999)).is_none());
        assert!(opt.find_route(ShardId(999), ShardId(0)).is_none());
    }

    #[test]
    fn single_shard_network() {
        let opt = RouteOptimizer::new(RouteOptimizerConfig {
            shard_count: 1,
            base_hop_cost: 100,
            gas_penalty_per_hop: 5_000,
        });
        let route = opt.find_route(ShardId(0), ShardId(0)).unwrap();
        assert_eq!(route.total_cost, 0);
    }

    #[test]
    fn two_shard_network() {
        let opt = RouteOptimizer::new(RouteOptimizerConfig {
            shard_count: 2,
            base_hop_cost: 50,
            gas_penalty_per_hop: 2_500,
        });
        let route = opt.find_route(ShardId(0), ShardId(1)).unwrap();
        assert_eq!(route.total_cost, 50);
        assert_eq!(route.hop_count, 1);
        assert_eq!(route.gas_penalty, 2_500);
    }

    #[test]
    fn config_accessor() {
        let opt = default_optimizer();
        assert_eq!(opt.config().shard_count, 16);
        assert_eq!(opt.config().base_hop_cost, 100);
    }

    #[test]
    fn set_edge_out_of_range_ignored() {
        let mut opt = default_optimizer();
        // Should not panic.
        opt.set_edge_cost(ShardId(999), ShardId(0), 42);
        opt.set_edge_cost(ShardId(0), ShardId(999), 42);
    }

    #[test]
    fn route_deterministic() {
        let opt = default_optimizer();
        let r1 = opt.find_route(ShardId(2), ShardId(14)).unwrap();
        let r2 = opt.find_route(ShardId(2), ShardId(14)).unwrap();
        assert_eq!(r1, r2);
    }
}
