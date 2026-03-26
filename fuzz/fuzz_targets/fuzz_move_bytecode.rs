//! Fuzz target: Move bytecode / payload deserialization.
//!
//! Feeds arbitrary bytes into the BCS decoder for Move-related
//! transaction payloads (MovePublish, MoveScript, MoveCall).
//! Ensures malformed bytecode never causes panics in the decoder.

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_execution::types::TransactionPayload;

fuzz_target!(|data: &[u8]| {
    // Full payload decoding — covers all variants including Move ones.
    let _ = bcs::from_bytes::<TransactionPayload>(data);

    // If data is long enough, try interpreting the tail as a raw
    // bytecode_modules vector, which is the inner field of MovePublish.
    if data.len() >= 4 {
        let _ = bcs::from_bytes::<Vec<Vec<u8>>>(data);
    }
});
