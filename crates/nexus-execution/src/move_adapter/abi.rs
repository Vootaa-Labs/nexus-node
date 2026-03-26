// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Move module function ABI — describes callable functions in published modules.
//!
//! When a module is published, its bytecode is stored along with a serialised
//! ABI that maps function names to parameter/return types. The ABI enables
//! the [`NexusMoveVm`](super::nexus_vm::NexusMoveVm) to validate call
//! arguments and dispatch execution without a full bytecode interpreter.
//!
//! # ABI Wire Format (v1)
//!
//! The ABI is BCS-encoded as `Vec<FunctionAbi>` and stored under the
//! `"abi"` key at the contract address.

use serde::{Deserialize, Serialize};

// ── Storage key ─────────────────────────────────────────────────────────

/// Storage key for the serialised ABI under the contract address.
pub(crate) const MODULE_ABI_KEY: &[u8] = b"abi";

// ── ABI types ───────────────────────────────────────────────────────────

/// Describes a single callable function in a published module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FunctionAbi {
    /// Human-readable function name (e.g. `"increment"`).
    pub name: String,
    /// Parameter types, in order.
    pub params: Vec<MoveType>,
    /// Return type (None = void / unit).
    pub returns: Option<MoveType>,
    /// Whether this function mutates state.
    pub is_entry: bool,
}

/// Primitive Move types supported at the ABI boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum MoveType {
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 128-bit integer.
    U128,
    /// Boolean.
    Bool,
    /// 32-byte address.
    Address,
    /// Variable-length byte vector.
    VectorU8,
}

/// Parse an ABI from BCS-encoded bytes.
pub(crate) fn decode_abi(bytes: &[u8]) -> Result<Vec<FunctionAbi>, bcs::Error> {
    bcs::from_bytes(bytes)
}

/// Encode an ABI to BCS bytes.
pub(crate) fn encode_abi(functions: &[FunctionAbi]) -> Result<Vec<u8>, bcs::Error> {
    bcs::to_bytes(functions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_round_trip() {
        let abi = vec![
            FunctionAbi {
                name: "increment".into(),
                params: vec![],
                returns: None,
                is_entry: true,
            },
            FunctionAbi {
                name: "get_count".into(),
                params: vec![],
                returns: Some(MoveType::U64),
                is_entry: false,
            },
            FunctionAbi {
                name: "transfer".into(),
                params: vec![MoveType::Address, MoveType::U64],
                returns: None,
                is_entry: true,
            },
        ];

        let encoded = encode_abi(&abi).expect("encode");
        let decoded: Vec<FunctionAbi> = decode_abi(&encoded).expect("decode");
        assert_eq!(abi, decoded);
    }

    #[test]
    fn abi_empty_functions() {
        let abi: Vec<FunctionAbi> = vec![];
        let encoded = encode_abi(&abi).expect("encode");
        let decoded: Vec<FunctionAbi> = decode_abi(&encoded).expect("decode");
        assert!(decoded.is_empty());
    }
}
