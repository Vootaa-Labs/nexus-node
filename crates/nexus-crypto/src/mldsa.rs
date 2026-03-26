//! ML-DSA-65 digital signature scheme (NIST FIPS 204, Level 3).
//!
//! Used for **user transaction** signatures. Provides post-quantum security
//! at NIST Level 3. Implements the [`Signer`] trait.
//!
//! Pure Rust implementation via RustCrypto `ml-dsa` crate.
//! Migrated from C FFI `pqcrypto-mldsa` per Solutions/20-Pure-Rust-PQ-Crypto-Migration.

use ml_dsa::signature::{SignatureEncoding, Signer as SigSigner, Verifier};
use ml_dsa::MlDsa65;
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::error::NexusCryptoError;
use crate::hasher::Blake3Hasher;
use crate::traits::{CryptoHasher, Signer};

// ── Key types ─────────────────────────────────────────────────────────────────

/// ML-DSA-65 signing (private) key — stored as 32-byte seed.
///
/// The full expanded key is reconstructed via `SigningKey::from_seed()` when
/// signing. Zeroized on drop. **Not cloneable**.
#[derive(ZeroizeOnDrop)]
pub struct DilithiumSigningKey {
    seed: [u8; 32],
}

impl DilithiumSigningKey {
    /// Reconstruct from a 32-byte seed.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the length is not 32.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        let seed: [u8; 32] = bytes.try_into().map_err(|_| NexusCryptoError::InvalidKey {
            reason: format!(
                "invalid ML-DSA-65 seed length: expected 32, got {}",
                bytes.len()
            ),
        })?;
        Ok(Self { seed })
    }

    /// View the seed bytes.
    ///
    /// # Security
    /// Use only for controlled serialisation (e.g. key export to file).
    /// The returned slice is backed by zeroize-on-drop storage.
    pub fn as_bytes(&self) -> &[u8] {
        &self.seed
    }

    /// Reconstruct the expanded signing key from the stored seed.
    fn to_inner(&self) -> ml_dsa::SigningKey<MlDsa65> {
        ml_dsa::SigningKey::<MlDsa65>::from_seed(&self.seed.into())
    }
}

impl std::fmt::Debug for DilithiumSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DilithiumSigningKey(REDACTED)")
    }
}

/// ML-DSA-65 verification (public) key — FIPS 204 encoded form.
#[derive(Clone, Serialize, Deserialize)]
pub struct DilithiumVerifyKey {
    bytes: Vec<u8>,
}

impl DilithiumVerifyKey {
    /// Reconstruct from FIPS 204 encoded bytes (1952 bytes).
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the length is wrong.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        // Validate by attempting to decode
        let enc = ml_dsa::EncodedVerifyingKey::<MlDsa65>::try_from(bytes).map_err(|_| {
            NexusCryptoError::InvalidKey {
                reason: format!(
                    "invalid ML-DSA-65 public key length: expected 1952, got {}",
                    bytes.len()
                ),
            }
        })?;
        // Ensure it decodes without panic (decode is infallible but we verify the format)
        let _ = ml_dsa::VerifyingKey::<MlDsa65>::decode(&enc);
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    /// View as raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Decode into the inner `ml_dsa::VerifyingKey`.
    fn to_inner(&self) -> ml_dsa::VerifyingKey<MlDsa65> {
        // C2-exempt: invariant — bytes are always validated in from_bytes() or generate_keypair()
        let enc = ml_dsa::EncodedVerifyingKey::<MlDsa65>::try_from(self.bytes.as_slice())
            .expect("internal: DilithiumVerifyKey always holds valid encoded bytes");
        ml_dsa::VerifyingKey::<MlDsa65>::decode(&enc)
    }
}

impl std::fmt::Debug for DilithiumVerifyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = hex::encode(&self.bytes[..4.min(self.bytes.len())]);
        write!(f, "DilithiumVerifyKey({}…)", hex)
    }
}

/// ML-DSA-65 detached signature — FIPS 204 encoded form.
#[derive(Clone, Serialize, Deserialize)]
pub struct DilithiumSignature {
    bytes: Vec<u8>,
}

impl DilithiumSignature {
    /// Reconstruct from FIPS 204 encoded bytes.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidSignature`] if bytes are invalid.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        // Validate by attempting to decode
        let _sig = ml_dsa::Signature::<MlDsa65>::try_from(bytes).map_err(|_| {
            NexusCryptoError::InvalidSignature {
                reason: format!(
                    "invalid ML-DSA-65 signature: cannot decode {} bytes",
                    bytes.len()
                ),
            }
        })?;
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    /// View as raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for DilithiumSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DilithiumSignature({}B)", self.bytes.len())
    }
}

// ── Signer impl ──────────────────────────────────────────────────────────────

/// Marker type for ML-DSA-65 signature scheme.
pub struct DilithiumSigner;

/// Build the domain-prefixed message: `BLAKE3(domain ‖ message)`.
fn domain_message(domain: &[u8], message: &[u8]) -> Vec<u8> {
    let digest = Blake3Hasher::hash(domain, message);
    digest.as_bytes().to_vec()
}

impl DilithiumSigner {
    /// Derive a keypair deterministically from a 32-byte seed.
    ///
    /// This is used when loading a signing key from a file (which stores only
    /// the seed) and the caller needs the corresponding public key.
    pub fn keypair_from_seed(seed: &[u8; 32]) -> (DilithiumSigningKey, DilithiumVerifyKey) {
        let inner = ml_dsa::SigningKey::<MlDsa65>::from_seed(&(*seed).into());
        let vk_bytes: Vec<u8> = <[u8]>::to_vec(inner.verifying_key().encode().as_ref());
        (
            DilithiumSigningKey { seed: *seed },
            DilithiumVerifyKey { bytes: vk_bytes },
        )
    }
}

impl Signer for DilithiumSigner {
    type SigningKey = DilithiumSigningKey;
    type VerifyKey = DilithiumVerifyKey;
    type Signature = DilithiumSignature;

    fn generate_keypair() -> (DilithiumSigningKey, DilithiumVerifyKey) {
        use ml_dsa::KeyGen;

        let mut rng = crate::csprng::OsRng;
        let kp = MlDsa65::key_gen(&mut rng);

        let seed = kp.to_seed();
        let signing_key = DilithiumSigningKey { seed: seed.into() };

        let verify_key = DilithiumVerifyKey {
            bytes: <[u8]>::to_vec(kp.verifying_key().encode().as_ref()),
        };

        (signing_key, verify_key)
    }

    fn sign(sk: &DilithiumSigningKey, domain: &[u8], message: &[u8]) -> DilithiumSignature {
        let msg = domain_message(domain, message);
        let inner = sk.to_inner();
        let sig: ml_dsa::Signature<MlDsa65> = inner.sign(&msg);
        DilithiumSignature {
            bytes: <[u8]>::to_vec(sig.to_bytes().as_ref()),
        }
    }

    fn verify(
        vk: &DilithiumVerifyKey,
        domain: &[u8],
        message: &[u8],
        sig: &DilithiumSignature,
    ) -> Result<(), NexusCryptoError> {
        let msg = domain_message(domain, message);
        let inner_vk = vk.to_inner();
        let inner_sig =
            ml_dsa::Signature::<MlDsa65>::try_from(sig.bytes.as_slice()).map_err(|_| {
                NexusCryptoError::InvalidSignature {
                    reason: "corrupt ML-DSA-65 signature bytes".into(),
                }
            })?;
        inner_vk
            .verify(&msg, &inner_sig)
            .map_err(|_| NexusCryptoError::VerificationFailed {
                reason: "ML-DSA-65 signature verification failed".into(),
            })
    }

    fn signature_size() -> usize {
        // ML-DSA-65 signature: 3309 bytes (FIPS 204)
        3309
    }

    fn verify_key_size() -> usize {
        // ML-DSA-65 verifying key: 1952 bytes (FIPS 204)
        1952
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains;

    #[test]
    fn keypair_generation() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        assert_eq!(vk.as_bytes().len(), DilithiumSigner::verify_key_size());
        assert_eq!(sk.as_bytes().len(), 32); // seed is 32 bytes
        let debug = format!("{:?}", sk);
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn sign_and_verify() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"transfer 100 NXS");
        assert!(DilithiumSigner::verify(&vk, domains::USER_TX, b"transfer 100 NXS", &sig).is_ok());
    }

    #[test]
    fn wrong_message_fails() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"correct");
        assert!(DilithiumSigner::verify(&vk, domains::USER_TX, b"wrong", &sig).is_err());
    }

    #[test]
    fn wrong_domain_fails() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"msg");
        assert!(DilithiumSigner::verify(&vk, domains::SHOAL_VOTE, b"msg", &sig).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let (sk, _vk) = DilithiumSigner::generate_keypair();
        let (_sk2, vk2) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"msg");
        assert!(DilithiumSigner::verify(&vk2, domains::USER_TX, b"msg", &sig).is_err());
    }

    #[test]
    fn signature_roundtrip() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"msg");
        let restored = DilithiumSignature::from_bytes(sig.as_bytes()).unwrap();
        assert!(DilithiumSigner::verify(&vk, domains::USER_TX, b"msg", &restored).is_ok());
    }

    #[test]
    fn verify_key_roundtrip() {
        let (_sk, vk) = DilithiumSigner::generate_keypair();
        let restored = DilithiumVerifyKey::from_bytes(vk.as_bytes()).unwrap();
        assert_eq!(vk.as_bytes(), restored.as_bytes());
    }
}
