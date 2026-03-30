// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Snapshot manifest signing and verification.
//!
//! Provides helpers for signing a [`SnapshotManifest`] with a Falcon-512
//! (post-quantum) key and verifying the signature on import.
//!
//! The signing flow:
//!
//! 1. Export produces a manifest with `signature = None`.
//! 2. Caller invokes [`sign_manifest`] with the validator's signing key.
//! 3. The signed manifest can be serialized alongside the snapshot.
//!
//! The verification flow:
//!
//! 1. Import deserializes the manifest.
//! 2. If `signature` is present, caller invokes [`verify_manifest`]
//!    with a trusted set of validator public keys.

use nexus_crypto::falcon::{FalconSigningKey, FalconVerifyKey};
use nexus_crypto::{FalconSigner, Signer};
use nexus_storage::rocks::RocksStore;
use nexus_storage::{SnapshotManifest, SNAPSHOT_SIGN_DOMAIN};
use std::path::Path;

/// Sign a snapshot manifest in-place using a Falcon-512 signing key.
///
/// Populates the `signature`, `signer_public_key`, and `signature_scheme`
/// fields. The manifest's `content_hash` must already be set.
pub fn sign_manifest(
    manifest: &mut SnapshotManifest,
    signing_key: &FalconSigningKey,
    verify_key: &FalconVerifyKey,
) {
    let payload = manifest.signable_bytes();
    let sig = FalconSigner::sign(signing_key, SNAPSHOT_SIGN_DOMAIN, &payload);
    manifest.signature = Some(sig.as_bytes().to_vec());
    manifest.signer_public_key = Some(verify_key.as_bytes().to_vec());
    manifest.signature_scheme = Some("falcon-512".to_string());
}

/// Verify a snapshot manifest's signature against a trusted public key.
///
/// # Errors
///
/// Returns an error if:
/// - The manifest has no signature fields.
/// - The signature scheme is not `falcon-512`.
/// - The public key in the manifest doesn't match the trusted key.
/// - The cryptographic verification fails.
pub fn verify_manifest(
    manifest: &SnapshotManifest,
    trusted_key: &FalconVerifyKey,
) -> Result<(), SnapshotSignError> {
    let sig_bytes = manifest
        .signature
        .as_ref()
        .ok_or(SnapshotSignError::MissingSignature)?;

    let scheme = manifest
        .signature_scheme
        .as_deref()
        .ok_or(SnapshotSignError::MissingScheme)?;

    if scheme != "falcon-512" {
        return Err(SnapshotSignError::UnsupportedScheme(scheme.to_string()));
    }

    let pk_bytes = manifest
        .signer_public_key
        .as_ref()
        .ok_or(SnapshotSignError::MissingPublicKey)?;

    // Verify the embedded public key matches the trusted key.
    if pk_bytes.as_slice() != trusted_key.as_bytes() {
        return Err(SnapshotSignError::UntrustedSigner);
    }

    let sig = nexus_crypto::falcon::FalconSignature::from_bytes(sig_bytes)
        .map_err(|e| SnapshotSignError::InvalidSignature(e.to_string()))?;

    let payload = manifest.signable_bytes();
    FalconSigner::verify(trusted_key, SNAPSHOT_SIGN_DOMAIN, &payload, &sig)
        .map_err(|e| SnapshotSignError::VerificationFailed(e.to_string()))?;

    Ok(())
}

/// Verify a snapshot file's manifest signature offline.
///
/// Reads the manifest from `snapshot_path` (either a directory containing
/// `state_snapshot.bin` or the file itself), then verifies the cryptographic
/// signature against `trusted_key`.
///
/// This does NOT import the snapshot — it only reads the header.
pub fn verify_snapshot_file(
    snapshot_path: &Path,
    trusted_key: &FalconVerifyKey,
) -> Result<SnapshotManifest, SnapshotSignError> {
    let manifest = RocksStore::read_snapshot_manifest(snapshot_path)
        .map_err(|e| SnapshotSignError::IoError(e.to_string()))?;
    verify_manifest(&manifest, trusted_key)?;
    Ok(manifest)
}

/// Errors from snapshot manifest signing/verification.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotSignError {
    #[error("manifest has no signature")]
    MissingSignature,
    #[error("manifest has no signature scheme")]
    MissingScheme,
    #[error("manifest has no signer public key")]
    MissingPublicKey,
    #[error("unsupported signature scheme: {0}")]
    UnsupportedScheme(String),
    #[error("signer public key does not match any trusted key")]
    UntrustedSigner,
    #[error("invalid signature encoding: {0}")]
    InvalidSignature(String),
    #[error("signature verification failed: {0}")]
    VerificationFailed(String),
    #[error("failed to read snapshot: {0}")]
    IoError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 42,
            entry_count: 100,
            total_bytes: 5000,
            content_hash: Some([0xAB; 32]),
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };

        sign_manifest(&mut manifest, &sk, &vk);

        assert!(manifest.signature.is_some());
        assert_eq!(manifest.signature_scheme.as_deref(), Some("falcon-512"));
        assert!(manifest.signer_public_key.is_some());

        verify_manifest(&manifest, &vk).expect("verification should succeed");
    }

    #[test]
    fn verify_fails_with_wrong_key() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let (_sk2, vk2) = FalconSigner::generate_keypair();

        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 10,
            entry_count: 50,
            total_bytes: 2500,
            content_hash: Some([0xCD; 32]),
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };

        sign_manifest(&mut manifest, &sk, &vk);

        let err = verify_manifest(&manifest, &vk2).unwrap_err();
        assert!(matches!(err, SnapshotSignError::UntrustedSigner));
    }

    #[test]
    fn verify_fails_when_unsigned() {
        let (_sk, vk) = FalconSigner::generate_keypair();

        let manifest = SnapshotManifest {
            version: 1,
            block_height: 1,
            entry_count: 0,
            total_bytes: 0,
            content_hash: None,
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };

        let err = verify_manifest(&manifest, &vk).unwrap_err();
        assert!(matches!(err, SnapshotSignError::MissingSignature));
    }

    #[test]
    fn verify_fails_on_tampered_manifest() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 99,
            entry_count: 200,
            total_bytes: 10_000,
            content_hash: Some([0xFF; 32]),
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };

        sign_manifest(&mut manifest, &sk, &vk);

        // Tamper with the block height.
        manifest.block_height = 100;

        let err = verify_manifest(&manifest, &vk).unwrap_err();
        assert!(matches!(err, SnapshotSignError::VerificationFailed(_)));
    }

    #[test]
    fn verify_rejects_unsupported_scheme() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 1,
            entry_count: 0,
            total_bytes: 0,
            content_hash: None,
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };

        sign_manifest(&mut manifest, &sk, &vk);
        manifest.signature_scheme = Some("ed25519".to_string());

        let err = verify_manifest(&manifest, &vk).unwrap_err();
        assert!(matches!(err, SnapshotSignError::UnsupportedScheme(_)));
    }

    #[test]
    fn verify_rejects_missing_public_key() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 1,
            entry_count: 0,
            total_bytes: 0,
            content_hash: None,
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };
        sign_manifest(&mut manifest, &sk, &vk);
        manifest.signer_public_key = None;
        let err = verify_manifest(&manifest, &vk).unwrap_err();
        assert!(matches!(err, SnapshotSignError::MissingPublicKey));
    }

    #[test]
    fn verify_rejects_missing_scheme() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 1,
            entry_count: 0,
            total_bytes: 0,
            content_hash: None,
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };
        sign_manifest(&mut manifest, &sk, &vk);
        manifest.signature_scheme = None;
        let err = verify_manifest(&manifest, &vk).unwrap_err();
        assert!(matches!(err, SnapshotSignError::MissingScheme));
    }

    #[test]
    fn verify_rejects_invalid_signature_bytes() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let mut manifest = SnapshotManifest {
            version: 1,
            block_height: 1,
            entry_count: 0,
            total_bytes: 0,
            content_hash: None,
            signature: None,
            signer_public_key: None,
            signature_scheme: None,
            chain_id: None,
            epoch: None,
            created_at_ms: None,
            previous_manifest_hash: None,
        };
        sign_manifest(&mut manifest, &sk, &vk);
        manifest.signature = Some(vec![0xFF; 10]);
        let err = verify_manifest(&manifest, &vk).unwrap_err();
        // Falcon-512 signature is a specific length; short bytes may be
        // rejected as InvalidSignature or VerificationFailed.
        assert!(
            matches!(
                err,
                SnapshotSignError::InvalidSignature(_)
                    | SnapshotSignError::VerificationFailed(_)
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn verify_snapshot_file_errors_on_nonexistent_path() {
        let (_sk, vk) = FalconSigner::generate_keypair();
        let err = verify_snapshot_file(std::path::Path::new("/tmp/no-such-snapshot-xyz"), &vk)
            .unwrap_err();
        assert!(matches!(err, SnapshotSignError::IoError(_)));
    }

    #[test]
    fn all_error_variants_display() {
        let errors = [
            SnapshotSignError::MissingSignature,
            SnapshotSignError::MissingScheme,
            SnapshotSignError::MissingPublicKey,
            SnapshotSignError::UnsupportedScheme("ed25519".into()),
            SnapshotSignError::UntrustedSigner,
            SnapshotSignError::InvalidSignature("bad".into()),
            SnapshotSignError::VerificationFailed("failed".into()),
            SnapshotSignError::IoError("io".into()),
        ];
        for e in &errors {
            let msg = format!("{e}");
            assert!(!msg.is_empty());
        }
    }
}
