//! Cryptographic test fixtures: keypairs, signatures, hash outputs.
//!
//! These helpers wrap the `nexus-crypto` API to provide convenient one-line
//! keypair generation and signing for test code.

use nexus_crypto::{
    domains, Blake3Hasher, DilithiumSigner, FalconSigner, KeyEncapsulationMechanism, KyberKem,
    Signer,
};
use nexus_crypto::{
    DilithiumSigningKey, DilithiumVerifyKey, FalconSigningKey, FalconVerifyKey, KyberDecapsKey,
    KyberEncapsKey,
};
use nexus_primitives::Blake3Digest;

// ── Falcon (consensus layer) ─────────────────────────────────────────────────

/// Generate a Falcon-512 keypair for test use.
pub fn make_falcon_keypair() -> (FalconSigningKey, FalconVerifyKey) {
    FalconSigner::generate_keypair()
}

/// Sign `message` with Falcon using the Narwhal batch domain.
pub fn falcon_sign_batch(sk: &FalconSigningKey, message: &[u8]) -> nexus_crypto::FalconSignature {
    FalconSigner::sign(sk, domains::NARWHAL_BATCH, message)
}

/// Sign `message` with Falcon using the Narwhal certificate domain.
pub fn falcon_sign_cert(sk: &FalconSigningKey, message: &[u8]) -> nexus_crypto::FalconSignature {
    FalconSigner::sign(sk, domains::NARWHAL_CERT, message)
}

// ── Dilithium (user transaction layer) ───────────────────────────────────────

/// Generate a Dilithium3 keypair for test use.
pub fn make_dilithium_keypair() -> (DilithiumSigningKey, DilithiumVerifyKey) {
    DilithiumSigner::generate_keypair()
}

/// Sign `message` with Dilithium using the user transaction domain.
pub fn dilithium_sign_tx(
    sk: &DilithiumSigningKey,
    message: &[u8],
) -> nexus_crypto::DilithiumSignature {
    DilithiumSigner::sign(sk, domains::USER_TX, message)
}

// ── Kyber (key encapsulation) ────────────────────────────────────────────────

/// Generate a Kyber-768 keypair for test use.
pub fn make_kyber_keypair() -> (KyberEncapsKey, KyberDecapsKey) {
    KyberKem::generate_keypair()
}

// ── Blake3 Hashing ───────────────────────────────────────────────────────────

/// Hash `data` with a test domain separator.
pub fn hash_with_domain(domain: &[u8], data: &[u8]) -> Blake3Digest {
    Blake3Hasher::digest(domain, data)
}

/// Hash `data` using the user transaction domain.
pub fn hash_tx(data: &[u8]) -> Blake3Digest {
    Blake3Hasher::digest(domains::USER_TX, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::Signer;

    #[test]
    fn falcon_keypair_and_sign() {
        let (sk, vk) = make_falcon_keypair();
        let sig = falcon_sign_batch(&sk, b"test message");
        FalconSigner::verify(&vk, domains::NARWHAL_BATCH, b"test message", &sig)
            .expect("should verify");
    }

    #[test]
    fn dilithium_keypair_and_sign() {
        let (sk, vk) = make_dilithium_keypair();
        let sig = dilithium_sign_tx(&sk, b"test tx");
        DilithiumSigner::verify(&vk, domains::USER_TX, b"test tx", &sig).expect("should verify");
    }

    #[test]
    fn kyber_encaps_decaps() {
        let (ek, dk) = make_kyber_keypair();
        let (ss1, ct) = KyberKem::encapsulate(&ek);
        let ss2 = KyberKem::decapsulate(&dk, &ct).expect("should decapsulate");
        assert_eq!(ss1.as_ref(), ss2.as_ref());
    }

    #[test]
    fn hash_deterministic() {
        let h1 = hash_tx(b"same input");
        let h2 = hash_tx(b"same input");
        assert_eq!(h1, h2);
    }
}
