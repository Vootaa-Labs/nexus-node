//! Fuzz target: BCS deserialization of consensus types.
//!
//! Ensures that arbitrary byte sequences do not cause panics when
//! decoded as consensus protocol types.

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_consensus::types::{
    CommittedBatch, NarwhalBatch, NarwhalCertificate, ShoalAnchor, ShoalVote, ValidatorBitset,
};

fuzz_target!(|data: &[u8]| {
    let _ = bcs::from_bytes::<NarwhalCertificate>(data);
    let _ = bcs::from_bytes::<NarwhalBatch>(data);
    let _ = bcs::from_bytes::<ShoalVote>(data);
    let _ = bcs::from_bytes::<ShoalAnchor>(data);
    let _ = bcs::from_bytes::<CommittedBatch>(data);
    let _ = bcs::from_bytes::<ValidatorBitset>(data);
});
