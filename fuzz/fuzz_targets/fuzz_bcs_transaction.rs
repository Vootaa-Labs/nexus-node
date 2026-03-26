//! Fuzz target: BCS deserialization of transaction types.
//!
//! Ensures that arbitrary byte sequences do not cause panics or
//! undefined behaviour when fed to the BCS decoder for execution
//! types.

#![no_main]

use libfuzzer_sys::fuzz_target;

use nexus_execution::types::{
    BlockExecutionResult, SignedTransaction, TransactionBody, TransactionPayload,
    TransactionReceipt,
};

fuzz_target!(|data: &[u8]| {
    // Each deserialization attempt must either succeed or return an
    // error — never panic or corrupt memory.
    let _ = bcs::from_bytes::<TransactionBody>(data);
    let _ = bcs::from_bytes::<TransactionPayload>(data);
    let _ = bcs::from_bytes::<SignedTransaction>(data);
    let _ = bcs::from_bytes::<TransactionReceipt>(data);
    let _ = bcs::from_bytes::<BlockExecutionResult>(data);
});
