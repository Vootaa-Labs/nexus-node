//! Fuzz target: BCS deserialization of state-sync messages.
//!
//! Ensures that arbitrary byte sequences do not cause panics when
//! decoded as state synchronization protocol types.

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_node::state_sync::StateSyncMessage;

fuzz_target!(|data: &[u8]| {
    let _ = bcs::from_bytes::<StateSyncMessage>(data);
});
