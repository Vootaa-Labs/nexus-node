// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Adaptive parallelism controller for Block-STM.
//!
//! Dynamically adjusts the number of rayon worker threads based on
//! observed conflict rates over a sliding window of recent batches.
//!
//! # Algorithm
//!
//! The controller maintains a fixed-size window of recent per-batch
//! conflict rates. The average rate maps to a worker count:
//!
//! | Conflict Rate | Workers                    |
//! |---------------|----------------------------|
//! | < 5%          | `max_workers` (full speed) |
//! | 5% – 20%      | 75% of `max_workers`       |
//! | 20% – 40%     | 50% of `max_workers`       |
//! | > 40%         | 25% of `max_workers`       |

/// Sliding-window conflict-rate tracker that recommends thread counts.
pub(crate) struct AdaptiveParallelism {
    /// Upper bound on worker threads.
    max_workers: usize,
    /// Recent per-batch conflict rates (0.0 – 1.0).
    window: Vec<f64>,
    /// Maximum entries retained in the sliding window.
    window_capacity: usize,
}

/// Default sliding window size: 8 recent batches.
const DEFAULT_WINDOW_CAPACITY: usize = 8;

impl AdaptiveParallelism {
    /// Create a new controller with the given maximum worker count.
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
            window: Vec::with_capacity(DEFAULT_WINDOW_CAPACITY),
            window_capacity: DEFAULT_WINDOW_CAPACITY,
        }
    }

    /// Record the conflict rate from a completed batch execution.
    ///
    /// `rate` should be in the range `[0.0, 1.0]` where 0.0 means no
    /// conflicts and 1.0 means every transaction conflicted.
    pub fn record_conflict_rate(&mut self, rate: f64) {
        if self.window.len() >= self.window_capacity {
            self.window.remove(0);
        }
        self.window.push(rate.clamp(0.0, 1.0));
    }

    /// Recommend the number of workers for the next batch.
    ///
    /// Based on the average conflict rate over the sliding window.
    /// Returns at least 1.
    pub fn recommend_workers(&self) -> usize {
        if self.window.is_empty() {
            return self.max_workers;
        }

        let avg: f64 = self.window.iter().sum::<f64>() / self.window.len() as f64;

        let workers = if avg < 0.05 {
            self.max_workers
        } else if avg < 0.20 {
            self.max_workers * 3 / 4
        } else if avg < 0.40 {
            self.max_workers / 2
        } else {
            self.max_workers / 4
        };

        workers.max(1)
    }

    /// The maximum configured worker count.
    #[cfg(test)]
    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    /// Current average conflict rate, or 0.0 if no data yet.
    #[cfg(test)]
    pub fn average_conflict_rate(&self) -> f64 {
        if self.window.is_empty() {
            0.0
        } else {
            self.window.iter().sum::<f64>() / self.window.len() as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_controller_recommends_max() {
        let ap = AdaptiveParallelism::new(8);
        assert_eq!(ap.recommend_workers(), 8);
        assert_eq!(ap.max_workers(), 8);
    }

    #[test]
    fn min_one_worker() {
        let ap = AdaptiveParallelism::new(0);
        assert_eq!(ap.recommend_workers(), 1);
        assert_eq!(ap.max_workers(), 1);
    }

    #[test]
    fn low_conflict_full_speed() {
        let mut ap = AdaptiveParallelism::new(8);
        ap.record_conflict_rate(0.02);
        ap.record_conflict_rate(0.03);
        ap.record_conflict_rate(0.01);
        assert_eq!(ap.recommend_workers(), 8); // avg ~0.02 < 0.05
    }

    #[test]
    fn medium_conflict_reduces_75() {
        let mut ap = AdaptiveParallelism::new(8);
        for _ in 0..4 {
            ap.record_conflict_rate(0.10);
        }
        assert_eq!(ap.recommend_workers(), 6); // 8 * 3/4 = 6
    }

    #[test]
    fn high_conflict_reduces_50() {
        let mut ap = AdaptiveParallelism::new(8);
        for _ in 0..4 {
            ap.record_conflict_rate(0.30);
        }
        assert_eq!(ap.recommend_workers(), 4); // 8 / 2 = 4
    }

    #[test]
    fn very_high_conflict_reduces_25() {
        let mut ap = AdaptiveParallelism::new(8);
        for _ in 0..4 {
            ap.record_conflict_rate(0.50);
        }
        assert_eq!(ap.recommend_workers(), 2); // 8 / 4 = 2
    }

    #[test]
    fn sliding_window_evicts_oldest() {
        let mut ap = AdaptiveParallelism::new(8);
        // Fill window with low-conflict data.
        for _ in 0..DEFAULT_WINDOW_CAPACITY {
            ap.record_conflict_rate(0.01);
        }
        assert_eq!(ap.recommend_workers(), 8);

        // Push high-conflict data to evict the old entries.
        for _ in 0..DEFAULT_WINDOW_CAPACITY {
            ap.record_conflict_rate(0.50);
        }
        assert_eq!(ap.recommend_workers(), 2);
        assert_eq!(ap.window.len(), DEFAULT_WINDOW_CAPACITY);
    }

    #[test]
    fn average_conflict_rate_empty() {
        let ap = AdaptiveParallelism::new(4);
        assert!((ap.average_conflict_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamps_input_rate() {
        let mut ap = AdaptiveParallelism::new(4);
        ap.record_conflict_rate(2.0); // should clamp to 1.0
        ap.record_conflict_rate(-0.5); // should clamp to 0.0
        assert!((ap.average_conflict_rate() - 0.5).abs() < f64::EPSILON);
    }
}
