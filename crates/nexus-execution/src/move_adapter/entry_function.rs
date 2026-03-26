// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Entry function dispatch — validates and executes Move function calls.
//!
//! TLD-09 §6.1: all contract calls pass through a [`MoveCallRequest`]
//! canonical DTO. This module validates the request against the published
//! ABI and dispatches execution to the appropriate handler.

use nexus_primitives::AccountAddress;
use serde::{Deserialize, Serialize};

use super::abi::{FunctionAbi, MoveType};

// ── Canonical call DTO (TLD-09 §6.1) ───────────────────────────────────

/// Canonical contract call request.
///
/// This is the Nexus-public DTO — no Move-internal types leak.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MoveCallRequest {
    /// Transaction sender.
    pub sender: AccountAddress,
    /// Target contract (package) address.
    pub package: AccountAddress,
    /// Module name (currently unused — one module per package).
    pub module: String,
    /// Function name.
    pub function: String,
    /// Serialised type arguments (future generics support).
    pub type_args: Vec<Vec<u8>>,
    /// BCS-encoded call arguments.
    pub bcs_args: Vec<Vec<u8>>,
    /// Gas budget.
    pub gas_budget: u64,
}

// ── Argument validation ─────────────────────────────────────────────────

/// Check that the provided arguments match the function's ABI.
///
/// Returns `Ok(())` if the argument count and types are compatible,
/// or the first mismatch description on error.
pub(crate) fn validate_args(abi: &FunctionAbi, args: &[Vec<u8>]) -> Result<(), String> {
    if args.len() != abi.params.len() {
        return Err(format!(
            "argument count mismatch: {} expected {}, got {}",
            abi.name,
            abi.params.len(),
            args.len()
        ));
    }

    for (i, (param_type, arg_bytes)) in abi.params.iter().zip(args.iter()).enumerate() {
        if let Err(reason) = validate_arg_encoding(param_type, arg_bytes) {
            return Err(format!(
                "arg[{i}] for {}: expected {:?}, {reason}",
                abi.name, param_type
            ));
        }
    }
    Ok(())
}

/// Validate that `bytes` is a plausible BCS encoding for the given
/// primitive type.
fn validate_arg_encoding(ty: &MoveType, bytes: &[u8]) -> Result<(), &'static str> {
    match ty {
        MoveType::U64 => {
            if bytes.len() == 8 {
                Ok(())
            } else {
                Err("expected 8 bytes")
            }
        }
        MoveType::U128 => {
            if bytes.len() == 16 {
                Ok(())
            } else {
                Err("expected 16 bytes")
            }
        }
        MoveType::Bool => {
            if bytes.len() == 1 && bytes[0] <= 1 {
                Ok(())
            } else {
                Err("expected 1 byte (0 or 1)")
            }
        }
        MoveType::Address => {
            if bytes.len() == 32 {
                Ok(())
            } else {
                Err("expected 32 bytes")
            }
        }
        MoveType::VectorU8 => {
            // Defence-in-depth: reject empty vectors and enforce a 1 MB
            // ceiling before the bytes reach the Move VM / BCS layer.
            const MAX_VECTOR_ARG_SIZE: usize = 1_048_576;
            if bytes.is_empty() {
                Err("vector argument cannot be empty")
            } else if bytes.len() > MAX_VECTOR_ARG_SIZE {
                Err("vector argument exceeds 1 MB limit")
            } else {
                Ok(())
            }
        }
    }
}

/// Decode a u64 from BCS (little-endian 8 bytes).
pub(crate) fn decode_u64_arg(bytes: &[u8]) -> Result<u64, &'static str> {
    let arr: [u8; 8] = bytes.try_into().map_err(|_| "expected 8 bytes for u64")?;
    Ok(u64::from_le_bytes(arr))
}

/// Encode a u64 to BCS (little-endian 8 bytes).
pub(crate) fn encode_u64_arg(val: u64) -> Vec<u8> {
    val.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter_increment_abi() -> FunctionAbi {
        FunctionAbi {
            name: "increment".into(),
            params: vec![],
            returns: None,
            is_entry: true,
        }
    }

    fn transfer_abi() -> FunctionAbi {
        FunctionAbi {
            name: "transfer".into(),
            params: vec![MoveType::Address, MoveType::U64],
            returns: None,
            is_entry: true,
        }
    }

    #[test]
    fn args_match_abi() {
        assert!(validate_args(&counter_increment_abi(), &[]).is_ok());
    }

    #[test]
    fn args_count_mismatch() {
        let result = validate_args(&counter_increment_abi(), &[vec![1]]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("argument count mismatch"));
    }

    #[test]
    fn args_type_mismatch() {
        // Transfer expects (Address[32], U64[8]) — pass wrong sizes.
        let result = validate_args(&transfer_abi(), &[vec![0; 4], vec![0; 8]]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expected 32 bytes"));
    }

    #[test]
    fn args_correct_types() {
        let result = validate_args(&transfer_abi(), &[vec![0; 32], vec![0; 8]]);
        assert!(result.is_ok());
    }

    #[test]
    fn u64_round_trip() {
        let val = 42u64;
        let encoded = encode_u64_arg(val);
        let decoded = decode_u64_arg(&encoded).unwrap();
        assert_eq!(val, decoded);
    }

    #[test]
    fn vector_u8_rejects_empty() {
        let result = validate_arg_encoding(&MoveType::VectorU8, &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn vector_u8_accepts_normal() {
        assert!(validate_arg_encoding(&MoveType::VectorU8, &[1, 2, 3]).is_ok());
    }

    #[test]
    fn vector_u8_rejects_oversized() {
        let big = vec![0u8; 1_048_577]; // 1 MB + 1
        let result = validate_arg_encoding(&MoveType::VectorU8, &big);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1 MB"));
    }
}
