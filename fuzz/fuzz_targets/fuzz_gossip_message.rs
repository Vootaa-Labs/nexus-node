//! Fuzz target: BCS deserialization of gossip network types.
//!
//! Ensures that arbitrary byte sequences do not cause panics when
//! decoded as gossip protocol types (peer IDs, topics, message types).

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_network::types::{MessageType, PeerId, Topic};

fuzz_target!(|data: &[u8]| {
    let _ = bcs::from_bytes::<PeerId>(data);
    let _ = bcs::from_bytes::<Topic>(data);
    let _ = bcs::from_bytes::<MessageType>(data);
});
