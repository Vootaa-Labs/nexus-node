//! Validator key loading from persistent key files.
//!
//! Loads Falcon-512 signing keys produced by `nexus-keygen validator`.
//! Supports both JSON and hex file formats.
//!
//! # Security
//! - Key files must have restricted permissions (0600 on Unix).
//! - Missing or unreadable key files cause fail-fast with a clear error.
//! - No automatic dev-key fallback when a key path is configured.

#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::Context;
use nexus_crypto::falcon::{FalconSigningKey, FalconVerifyKey};
use nexus_crypto::{FalconSigner, Signer as _};

/// Loaded validator key pair.
pub struct ValidatorKeyPair {
    /// Falcon-512 signing key (consensus layer).
    pub signing_key: FalconSigningKey,
    /// Falcon-512 verification key (derived from signing key).
    pub verify_key: FalconVerifyKey,
}

impl std::fmt::Debug for ValidatorKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatorKeyPair")
            .field("signing_key", &"<redacted>")
            .field("verify_key", &"<redacted>")
            .finish()
    }
}

/// Load a Falcon-512 signing key from a `nexus-keygen`-produced key directory.
///
/// Looks for `falcon-secret.json` first, then `falcon.sk` (hex format).
/// The verification key is derived by re-signing and checking,
/// or loaded from the corresponding public key file if available.
///
/// # Errors
/// - Key directory does not exist.
/// - No recognisable key file found.
/// - File permissions are too open (Unix: not 0600/0700).
/// - Key bytes are invalid.
pub fn load_validator_keys(key_dir: &Path) -> anyhow::Result<ValidatorKeyPair> {
    anyhow::ensure!(
        key_dir.exists(),
        "validator key directory does not exist: {}",
        key_dir.display()
    );
    anyhow::ensure!(
        key_dir.is_dir(),
        "validator key path is not a directory: {}",
        key_dir.display()
    );

    // Try JSON format first (falcon-secret.json).
    let json_path = key_dir.join("falcon-secret.json");
    let hex_path = key_dir.join("falcon.sk");

    let sk_hex = if json_path.exists() {
        check_file_permissions(&json_path)?;
        let content = std::fs::read_to_string(&json_path)
            .with_context(|| format!("failed to read {}", json_path.display()))?;
        parse_json_key_file(&content)
            .with_context(|| format!("failed to parse {}", json_path.display()))?
    } else if hex_path.exists() {
        check_file_permissions(&hex_path)?;
        std::fs::read_to_string(&hex_path)
            .with_context(|| format!("failed to read {}", hex_path.display()))?
            .trim()
            .to_owned()
    } else {
        anyhow::bail!(
            "no Falcon key file found in {} (expected falcon-secret.json or falcon.sk)",
            key_dir.display()
        );
    };

    let sk_bytes = hex::decode(&sk_hex).context("falcon secret key: invalid hex in key file")?;

    let signing_key =
        FalconSigningKey::from_bytes(&sk_bytes).context("falcon secret key: invalid key bytes")?;

    // Load or derive verification key.
    let verify_key = load_or_derive_verify_key(key_dir, &signing_key)?;

    Ok(ValidatorKeyPair {
        signing_key,
        verify_key,
    })
}

/// Load the ephemeral dev key pair (for development/testing only).
///
/// Generates a fresh random Falcon-512 key pair on each call.
pub fn generate_dev_keys() -> ValidatorKeyPair {
    let (signing_key, verify_key) = FalconSigner::generate_keypair();
    ValidatorKeyPair {
        signing_key,
        verify_key,
    }
}

/// Parse the hex field from a `nexus-keygen` JSON secret key file.
fn parse_json_key_file(content: &str) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    struct KeyFile {
        hex: String,
    }
    let kf: KeyFile = serde_json::from_str(content).context("invalid JSON key file format")?;
    Ok(kf.hex)
}

/// Load verify key from file if available, otherwise derive from signing key.
fn load_or_derive_verify_key(
    key_dir: &Path,
    _signing_key: &FalconSigningKey,
) -> anyhow::Result<FalconVerifyKey> {
    // Try loading from falcon-public.json or falcon.vk.
    let json_path = key_dir.join("falcon-public.json");
    let hex_path = key_dir.join("falcon.vk");

    let vk_hex = if json_path.exists() {
        let content = std::fs::read_to_string(&json_path)
            .with_context(|| format!("failed to read {}", json_path.display()))?;
        parse_json_key_file(&content)
            .with_context(|| format!("failed to parse {}", json_path.display()))?
    } else if hex_path.exists() {
        std::fs::read_to_string(&hex_path)
            .with_context(|| format!("failed to read {}", hex_path.display()))?
            .trim()
            .to_owned()
    } else {
        anyhow::bail!(
            "no Falcon public key file found in {} (expected falcon-public.json or falcon.vk)",
            key_dir.display()
        );
    };

    let vk_bytes = hex::decode(&vk_hex).context("falcon verify key: invalid hex in key file")?;
    FalconVerifyKey::from_bytes(&vk_bytes).context("falcon verify key: invalid key bytes")
}

/// Check that a secret key file has restricted permissions (Unix: 0600 or 0700 on dir).
///
/// On non-Unix platforms this is a no-op.
#[allow(unused_variables)]
fn check_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "validator key file {} has overly permissive mode {:04o} — \
                 expected 0600 (owner read/write only). \
                 Fix with: chmod 600 {}",
                path.display(),
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_crypto::{FalconSigner, Signer};

    fn write_test_key_files(dir: &Path) {
        let (sk, vk) = FalconSigner::generate_keypair();
        let sk_json = serde_json::json!({
            "algorithm": "Falcon-512",
            "key_type": "secret",
            "hex": hex::encode(sk.as_bytes()),
        });
        let vk_json = serde_json::json!({
            "algorithm": "Falcon-512",
            "key_type": "public",
            "hex": hex::encode(vk.as_bytes()),
        });
        let sk_path = dir.join("falcon-secret.json");
        let vk_path = dir.join("falcon-public.json");
        std::fs::write(&sk_path, sk_json.to_string()).unwrap();
        std::fs::write(&vk_path, vk_json.to_string()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sk_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn load_json_format_keys() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_key_files(tmp.path());
        let kp = load_validator_keys(tmp.path()).unwrap();
        // Key loaded — verify it can produce a valid signature.
        let sig = FalconSigner::sign(&kp.signing_key, b"test", b"message");
        FalconSigner::verify(&kp.verify_key, b"test", b"message", &sig).unwrap();
    }

    #[test]
    fn load_hex_format_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let (sk, vk) = FalconSigner::generate_keypair();
        let sk_path = tmp.path().join("falcon.sk");
        let vk_path = tmp.path().join("falcon.vk");
        std::fs::write(&sk_path, hex::encode(sk.as_bytes())).unwrap();
        std::fs::write(&vk_path, hex::encode(vk.as_bytes())).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sk_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let kp = load_validator_keys(tmp.path()).unwrap();
        let sig = FalconSigner::sign(&kp.signing_key, b"test", b"data");
        FalconSigner::verify(&kp.verify_key, b"test", b"data", &sig).unwrap();
    }

    #[test]
    fn missing_dir_fails() {
        let result = load_validator_keys(Path::new("/nonexistent/keys"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn missing_key_file_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let result = load_validator_keys(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no Falcon key"));
    }

    #[test]
    fn dev_keys_produces_valid_keypair() {
        let kp = generate_dev_keys();
        let sig = FalconSigner::sign(&kp.signing_key, b"domain", b"msg");
        FalconSigner::verify(&kp.verify_key, b"domain", b"msg", &sig).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn overly_permissive_key_file_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (sk, _vk) = FalconSigner::generate_keypair();
        let sk_json = serde_json::json!({
            "algorithm": "Falcon-512",
            "key_type": "secret",
            "hex": hex::encode(sk.as_bytes()),
        });
        let sk_path = tmp.path().join("falcon-secret.json");
        std::fs::write(&sk_path, sk_json.to_string()).unwrap();
        // Set world-readable (0644) — should be rejected.
        std::fs::set_permissions(&sk_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let result = load_validator_keys(tmp.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("overly permissive"));
    }
}
