// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Domain-specific assertion helpers.
//!
//! These functions provide clear, consistent error messages for common
//! test assertions across the Nexus workspace.

use nexus_crypto::Signer;
use nexus_primitives::Blake3Digest;

/// Assert that a signature verifies correctly.
///
/// Panics with a descriptive message if verification fails.
pub fn assert_signature_valid<S: Signer>(
    vk: &S::VerifyKey,
    domain: &[u8],
    message: &[u8],
    sig: &S::Signature,
) {
    S::verify(vk, domain, message, sig).expect("signature should be valid");
}

/// Assert that a signature does **not** verify.
///
/// Panics if verification unexpectedly succeeds.
pub fn assert_signature_invalid<S: Signer>(
    vk: &S::VerifyKey,
    domain: &[u8],
    message: &[u8],
    sig: &S::Signature,
) {
    assert!(
        S::verify(vk, domain, message, sig).is_err(),
        "signature should not verify but did"
    );
}

/// Assert that two digests are equal, with clear hex output on failure.
pub fn assert_digests_equal(a: &Blake3Digest, b: &Blake3Digest) {
    assert_eq!(
        a.as_bytes(),
        b.as_bytes(),
        "digests differ:\n  left:  {}\n  right: {}",
        a.to_hex(),
        b.to_hex(),
    );
}

/// Assert that a hash computation is deterministic: hashing the same
/// input with the same domain twice yields the same digest.
pub fn assert_hash_deterministic(domain: &[u8], data: &[u8]) {
    let h1 = nexus_crypto::Blake3Hasher::digest(domain, data);
    let h2 = nexus_crypto::Blake3Hasher::digest(domain, data);
    assert_digests_equal(&h1, &h2);
}

/// Assert that a JSON serialization roundtrip preserves the value.
///
/// The type must implement `Serialize + DeserializeOwned + PartialEq + Debug`.
pub fn assert_json_roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("JSON serialize should succeed");
    let restored: T = serde_json::from_str(&json).expect("JSON deserialize should succeed");
    assert_eq!(
        *value, restored,
        "JSON roundtrip changed the value:\n  json: {json}"
    );
}

/// Assert a `Result` is `Ok`, returning the inner value. Panics with context on `Err`.
pub fn assert_ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(v) => v,
        Err(e) => panic!("{context}: expected Ok, got Err({e:?})"),
    }
}

/// Assert a `Result` is `Err`. Panics with context on `Ok`.
pub fn assert_err<T: std::fmt::Debug, E>(result: Result<T, E>, context: &str) -> E {
    match result {
        Ok(v) => panic!("{context}: expected Err, got Ok({v:?})"),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{domains, Blake3Hasher, DilithiumSigner, FalconSigner};

    #[test]
    fn assert_valid_falcon_signature() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::NARWHAL_BATCH, b"test");
        assert_signature_valid::<FalconSigner>(&vk, domains::NARWHAL_BATCH, b"test", &sig);
    }

    #[test]
    fn assert_invalid_falcon_signature() {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sig = FalconSigner::sign(&sk, domains::NARWHAL_BATCH, b"test");
        assert_signature_invalid::<FalconSigner>(&vk, domains::NARWHAL_BATCH, b"wrong", &sig);
    }

    #[test]
    fn assert_dilithium_valid() {
        let (sk, vk) = DilithiumSigner::generate_keypair();
        let sig = DilithiumSigner::sign(&sk, domains::USER_TX, b"tx data");
        assert_signature_valid::<DilithiumSigner>(&vk, domains::USER_TX, b"tx data", &sig);
    }

    #[test]
    fn digest_equality() {
        let d = Blake3Hasher::digest(b"test", b"data");
        assert_digests_equal(&d, &d);
    }

    #[test]
    fn hash_determinism() {
        assert_hash_deterministic(b"domain", b"data");
    }

    #[test]
    fn json_roundtrip_primitive() {
        let val = 42u64;
        assert_json_roundtrip(&val);
    }

    #[test]
    fn assert_ok_works() {
        let r: Result<i32, &str> = Ok(42);
        assert_eq!(assert_ok(r, "should be ok"), 42);
    }

    #[test]
    fn assert_err_works() {
        let r: Result<i32, &str> = Err("bad");
        assert_eq!(assert_err(r, "should be err"), "bad");
    }
}
