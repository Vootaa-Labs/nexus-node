// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Test tracing (logging) setup.
//!
//! Call [`init`] at the start of a test function to enable `tracing`
//! output. Uses `RUST_LOG` or falls back to `warn`.
//!
//! ```no_run
//! #[test]
//! fn my_test() {
//!     nexus_test_utils::tracing_init::init();
//!     tracing::info!("test running");
//! }
//! ```

use std::sync::Once;

static INIT: Once = Once::new();

/// Initialise a `tracing-subscriber` for tests.
///
/// Safe to call multiple times — only the first invocation installs the
/// subscriber. Subsequent calls are no-ops.
///
/// The log level is controlled by the `RUST_LOG` environment variable.
/// If not set, defaults to `warn` to keep test output quiet.
pub fn init() {
    INIT.call_once(|| {
        let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned());
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init()
            .ok(); // Ignore if another subscriber was set.
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init();
        init(); // Must not panic.
    }
}
