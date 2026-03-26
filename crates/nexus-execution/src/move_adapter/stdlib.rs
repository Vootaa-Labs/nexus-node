// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Embedded Move standard library modules and native function registration.
//!
//! The Move VM requires framework modules (address `0x1`) to be available
//! at execution time.  Since Nexus does not yet deploy them via genesis,
//! we embed the compiled bytecode directly and serve it from
//! [`get_framework_module`].
//!
//! Native functions referenced in the stdlib bytecode (e.g.
//! `signer::borrow_address`) are registered via [`native_functions`].

use nexus_move_runtime::upstream::move_core_types::account_address::AccountAddress as MoveAddress;
use nexus_move_runtime::upstream::move_core_types::gas_algebra::InternalGas;
use nexus_move_runtime::upstream::move_core_types::identifier::Identifier;
use nexus_move_runtime::upstream::move_vm_runtime::native_functions::NativeFunction;
use nexus_move_runtime::upstream::move_vm_types::natives::function::NativeResult;
use nexus_move_runtime::upstream::move_vm_types::values::Value;
use std::collections::VecDeque;
use std::sync::Arc;

// ── Embedded bytecode ───────────────────────────────────────────────────

/// Compiled `0x1::signer` module (MoveStdlib).
const SIGNER_MV: &[u8] = include_bytes!("stdlib/signer.mv");

/// The framework address `0x0000…0001`.
pub(crate) fn framework_address() -> MoveAddress {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    MoveAddress::new(bytes)
}

/// Look up an embedded framework module by name.
///
/// Returns the compiled `.mv` bytecode if `module_name` matches a bundled
/// module, or `None` otherwise.
pub(crate) fn get_framework_module(module_name: &str) -> Option<&'static [u8]> {
    match module_name {
        "signer" => Some(SIGNER_MV),
        _ => None,
    }
}

// ── Native function table ───────────────────────────────────────────────

/// Build the native function table required by the Move runtime.
///
/// Currently registers:
/// - `0x1::signer::borrow_address`
pub(crate) fn native_functions() -> Vec<(MoveAddress, Identifier, Identifier, NativeFunction)> {
    let addr = framework_address();
    vec![(
        addr,
        Identifier::new("signer").expect("valid identifier"),
        Identifier::new("borrow_address").expect("valid identifier"),
        make_native_borrow_address(),
    )]
}

/// Native implementation of `0x1::signer::borrow_address`.
///
/// Pops a `SignerRef` from the argument stack and returns the inner address.
fn make_native_borrow_address() -> NativeFunction {
    Arc::new(
        |_context,
         _ty_args: Vec<
            nexus_move_runtime::upstream::move_vm_types::loaded_data::runtime_types::Type,
        >,
         mut arguments: VecDeque<Value>| {
            use nexus_move_runtime::upstream::move_vm_types::pop_arg;
            use nexus_move_runtime::upstream::move_vm_types::values::values_impl::SignerRef;

            let signer_ref = pop_arg!(arguments, SignerRef);
            NativeResult::map_partial_vm_result_one(InternalGas::zero(), signer_ref.borrow_signer())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_address_is_0x1() {
        let addr = framework_address();
        let mut expected = [0u8; 32];
        expected[31] = 1;
        assert_eq!(addr.into_bytes(), expected);
    }

    #[test]
    fn signer_module_is_available() {
        assert!(get_framework_module("signer").is_some());
        assert!(get_framework_module("nonexistent").is_none());
    }

    #[test]
    fn native_functions_table_has_signer() {
        let natives = native_functions();
        assert_eq!(natives.len(), 1);
        assert_eq!(natives[0].1.as_str(), "signer");
        assert_eq!(natives[0].2.as_str(), "borrow_address");
    }
}
