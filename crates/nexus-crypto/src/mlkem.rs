//! ML-KEM-768 key encapsulation mechanism (NIST FIPS 203).
//!
//! Used for establishing **shared secrets** over untrusted channels
//! with post-quantum security at NIST Level 3.
//! Implements the [`KeyEncapsulationMechanism`] trait.
//!
//! Pure Rust implementation via RustCrypto `ml-kem` crate.
//! Migrated from C FFI `pqcrypto-mlkem` per Solutions/20-Pure-Rust-PQ-Crypto-Migration.

use ml_kem::kem::{Decapsulate, Encapsulate};
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::error::NexusCryptoError;
use crate::traits::KeyEncapsulationMechanism;

// ── Key types ─────────────────────────────────────────────────────────────────

/// ML-KEM-768 encapsulation (public) key — FIPS 203 encoded form.
#[derive(Clone, Serialize, Deserialize)]
pub struct KyberEncapsKey {
    bytes: Vec<u8>,
}

impl KyberEncapsKey {
    /// Reconstruct from FIPS 203 encoded bytes (1184 bytes).
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the bytes are invalid.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        // Validate by attempting to construct the inner key
        let enc =
            ml_kem::kem::Key::<ml_kem::EncapsulationKey768>::try_from(bytes).map_err(|_| {
                NexusCryptoError::InvalidKey {
                    reason: format!(
                        "invalid ML-KEM-768 encapsulation key length: expected 1184, got {}",
                        bytes.len()
                    ),
                }
            })?;
        let _ =
            ml_kem::EncapsulationKey768::new(&enc).map_err(|_| NexusCryptoError::InvalidKey {
                reason: "invalid ML-KEM-768 encapsulation key content".into(),
            })?;
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    /// View as raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Decode into the inner `ml_kem::EncapsulationKey768`.
    fn to_inner(&self) -> ml_kem::EncapsulationKey768 {
        // C2-exempt: invariant — bytes are always validated in from_bytes() or generate_keypair()
        let enc = ml_kem::kem::Key::<ml_kem::EncapsulationKey768>::try_from(self.bytes.as_slice())
            .expect("internal: KyberEncapsKey always holds valid encoded bytes");
        ml_kem::EncapsulationKey768::new(&enc)
            .expect("internal: KyberEncapsKey always holds valid key")
    }
}

impl std::fmt::Debug for KyberEncapsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = hex::encode(&self.bytes[..4.min(self.bytes.len())]);
        write!(f, "KyberEncapsKey({}…)", hex)
    }
}

/// ML-KEM-768 decapsulation (private) key — stored as 64-byte seed.
///
/// Zeroized on drop. **Not cloneable**.
#[derive(ZeroizeOnDrop)]
pub struct KyberDecapsKey {
    seed: [u8; 64],
}

impl KyberDecapsKey {
    /// Reconstruct from a 64-byte seed.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidKey`] if the length is not 64.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        let seed: [u8; 64] = bytes.try_into().map_err(|_| NexusCryptoError::InvalidKey {
            reason: format!(
                "invalid ML-KEM-768 seed length: expected 64, got {}",
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

    /// Reconstruct the inner decapsulation key from the stored seed.
    fn to_inner(&self) -> ml_kem::DecapsulationKey768 {
        ml_kem::DecapsulationKey768::from_seed(self.seed.into())
    }
}

impl std::fmt::Debug for KyberDecapsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KyberDecapsKey(REDACTED)")
    }
}

/// ML-KEM-768 ciphertext — FIPS 203 encoded form.
#[derive(Clone, Serialize, Deserialize)]
pub struct KyberCiphertext {
    bytes: Vec<u8>,
}

impl KyberCiphertext {
    /// Reconstruct from raw bytes.
    ///
    /// # Errors
    /// Returns [`NexusCryptoError::InvalidSignature`] if bytes are invalid.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NexusCryptoError> {
        // ML-KEM-768 ciphertext is 1088 bytes
        let _ = ml_kem::ml_kem_768::Ciphertext::try_from(bytes).map_err(|_| {
            NexusCryptoError::InvalidSignature {
                reason: format!(
                    "invalid ML-KEM-768 ciphertext length: expected 1088, got {}",
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

impl std::fmt::Debug for KyberCiphertext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KyberCiphertext({}B)", self.bytes.len())
    }
}

/// ML-KEM-768 shared secret — zeroized on drop.
#[derive(ZeroizeOnDrop)]
pub struct KyberSharedSecret {
    bytes: [u8; 32],
}

impl AsRef<[u8]> for KyberSharedSecret {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for KyberSharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KyberSharedSecret(REDACTED)")
    }
}

impl KyberSharedSecret {
    /// Derive a sub-key using BLAKE3 key derivation.
    ///
    /// `context` should be a unique, domain-specific string identifying the
    /// key's purpose, e.g. `"nexus::network::session::aes256::v1"`.
    pub fn derive_key(&self, context: &str) -> [u8; 32] {
        blake3::derive_key(context, &self.bytes)
    }
}

// ── KEM impl ─────────────────────────────────────────────────────────────────

/// Marker type for ML-KEM-768.
pub struct KyberKem;

impl KeyEncapsulationMechanism for KyberKem {
    type EncapsKey = KyberEncapsKey;
    type DecapsKey = KyberDecapsKey;
    type Ciphertext = KyberCiphertext;
    type SharedSecret = KyberSharedSecret;

    fn generate_keypair() -> (KyberEncapsKey, KyberDecapsKey) {
        use ml_kem::kem::{Generate, KeyExport};

        let mut rng = crate::csprng::OsRng;
        let dk = ml_kem::DecapsulationKey768::generate_from_rng(&mut rng);

        // Export seed (64 bytes) for serialization
        let seed_arr = dk.to_bytes();
        let mut seed = [0u8; 64];
        seed.copy_from_slice(seed_arr.as_ref());

        // Derive encapsulation key
        let ek = dk.encapsulation_key();
        let ek_bytes = ek.to_bytes();

        let encaps_key = KyberEncapsKey {
            bytes: <[u8]>::to_vec(ek_bytes.as_ref()),
        };
        let decaps_key = KyberDecapsKey { seed };
        (encaps_key, decaps_key)
    }

    fn encapsulate(ek: &KyberEncapsKey) -> (KyberSharedSecret, KyberCiphertext) {
        let inner_ek = ek.to_inner();
        let mut rng = crate::csprng::OsRng;
        let (ct, ss) = inner_ek.encapsulate_with_rng(&mut rng);

        let shared = KyberSharedSecret {
            bytes: (*ss)
                .try_into()
                .expect("ML-KEM-768 shared secret is always 32 bytes"),
        };
        let ciphertext = KyberCiphertext {
            bytes: <[u8]>::to_vec(ct.as_ref()),
        };
        (shared, ciphertext)
    }

    fn decapsulate(
        dk: &KyberDecapsKey,
        ct: &KyberCiphertext,
    ) -> Result<KyberSharedSecret, NexusCryptoError> {
        let inner_dk = dk.to_inner();
        let inner_ct =
            ml_kem::ml_kem_768::Ciphertext::try_from(ct.bytes.as_slice()).map_err(|_| {
                NexusCryptoError::DecapsulationFailed {
                    reason: "corrupt ML-KEM-768 ciphertext".into(),
                }
            })?;
        let ss = inner_dk.decapsulate(&inner_ct);
        Ok(KyberSharedSecret {
            bytes: (*ss)
                .try_into()
                .expect("ML-KEM-768 shared secret is always 32 bytes"),
        })
    }

    fn ciphertext_size() -> usize {
        // ML-KEM-768 ciphertext: 1088 bytes (FIPS 203)
        1088
    }

    fn shared_secret_size() -> usize {
        32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_generation() {
        let (ek, dk) = KyberKem::generate_keypair();
        assert!(!ek.as_bytes().is_empty());
        assert_eq!(dk.as_bytes().len(), 64); // seed is 64 bytes
                                             // Debug must not leak key material
        let debug = format!("{:?}", dk);
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn encapsulate_decapsulate_roundtrip() {
        let (ek, dk) = KyberKem::generate_keypair();
        let (ss_sender, ct) = KyberKem::encapsulate(&ek);
        let ss_receiver = KyberKem::decapsulate(&dk, &ct).unwrap();
        assert_eq!(ss_sender.as_ref(), ss_receiver.as_ref());
    }

    #[test]
    fn wrong_key_produces_different_secret() {
        let (ek, _dk) = KyberKem::generate_keypair();
        let (_ek2, dk2) = KyberKem::generate_keypair();
        let (ss_sender, ct) = KyberKem::encapsulate(&ek);
        // ML-KEM-768 decapsulation with wrong key doesn't error but produces
        // a different (implicit rejection) shared secret
        let ss_wrong = KyberKem::decapsulate(&dk2, &ct).unwrap();
        assert_ne!(ss_sender.as_ref(), ss_wrong.as_ref());
    }

    #[test]
    fn shared_secret_size() {
        let (ek, _dk) = KyberKem::generate_keypair();
        let (ss, _ct) = KyberKem::encapsulate(&ek);
        assert_eq!(ss.as_ref().len(), KyberKem::shared_secret_size());
    }

    #[test]
    fn ciphertext_roundtrip() {
        let (ek, dk) = KyberKem::generate_keypair();
        let (ss_orig, ct) = KyberKem::encapsulate(&ek);
        let ct_restored = KyberCiphertext::from_bytes(ct.as_bytes()).unwrap();
        let ss_restored = KyberKem::decapsulate(&dk, &ct_restored).unwrap();
        assert_eq!(ss_orig.as_ref(), ss_restored.as_ref());
    }

    #[test]
    fn derive_key_is_deterministic() {
        let (ek, dk) = KyberKem::generate_keypair();
        let (_ss, ct) = KyberKem::encapsulate(&ek);
        let ss = KyberKem::decapsulate(&dk, &ct).unwrap();
        let k1 = ss.derive_key("nexus::test::v1");
        // Re-derive from same secret bytes should match
        let ss2 = KyberKem::decapsulate(&dk, &ct).unwrap();
        let k2 = ss2.derive_key("nexus::test::v1");
        assert_eq!(k1, k2);
    }

    #[test]
    fn derive_key_different_contexts() {
        let (ek, dk) = KyberKem::generate_keypair();
        let (_ss, ct) = KyberKem::encapsulate(&ek);
        let ss = KyberKem::decapsulate(&dk, &ct).unwrap();
        let k1 = ss.derive_key("nexus::ctx_a::v1");
        let ss2 = KyberKem::decapsulate(&dk, &ct).unwrap();
        let k2 = ss2.derive_key("nexus::ctx_b::v1");
        assert_ne!(k1, k2, "different contexts must produce different keys");
    }

    #[test]
    fn encaps_key_roundtrip() {
        let (ek, _dk) = KyberKem::generate_keypair();
        let restored = KyberEncapsKey::from_bytes(ek.as_bytes()).unwrap();
        assert_eq!(ek.as_bytes(), restored.as_bytes());
    }
}
