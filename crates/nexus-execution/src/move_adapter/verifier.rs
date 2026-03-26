// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bytecode verifier for Move module publishing.
//!
//! Performs structural validation of compiled Move bytecode *before* it is
//! stored on-chain.  This ensures only well-formed modules enter state,
//! enforcing safety invariants from TLD-03 (Execution Layer) and the
//! linear resource model described in Solutions/03-FinalBlueprint §Layer-2.
//!
//! # Verification Checks
//!
//! | # | Check | Abort code |
//! |---|-------|-----------|
//! | V1 | Empty module list rejected | 10 |
//! | V2 | Per-module minimum size (≥ 8 bytes header) | 11 |
//! | V3 | Move magic number (`0xa1, 0x1c, 0xeb, 0x0b`) | 12 |
//! | V4 | Max modules per publish transaction | 13 |
//! | V5 | Per-module size limit | 14 |
//! | V6 | Total size limit (aggregate) | 15 |
//! | V7 | Duplicate module detection (by BLAKE3 hash) | 16 |
//!
//! When a real `move-vm-runtime` integration is added, additional semantic
//! checks (ability verification, type safety, cyclic dependency) will be
//! layered on top of this structural verifier.

use std::collections::HashSet;

use super::vm_config::VmConfig;

// ── Move bytecode magic ─────────────────────────────────────────────────

/// The first 4 bytes of every valid Move compiled module.
const MOVE_MAGIC: [u8; 4] = [0xa1, 0x1c, 0xeb, 0x0b];

/// Minimum valid module size: 4 bytes magic + 4 bytes version header.
const MIN_MODULE_SIZE: usize = 8;

/// Default maximum number of modules in a single publish transaction.
const DEFAULT_MAX_MODULES_PER_PUBLISH: usize = 64;

// ── Verification error ──────────────────────────────────────────────────

/// Describes why bytecode verification failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerificationError {
    /// Human-readable description of the failure.
    pub reason: String,
    /// Numeric abort code for on-chain reporting.
    pub code: u64,
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bytecode verification failed (code {}): {}",
            self.code, self.reason
        )
    }
}

impl std::error::Error for VerificationError {}

// ── Verifier configuration ──────────────────────────────────────────────

/// Configuration for the bytecode verifier.
#[derive(Debug, Clone)]
pub(crate) struct VerifierConfig {
    /// Maximum total bytecode size across all modules (bytes).
    pub max_total_size: usize,
    /// Maximum size for a single module (bytes).
    pub max_module_size: usize,
    /// Maximum number of modules in one publish transaction.
    pub max_modules_per_publish: usize,
}

impl VerifierConfig {
    /// Derive verifier config from the VM config.
    pub fn from_vm_config(config: &VmConfig) -> Self {
        Self {
            max_total_size: config.max_binary_size,
            // Per-module limit = total limit (single module can use entire budget).
            max_module_size: config.max_binary_size,
            max_modules_per_publish: DEFAULT_MAX_MODULES_PER_PUBLISH,
        }
    }
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self::from_vm_config(&VmConfig::default())
    }
}

// ── BytecodeVerifier ────────────────────────────────────────────────────

/// Structural bytecode verifier.
///
/// Validates that submitted modules meet structural requirements before
/// they are stored on-chain.  Thread-safe and reusable across transactions.
pub(crate) struct BytecodeVerifier {
    config: VerifierConfig,
}

impl BytecodeVerifier {
    /// Create a new verifier with the given configuration.
    pub fn new(config: VerifierConfig) -> Self {
        Self { config }
    }

    /// Create a verifier from a [`VmConfig`].
    pub fn from_vm_config(config: &VmConfig) -> Self {
        Self::new(VerifierConfig::from_vm_config(config))
    }

    /// Verify a set of modules for a publish transaction.
    ///
    /// Returns `Ok(())` if all checks pass, or a [`VerificationError`]
    /// describing the first failing check.
    pub fn verify(&self, modules: &[Vec<u8>]) -> Result<(), VerificationError> {
        // V1: No empty publish.
        if modules.is_empty() {
            return Err(VerificationError {
                reason: "empty module list: at least one module required".into(),
                code: 10,
            });
        }

        // V4: Module count limit.
        if modules.len() > self.config.max_modules_per_publish {
            return Err(VerificationError {
                reason: format!(
                    "too many modules: {} (max {})",
                    modules.len(),
                    self.config.max_modules_per_publish
                ),
                code: 13,
            });
        }

        let mut total_size: usize = 0;
        let mut seen_hashes = HashSet::new();

        for (i, module) in modules.iter().enumerate() {
            // V2: Minimum size.
            if module.len() < MIN_MODULE_SIZE {
                return Err(VerificationError {
                    reason: format!(
                        "module[{i}]: too small ({} bytes, minimum {MIN_MODULE_SIZE})",
                        module.len()
                    ),
                    code: 11,
                });
            }

            // V3: Magic number.
            if module[..4] != MOVE_MAGIC {
                return Err(VerificationError {
                    reason: format!(
                        "module[{i}]: invalid magic bytes (expected 0xa11ceb0b, got 0x{:02x}{:02x}{:02x}{:02x})",
                        module[0], module[1], module[2], module[3]
                    ),
                    code: 12,
                });
            }

            // V5: Per-module size limit.
            if module.len() > self.config.max_module_size {
                return Err(VerificationError {
                    reason: format!(
                        "module[{i}]: size {} exceeds limit {}",
                        module.len(),
                        self.config.max_module_size
                    ),
                    code: 14,
                });
            }

            // Accumulate total size for V6.
            total_size = total_size.saturating_add(module.len());

            // V7: Duplicate detection by hash.
            let hash = blake3::hash(module);
            if !seen_hashes.insert(hash) {
                return Err(VerificationError {
                    reason: format!(
                        "module[{i}]: duplicate module (same bytecode already in this publish)"
                    ),
                    code: 16,
                });
            }
        }

        // V6: Total size limit.
        if total_size > self.config.max_total_size {
            return Err(VerificationError {
                reason: format!(
                    "total bytecode size {} exceeds limit {}",
                    total_size, self.config.max_total_size
                ),
                code: 15,
            });
        }

        Ok(())
    }
}

// ── Helper: build valid test module ─────────────────────────────────────

/// Build a minimal valid Move module bytecode for testing.
///
/// Returns 8+ bytes: 4-byte magic + 4-byte version + optional padding.
#[cfg(test)]
pub(crate) fn make_test_module(extra_bytes: usize) -> Vec<u8> {
    let mut module = Vec::with_capacity(MIN_MODULE_SIZE + extra_bytes);
    module.extend_from_slice(&MOVE_MAGIC);
    module.extend_from_slice(&1u32.to_le_bytes()); // version 1
    module.extend(std::iter::repeat(0xAB).take(extra_bytes));
    module
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn verifier() -> BytecodeVerifier {
        BytecodeVerifier::new(VerifierConfig {
            max_total_size: 1024,
            max_module_size: 512,
            max_modules_per_publish: 4,
        })
    }

    #[test]
    fn valid_single_module() {
        let v = verifier();
        let module = make_test_module(16);
        assert!(v.verify(&[module]).is_ok());
    }

    #[test]
    fn valid_multiple_modules() {
        let v = verifier();
        let m1 = make_test_module(10);
        let m2 = make_test_module(20); // different padding → different hash
        assert!(v.verify(&[m1, m2]).is_ok());
    }

    #[test]
    fn v1_empty_modules_rejected() {
        let v = verifier();
        let err = v.verify(&[]).unwrap_err();
        assert_eq!(err.code, 10);
        assert!(err.reason.contains("empty"));
    }

    #[test]
    fn v2_too_small() {
        let v = verifier();
        let err = v.verify(&[vec![0xa1, 0x1c, 0xeb]]).unwrap_err(); // 3 bytes
        assert_eq!(err.code, 11);
        assert!(err.reason.contains("too small"));
    }

    #[test]
    fn v3_bad_magic() {
        let v = verifier();
        let mut m = make_test_module(0);
        m[0] = 0xFF; // corrupt magic
        let err = v.verify(&[m]).unwrap_err();
        assert_eq!(err.code, 12);
        assert!(err.reason.contains("invalid magic"));
    }

    #[test]
    fn v4_too_many_modules() {
        let v = verifier();
        let modules: Vec<Vec<u8>> = (0..5).map(|i| make_test_module(i + 1)).collect();
        let err = v.verify(&modules).unwrap_err();
        assert_eq!(err.code, 13);
        assert!(err.reason.contains("too many"));
    }

    #[test]
    fn v5_single_module_too_large() {
        let v = verifier();
        let m = make_test_module(600); // 608 > 512
        let err = v.verify(&[m]).unwrap_err();
        assert_eq!(err.code, 14);
        assert!(err.reason.contains("exceeds limit"));
    }

    #[test]
    fn v6_total_size_exceeded() {
        let v = BytecodeVerifier::new(VerifierConfig {
            max_total_size: 50,
            max_module_size: 40,
            max_modules_per_publish: 10,
        });
        let m1 = make_test_module(22); // 30 bytes
        let m2 = make_test_module(23); // 31 bytes → total 61 > 50
        let err = v.verify(&[m1, m2]).unwrap_err();
        assert_eq!(err.code, 15);
        assert!(err.reason.contains("total bytecode size"));
    }

    #[test]
    fn v7_duplicate_module() {
        let v = verifier();
        let m = make_test_module(16);
        let err = v.verify(&[m.clone(), m]).unwrap_err();
        assert_eq!(err.code, 16);
        assert!(err.reason.contains("duplicate"));
    }

    #[test]
    fn exactly_min_size_passes() {
        let v = verifier();
        let m = make_test_module(0); // exactly 8 bytes
        assert!(v.verify(&[m]).is_ok());
    }

    #[test]
    fn exactly_at_max_size_passes() {
        let v = BytecodeVerifier::new(VerifierConfig {
            max_total_size: 1024,
            max_module_size: 8 + 100,
            max_modules_per_publish: 4,
        });
        let m = make_test_module(100); // exactly 108 = max_module_size
        assert!(v.verify(&[m]).is_ok());
    }

    #[test]
    fn default_config_from_vm_config() {
        let vc = VerifierConfig::from_vm_config(&VmConfig::default());
        assert_eq!(vc.max_total_size, 524_288);
        assert_eq!(vc.max_modules_per_publish, DEFAULT_MAX_MODULES_PER_PUBLISH);
    }

    #[test]
    fn verification_error_display() {
        let err = VerificationError {
            reason: "test failure".into(),
            code: 42,
        };
        let s = err.to_string();
        assert!(s.contains("code 42"));
        assert!(s.contains("test failure"));
    }
}
