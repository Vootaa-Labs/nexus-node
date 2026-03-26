// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! OS-backed CSPRNG for use with `rand_core 0.10` APIs.
//!
//! Provides [`OsRng`] — a zero-sized struct implementing
//! `rand_core::{Rng, CryptoRng}` (via blanket impls from `TryRng` +
//! `TryCryptoRng` with `Error = Infallible`).
//!
//! Uses [`getrandom::fill`] as the entropy source.

use core::convert::Infallible;

// Access rand_core 0.10 via ml-dsa → signature → rand_core re-export chain.
use ml_dsa::signature::rand_core::{TryCryptoRng, TryRng};

/// OS-backed cryptographically secure RNG.
///
/// This is a zero-sized wrapper that delegates to [`getrandom::fill`].
/// It implements `CryptoRng` (auto-derived from `TryCryptoRng<Error = Infallible>`).
pub struct OsRng;

impl TryRng for OsRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut buf = [0u8; 4];
        getrandom::fill(&mut buf).expect("getrandom failed");
        Ok(u32::from_le_bytes(buf))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).expect("getrandom failed");
        Ok(u64::from_le_bytes(buf))
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Infallible> {
        getrandom::fill(dest).expect("getrandom failed");
        Ok(())
    }
}

impl TryCryptoRng for OsRng {}
