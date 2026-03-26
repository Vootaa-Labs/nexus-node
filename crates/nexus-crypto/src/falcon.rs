//! FALCON-512 digital signature scheme (FN-DSA, NIST FIPS 206).
//!
//! Used for **consensus-layer** signatures (Narwhal certificates, Shoal++ votes).
//! Implements [`Signer`] and [`BatchVerifier`] traits.

use pqcrypto_falcon::falcon512;
use pqcrypto_traits::sign::{
    DetachedSignature as DetachedSigTrait, PublicKey as PkTrait, SecretKey as SkTrait,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::error::NexusCryptoError;
use crate::hasher::Blake3Hasher;
use crate::traits::{BatchVerifier, CryptoHasher, Signer};

// ── Key types ─────────────────────────────────────────────────────────────────

/// FALCON-512 signing (private) key.
///
/// Zeroized on drop (SEC-H1). **Not cloneable** — prevents key proliferation.
#[derive(ZeroizeOnDrop)]
pub struct FalconSigningKey {
    raw: Vec<u8>,
}

impl FalconSigningKey {
    /// Reconstruct from raw bytes.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the length is wrong.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        // Validate by round-tripping through pqcrypto
        let _ =
            falcon512::SecretKey::from_bytes(bytes).map_err(|_| NexusCryptoError::InvalidKey {
                reason: format!(
                    "invalid FALCON-512 secret key length: expected {}, got {}",
                    falcon512::secret_key_bytes(),
                    bytes.len()
                ),
            })?;
        Ok(Self {
            raw: bytes.to_vec(),
        })
    }

    /// View the raw secret key bytes.
    ///
    /// # Security
    /// Use only for controlled serialisation (e.g. key export to file).
    /// The returned slice is backed by zeroize-on-drop storage.
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }
}

impl std::fmt::Debug for FalconSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FalconSigningKey(REDACTED)")
    }
}

/// FALCON-512 verification (public) key.
#[derive(Clone, Serialize, Deserialize)]
pub struct FalconVerifyKey {
    bytes: Vec<u8>,
}

impl FalconVerifyKey {
    /// Reconstruct from raw bytes.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the length is wrong.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        let _ =
            falcon512::PublicKey::from_bytes(bytes).map_err(|_| NexusCryptoError::InvalidKey {
                reason: format!(
                    "invalid FALCON-512 public key length: expected {}, got {}",
                    falcon512::public_key_bytes(),
                    bytes.len()
                ),
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

impl std::fmt::Debug for FalconVerifyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = hex::encode(&self.bytes[..4.min(self.bytes.len())]);
        write!(f, "FalconVerifyKey({}…)", hex)
    }
}

/// FALCON-512 detached signature.
#[derive(Clone, Serialize, Deserialize)]
pub struct FalconSignature {
    bytes: Vec<u8>,
}

impl FalconSignature {
    /// Maximum signature size in bytes.
    pub const MAX_SIZE: usize = 809;

    /// Reconstruct from raw bytes.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidSignature`] if bytes are too long.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        let _ = falcon512::DetachedSignature::from_bytes(bytes).map_err(|_| {
            NexusCryptoError::InvalidSignature {
                reason: format!(
                    "invalid FALCON-512 signature length: {} (max {})",
                    bytes.len(),
                    Self::MAX_SIZE,
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

impl std::fmt::Debug for FalconSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FalconSignature({}B)", self.bytes.len())
    }
}

// ── Signer impl ──────────────────────────────────────────────────────────────

/// Marker type for FALCON-512 signature scheme.
pub struct FalconSigner;

/// Build the domain-prefixed message: `BLAKE3(domain ‖ message)`.
fn domain_message(domain: &[u8], message: &[u8]) -> Vec<u8> {
    let digest = Blake3Hasher::hash(domain, message);
    digest.as_bytes().to_vec()
}

impl Signer for FalconSigner {
    type SigningKey = FalconSigningKey;
    type VerifyKey = FalconVerifyKey;
    type Signature = FalconSignature;

    fn generate_keypair() -> (FalconSigningKey, FalconVerifyKey) {
        let (pk, sk) = falcon512::keypair();
        let signing_key = FalconSigningKey {
            raw: sk.as_bytes().to_vec(),
        };
        let verify_key = FalconVerifyKey {
            bytes: pk.as_bytes().to_vec(),
        };
        (signing_key, verify_key)
    }

    fn sign(sk: &FalconSigningKey, domain: &[u8], message: &[u8]) -> FalconSignature {
        let msg = domain_message(domain, message);
        // C2-exempt: internal invariant — sk.raw is always produced by generate_keypair()
        let pq_sk = falcon512::SecretKey::from_bytes(&sk.raw)
            .expect("internal: FalconSigningKey always holds valid bytes");
        let sig = falcon512::detached_sign(&msg, &pq_sk);
        FalconSignature {
            bytes: sig.as_bytes().to_vec(),
        }
    }

    fn verify(
        vk: &FalconVerifyKey,
        domain: &[u8],
        message: &[u8],
        sig: &FalconSignature,
    ) -> Result<(), NexusCryptoError> {
        let msg = domain_message(domain, message);
        let pq_pk = falcon512::PublicKey::from_bytes(&vk.bytes).map_err(|_| {
            NexusCryptoError::InvalidKey {
                reason: "corrupt FALCON-512 public key".into(),
            }
        })?;
        let pq_sig = falcon512::DetachedSignature::from_bytes(&sig.bytes).map_err(|_| {
            NexusCryptoError::InvalidSignature {
                reason: "corrupt FALCON-512 signature bytes".into(),
            }
        })?;
        falcon512::verify_detached_signature(&pq_sig, &msg, &pq_pk).map_err(|_| {
            NexusCryptoError::VerificationFailed {
                reason: "FALCON-512 signature verification failed".into(),
            }
        })
    }

    fn signature_size() -> usize {
        falcon512::signature_bytes()
    }

    fn verify_key_size() -> usize {
        falcon512::public_key_bytes()
    }
}

// ── BatchVerifier impl ────────────────────────────────────────────────────────

impl BatchVerifier for FalconSigner {
    fn batch_verify(
        pairs: &[(FalconVerifyKey, Vec<u8>, Vec<u8>, FalconSignature)],
    ) -> Result<(), NexusCryptoError> {
        let failed: Vec<usize> = pairs
            .par_iter()
            .enumerate()
            .filter_map(|(i, (vk, domain, message, sig))| {
                FalconSigner::verify(vk, domain, message, sig)
                    .err()
                    .map(|_| i)
            })
            .collect();

        if failed.is_empty() {
            Ok(())
        } else {
            Err(NexusCryptoError::BatchVerificationFailed {
                count: failed.len(),
                total: pairs.len(),
                failed_indices: failed,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains;

    #[test]
    fn keypair_generation() {
        let (sk, vk) = FalconSigner::generate_keypair();
        assert!(!vk.as_bytes().is_empty());
        assert_eq!(vk.as_bytes().len(), FalconSigner::verify_key_size());
        // Debug must not leak key material
        let debug = format!("{:?}", sk);
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn sign_and_verify() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::SHOAL_VOTE, b"test message");
        assert!(FalconSigner::verify(&vk, domains::SHOAL_VOTE, b"test message", &sig).is_ok());
    }

    #[test]
    fn wrong_message_fails() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::SHOAL_VOTE, b"correct");
        assert!(FalconSigner::verify(&vk, domains::SHOAL_VOTE, b"wrong", &sig).is_err());
    }

    #[test]
    fn wrong_domain_fails() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::SHOAL_VOTE, b"msg");
        assert!(FalconSigner::verify(&vk, domains::USER_TX, b"msg", &sig).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let (sk, _vk) = FalconSigner::generate_keypair();
        let (_sk2, vk2) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::SHOAL_VOTE, b"msg");
        assert!(FalconSigner::verify(&vk2, domains::SHOAL_VOTE, b"msg", &sig).is_err());
    }

    #[test]
    fn batch_verify_all_valid() {
        let pairs: Vec<_> = (0..4)
            .map(|i| {
                let (sk, vk) = FalconSigner::generate_keypair();
                let msg = format!("message {}", i).into_bytes();
                let sig = FalconSigner::sign(&sk, domains::NARWHAL_CERT, &msg);
                (vk, domains::NARWHAL_CERT.to_vec(), msg, sig)
            })
            .collect();
        assert!(FalconSigner::batch_verify(&pairs).is_ok());
    }

    #[test]
    fn batch_verify_detects_invalid() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::NARWHAL_CERT, b"good");

        let (_sk2, vk2) = FalconSigner::generate_keypair();
        // vk2 won't match sig from sk
        let pairs = vec![
            (
                vk.clone(),
                domains::NARWHAL_CERT.to_vec(),
                b"good".to_vec(),
                sig.clone(),
            ),
            (vk2, domains::NARWHAL_CERT.to_vec(), b"good".to_vec(), sig),
        ];
        let err = FalconSigner::batch_verify(&pairs).unwrap_err();
        match err {
            NexusCryptoError::BatchVerificationFailed {
                count,
                total,
                failed_indices,
            } => {
                assert_eq!(count, 1);
                assert_eq!(total, 2);
                assert_eq!(failed_indices, vec![1]);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn signature_roundtrip() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::SHOAL_VOTE, b"msg");
        let restored = FalconSignature::from_bytes(sig.as_bytes()).unwrap();
        assert!(FalconSigner::verify(&vk, domains::SHOAL_VOTE, b"msg", &restored).is_ok());
    }

    #[test]
    fn verify_key_roundtrip() {
        let (_sk, vk) = FalconSigner::generate_keypair();
        let restored = FalconVerifyKey::from_bytes(vk.as_bytes()).unwrap();
        assert_eq!(vk.as_bytes(), restored.as_bytes());
    }
}
