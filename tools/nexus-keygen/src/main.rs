// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-keygen` — CLI tool for Nexus validator key generation.
//!
//! Generates post-quantum cryptographic key pairs for Nexus validators:
//! - **Falcon-512**: Consensus layer signatures (Narwhal certificates, Shoal++ votes)
//! - **Dilithium3**: User transaction signatures (ML-DSA, NIST Level 3)
//! - **Kyber-768**: Key encapsulation for encrypted P2P channels (ML-KEM)
//!
//! # Usage
//! ```text
//! nexus-keygen validator --output-dir ./keys
//! nexus-keygen falcon --format json --output-dir ./keys
//! nexus-keygen dilithium --output-dir ./keys
//! nexus-keygen kyber --output-dir ./keys
//! ```

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

// ── CLI definitions ──────────────────────────────────────────────────────

/// Nexus validator key generation tool.
///
/// Generates post-quantum cryptographic key pairs for Nexus validators.
/// Secret keys are written with restricted file permissions (owner-only).
#[derive(Parser)]
#[command(name = "nexus-keygen", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a Falcon-512 key pair (consensus signatures).
    Falcon {
        /// Directory to write key files into.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Output format.
        #[arg(long, default_value = "json")]
        format: OutputFormat,

        /// Overwrite existing key files.
        #[arg(long)]
        force: bool,
    },

    /// Generate a Dilithium3 key pair (transaction signatures).
    Dilithium {
        /// Directory to write key files into.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Output format.
        #[arg(long, default_value = "json")]
        format: OutputFormat,

        /// Overwrite existing key files.
        #[arg(long)]
        force: bool,
    },

    /// Generate a Kyber-768 key pair (key encapsulation).
    Kyber {
        /// Directory to write key files into.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Output format.
        #[arg(long, default_value = "json")]
        format: OutputFormat,

        /// Overwrite existing key files.
        #[arg(long)]
        force: bool,
    },

    /// Generate a complete validator key bundle (Falcon + Dilithium + Kyber).
    Validator {
        /// Directory to write key files into.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Output format.
        #[arg(long, default_value = "json")]
        format: OutputFormat,

        /// Overwrite existing key files.
        #[arg(long)]
        force: bool,
    },

    /// Generate a libp2p Ed25519 network identity key.
    ///
    /// Writes `identity.key` (protobuf-encoded) and prints the PeerId to stdout.
    /// Used by `setup-devnet.sh` to pre-compute boot-node multiaddresses.
    Identity {
        /// Directory to write the identity key file into.
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,

        /// Overwrite existing key file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// JSON format with hex-encoded key bytes.
    Json,
    /// Raw hex-encoded key bytes (one file per key).
    Hex,
}

// ── Serializable key bundle types ────────────────────────────────────────

#[derive(Serialize)]
struct KeyFileSecret {
    algorithm: &'static str,
    key_type: &'static str,
    hex: String,
}

#[derive(Serialize)]
struct KeyFilePublic {
    algorithm: &'static str,
    key_type: &'static str,
    hex: String,
}

#[derive(Serialize)]
struct ValidatorBundle {
    falcon_verify_key: String,
    dilithium_verify_key: String,
    kyber_encaps_key: String,
}

// ── Main entry point ─────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Falcon {
            output_dir,
            format,
            force,
        } => generate_falcon(&output_dir, format, force),
        Command::Dilithium {
            output_dir,
            format,
            force,
        } => generate_dilithium(&output_dir, format, force),
        Command::Kyber {
            output_dir,
            format,
            force,
        } => generate_kyber(&output_dir, format, force),
        Command::Validator {
            output_dir,
            format,
            force,
        } => generate_validator_bundle(&output_dir, format, force),
        Command::Identity { output_dir, force } => generate_identity(&output_dir, force),
    }
}

// ── Key generation implementations ───────────────────────────────────────

fn generate_falcon(output_dir: &Path, format: OutputFormat, force: bool) -> Result<()> {
    use nexus_crypto::{FalconSigner, Signer};

    ensure_dir(output_dir)?;

    let (sk, vk) = FalconSigner::generate_keypair();
    let sk_hex = hex::encode(sk.as_bytes());
    let vk_hex = hex::encode(vk.as_bytes());

    write_keypair(
        output_dir,
        "falcon",
        "Falcon-512",
        &sk_hex,
        &vk_hex,
        format,
        force,
    )?;

    eprintln!("Falcon-512 key pair generated in {}", output_dir.display());
    eprintln!("  verify key: {}", truncate_hex(&vk_hex));
    Ok(())
}

fn generate_dilithium(output_dir: &Path, format: OutputFormat, force: bool) -> Result<()> {
    use nexus_crypto::{DilithiumSigner, Signer};

    ensure_dir(output_dir)?;

    let (sk, vk) = DilithiumSigner::generate_keypair();
    let sk_hex = hex::encode(sk.as_bytes());
    let vk_hex = hex::encode(vk.as_bytes());

    write_keypair(
        output_dir,
        "dilithium",
        "Dilithium3",
        &sk_hex,
        &vk_hex,
        format,
        force,
    )?;

    eprintln!("Dilithium3 key pair generated in {}", output_dir.display());
    eprintln!("  verify key: {}", truncate_hex(&vk_hex));
    Ok(())
}

fn generate_kyber(output_dir: &Path, format: OutputFormat, force: bool) -> Result<()> {
    use nexus_crypto::{KeyEncapsulationMechanism, KyberKem};

    ensure_dir(output_dir)?;

    let (ek, dk) = KyberKem::generate_keypair();
    let ek_hex = hex::encode(ek.as_bytes());
    let dk_hex = hex::encode(dk.as_bytes());

    write_keypair(
        output_dir,
        "kyber",
        "Kyber-768",
        &dk_hex,
        &ek_hex,
        format,
        force,
    )?;

    eprintln!("Kyber-768 key pair generated in {}", output_dir.display());
    eprintln!("  encaps key: {}", truncate_hex(&ek_hex));
    Ok(())
}

fn generate_validator_bundle(output_dir: &Path, format: OutputFormat, force: bool) -> Result<()> {
    use nexus_crypto::{
        DilithiumSigner, FalconSigner, KeyEncapsulationMechanism, KyberKem, Signer,
    };

    ensure_dir(output_dir)?;

    // Generate all three key pairs.
    let (falcon_sk, falcon_vk) = FalconSigner::generate_keypair();
    let (dilithium_sk, dilithium_vk) = DilithiumSigner::generate_keypair();
    let (kyber_ek, kyber_dk) = KyberKem::generate_keypair();

    let falcon_sk_hex = hex::encode(falcon_sk.as_bytes());
    let falcon_vk_hex = hex::encode(falcon_vk.as_bytes());
    let dilithium_sk_hex = hex::encode(dilithium_sk.as_bytes());
    let dilithium_vk_hex = hex::encode(dilithium_vk.as_bytes());
    let kyber_ek_hex = hex::encode(kyber_ek.as_bytes());
    let kyber_dk_hex = hex::encode(kyber_dk.as_bytes());

    // Write individual key files.
    write_keypair(
        output_dir,
        "falcon",
        "Falcon-512",
        &falcon_sk_hex,
        &falcon_vk_hex,
        format,
        force,
    )?;
    write_keypair(
        output_dir,
        "dilithium",
        "Dilithium3",
        &dilithium_sk_hex,
        &dilithium_vk_hex,
        format,
        force,
    )?;
    write_keypair(
        output_dir,
        "kyber",
        "Kyber-768",
        &kyber_dk_hex,
        &kyber_ek_hex,
        format,
        force,
    )?;

    // Write combined public-key bundle for easy distribution.
    let bundle = ValidatorBundle {
        falcon_verify_key: falcon_vk_hex.clone(),
        dilithium_verify_key: dilithium_vk_hex.clone(),
        kyber_encaps_key: kyber_ek_hex.clone(),
    };
    let bundle_path = output_dir.join("validator-public-keys.json");
    check_overwrite(&bundle_path, force)?;
    let bundle_json =
        serde_json::to_string_pretty(&bundle).context("failed to serialize validator bundle")?;
    fs::write(&bundle_path, &bundle_json).context("failed to write validator bundle")?;

    eprintln!("Validator key bundle generated in {}", output_dir.display());
    eprintln!("  falcon  vk: {}", truncate_hex(&falcon_vk_hex));
    eprintln!("  dilithium vk: {}", truncate_hex(&dilithium_vk_hex));
    eprintln!("  kyber   ek: {}", truncate_hex(&kyber_ek_hex));
    eprintln!("  bundle: {}", bundle_path.display());
    Ok(())
}

// ── Network identity key generation ──────────────────────────────────────

fn generate_identity(output_dir: &Path, force: bool) -> Result<()> {
    use libp2p_identity::Keypair;

    ensure_dir(output_dir)?;

    let keypair = Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    let encoded = keypair
        .to_protobuf_encoding()
        .context("failed to encode identity keypair")?;

    let key_path = output_dir.join("identity.key");
    check_overwrite(&key_path, force)?;
    write_secret_file(&key_path, &encoded)?;

    // Print PeerId to stdout (machine-readable) and summary to stderr.
    println!("{peer_id}");
    eprintln!("Network identity key generated in {}", output_dir.display());
    eprintln!("  peer id: {peer_id}");
    eprintln!("  key file: {}", key_path.display());
    Ok(())
}

// ── File I/O helpers ─────────────────────────────────────────────────────

fn ensure_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;
    }
    Ok(())
}

fn check_overwrite(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "file already exists: {} (use --force to overwrite)",
            path.display()
        );
    }
    Ok(())
}

fn write_keypair(
    dir: &Path,
    prefix: &str,
    algorithm: &'static str,
    secret_hex: &str,
    public_hex: &str,
    format: OutputFormat,
    force: bool,
) -> Result<()> {
    match format {
        OutputFormat::Json => {
            let sk_path = dir.join(format!("{prefix}-secret.json"));
            let vk_path = dir.join(format!("{prefix}-public.json"));

            check_overwrite(&sk_path, force)?;
            check_overwrite(&vk_path, force)?;

            let sk_json = serde_json::to_string_pretty(&KeyFileSecret {
                algorithm,
                key_type: "secret",
                hex: secret_hex.to_owned(),
            })
            .context("failed to serialize secret key")?;

            let vk_json = serde_json::to_string_pretty(&KeyFilePublic {
                algorithm,
                key_type: "public",
                hex: public_hex.to_owned(),
            })
            .context("failed to serialize public key")?;

            write_secret_file(&sk_path, sk_json.as_bytes())?;
            fs::write(&vk_path, &vk_json).context("failed to write public key file")?;
        }
        OutputFormat::Hex => {
            let sk_path = dir.join(format!("{prefix}.sk"));
            let vk_path = dir.join(format!("{prefix}.vk"));

            check_overwrite(&sk_path, force)?;
            check_overwrite(&vk_path, force)?;

            write_secret_file(&sk_path, secret_hex.as_bytes())?;
            fs::write(&vk_path, public_hex).context("failed to write public key file")?;
        }
    }
    Ok(())
}

/// Write a file with owner-only permissions (0600) to protect secret key material.
fn write_secret_file(path: &Path, data: &[u8]) -> Result<()> {
    fs::write(path, data)
        .with_context(|| format!("failed to write secret file: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to set permissions on: {}", path.display()))?;
    }

    Ok(())
}

fn truncate_hex(h: &str) -> String {
    if h.len() > 16 {
        format!("{}...{}", &h[..8], &h[h.len() - 8..])
    } else {
        h.to_owned()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_falcon_keygen_json() {
        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Json, false).unwrap();

        let sk_path = dir.path().join("falcon-secret.json");
        let vk_path = dir.path().join("falcon-public.json");
        assert!(sk_path.exists(), "secret key file should exist");
        assert!(vk_path.exists(), "public key file should exist");

        // Verify JSON is parseable and has correct fields.
        let sk_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sk_path).unwrap()).unwrap();
        assert_eq!(sk_content["algorithm"], "Falcon-512");
        assert_eq!(sk_content["key_type"], "secret");
        assert!(!sk_content["hex"].as_str().unwrap().is_empty());

        let vk_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&vk_path).unwrap()).unwrap();
        assert_eq!(vk_content["algorithm"], "Falcon-512");
        assert_eq!(vk_content["key_type"], "public");
    }

    #[test]
    fn test_falcon_keygen_hex() {
        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Hex, false).unwrap();

        let sk = fs::read_to_string(dir.path().join("falcon.sk")).unwrap();
        let vk = fs::read_to_string(dir.path().join("falcon.vk")).unwrap();
        assert!(!sk.is_empty());
        assert!(!vk.is_empty());
        // Verify valid hex.
        hex::decode(&sk).expect("secret key should be valid hex");
        hex::decode(&vk).expect("verify key should be valid hex");
    }

    #[test]
    fn test_dilithium_keygen_json() {
        let dir = TempDir::new().unwrap();
        generate_dilithium(dir.path(), OutputFormat::Json, false).unwrap();

        let sk_path = dir.path().join("dilithium-secret.json");
        let vk_path = dir.path().join("dilithium-public.json");
        assert!(sk_path.exists());
        assert!(vk_path.exists());

        let vk_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&vk_path).unwrap()).unwrap();
        assert_eq!(vk_content["algorithm"], "Dilithium3");
    }

    #[test]
    fn test_kyber_keygen_json() {
        let dir = TempDir::new().unwrap();
        generate_kyber(dir.path(), OutputFormat::Json, false).unwrap();

        let sk_path = dir.path().join("kyber-secret.json");
        let vk_path = dir.path().join("kyber-public.json");
        assert!(sk_path.exists());
        assert!(vk_path.exists());

        let vk_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&vk_path).unwrap()).unwrap();
        assert_eq!(vk_content["algorithm"], "Kyber-768");
    }

    #[test]
    fn test_validator_bundle() {
        let dir = TempDir::new().unwrap();
        generate_validator_bundle(dir.path(), OutputFormat::Json, false).unwrap();

        // All key files should exist.
        assert!(dir.path().join("falcon-secret.json").exists());
        assert!(dir.path().join("falcon-public.json").exists());
        assert!(dir.path().join("dilithium-secret.json").exists());
        assert!(dir.path().join("dilithium-public.json").exists());
        assert!(dir.path().join("kyber-secret.json").exists());
        assert!(dir.path().join("kyber-public.json").exists());

        // Validator bundle should exist and be parseable.
        let bundle_path = dir.path().join("validator-public-keys.json");
        assert!(bundle_path.exists());
        let bundle: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&bundle_path).unwrap()).unwrap();
        assert!(!bundle["falcon_verify_key"].as_str().unwrap().is_empty());
        assert!(!bundle["dilithium_verify_key"].as_str().unwrap().is_empty());
        assert!(!bundle["kyber_encaps_key"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_no_overwrite_without_force() {
        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Json, false).unwrap();
        let result = generate_falcon(dir.path(), OutputFormat::Json, false);
        assert!(
            result.is_err(),
            "should refuse to overwrite without --force"
        );
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_overwrite_with_force() {
        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Json, false).unwrap();
        generate_falcon(dir.path(), OutputFormat::Json, true).unwrap();
    }

    #[test]
    fn test_creates_output_directory() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested").join("keys");
        generate_falcon(&nested, OutputFormat::Hex, false).unwrap();
        assert!(nested.join("falcon.sk").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_secret_key_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Json, false).unwrap();

        let perms = fs::metadata(dir.path().join("falcon-secret.json"))
            .unwrap()
            .permissions();
        assert_eq!(perms.mode() & 0o777, 0o600, "secret key should be 0600");
    }

    #[test]
    fn test_round_trip_falcon_keys() {
        use nexus_crypto::{FalconSigningKey, FalconVerifyKey};

        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Hex, false).unwrap();

        let sk_hex = fs::read_to_string(dir.path().join("falcon.sk")).unwrap();
        let vk_hex = fs::read_to_string(dir.path().join("falcon.vk")).unwrap();

        let sk_bytes = hex::decode(&sk_hex).unwrap();
        let vk_bytes = hex::decode(&vk_hex).unwrap();

        FalconSigningKey::from_bytes(&sk_bytes).expect("should reconstruct signing key");
        FalconVerifyKey::from_bytes(&vk_bytes).expect("should reconstruct verify key");
    }

    #[test]
    fn test_round_trip_dilithium_keys() {
        use nexus_crypto::{DilithiumSigningKey, DilithiumVerifyKey};

        let dir = TempDir::new().unwrap();
        generate_dilithium(dir.path(), OutputFormat::Hex, false).unwrap();

        let sk_hex = fs::read_to_string(dir.path().join("dilithium.sk")).unwrap();
        let vk_hex = fs::read_to_string(dir.path().join("dilithium.vk")).unwrap();

        DilithiumSigningKey::from_bytes(&hex::decode(&sk_hex).unwrap())
            .expect("should reconstruct signing key");
        DilithiumVerifyKey::from_bytes(&hex::decode(&vk_hex).unwrap())
            .expect("should reconstruct verify key");
    }

    #[test]
    fn test_round_trip_kyber_keys() {
        use nexus_crypto::{KyberDecapsKey, KyberEncapsKey};

        let dir = TempDir::new().unwrap();
        generate_kyber(dir.path(), OutputFormat::Hex, false).unwrap();

        let dk_hex = fs::read_to_string(dir.path().join("kyber.sk")).unwrap();
        let ek_hex = fs::read_to_string(dir.path().join("kyber.vk")).unwrap();

        KyberDecapsKey::from_bytes(&hex::decode(&dk_hex).unwrap())
            .expect("should reconstruct decaps key");
        KyberEncapsKey::from_bytes(&hex::decode(&ek_hex).unwrap())
            .expect("should reconstruct encaps key");
    }

    #[test]
    fn test_truncate_hex_short() {
        assert_eq!(truncate_hex("abcdef"), "abcdef");
    }

    #[test]
    fn test_truncate_hex_long() {
        let long = "abcdef0123456789abcdef0123456789";
        let truncated = truncate_hex(long);
        assert!(truncated.contains("..."));
        assert_eq!(&truncated[..8], "abcdef01");
    }

    #[test]
    fn test_identity_keygen() {
        let dir = TempDir::new().unwrap();
        generate_identity(dir.path(), false).unwrap();

        let key_path = dir.path().join("identity.key");
        assert!(key_path.exists(), "identity.key should exist");

        // Round-trip: load the key and derive the same PeerId.
        let bytes = fs::read(&key_path).unwrap();
        let keypair = libp2p_identity::Keypair::from_protobuf_encoding(&bytes)
            .expect("should decode identity key");
        let peer_id = keypair.public().to_peer_id();
        assert!(
            peer_id.to_string().starts_with("12D3KooW"),
            "PeerId should have expected prefix"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_identity_key_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        generate_identity(dir.path(), false).unwrap();

        let perms = fs::metadata(dir.path().join("identity.key"))
            .unwrap()
            .permissions();
        assert_eq!(perms.mode() & 0o777, 0o600, "identity key should be 0600");
    }

    #[test]
    fn test_dilithium_keygen_hex() {
        let dir = TempDir::new().unwrap();
        generate_dilithium(dir.path(), OutputFormat::Hex, false).unwrap();

        let sk = fs::read_to_string(dir.path().join("dilithium.sk")).unwrap();
        let vk = fs::read_to_string(dir.path().join("dilithium.vk")).unwrap();
        assert!(!sk.is_empty());
        assert!(!vk.is_empty());
        hex::decode(&sk).expect("dilithium secret key should be valid hex");
        hex::decode(&vk).expect("dilithium verify key should be valid hex");
    }

    #[test]
    fn test_kyber_keygen_hex() {
        let dir = TempDir::new().unwrap();
        generate_kyber(dir.path(), OutputFormat::Hex, false).unwrap();

        let sk = fs::read_to_string(dir.path().join("kyber.sk")).unwrap();
        let vk = fs::read_to_string(dir.path().join("kyber.vk")).unwrap();
        assert!(!sk.is_empty());
        assert!(!vk.is_empty());
        hex::decode(&sk).expect("kyber secret key should be valid hex");
        hex::decode(&vk).expect("kyber encaps key should be valid hex");
    }

    #[test]
    fn test_identity_keygen_force_overwrite() {
        let dir = TempDir::new().unwrap();
        generate_identity(dir.path(), false).unwrap();
        // Second call with force=true should not error.
        generate_identity(dir.path(), true).unwrap();
        assert!(dir.path().join("identity.key").exists());
    }

    #[test]
    fn test_check_overwrite_passes_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("absent.json");
        // File does not exist → should succeed regardless of force flag.
        check_overwrite(&absent, false).unwrap();
        check_overwrite(&absent, true).unwrap();
    }

    #[test]
    fn test_validator_bundle_hex() {
        // Covers generate_validator_bundle with OutputFormat::Hex.
        let dir = TempDir::new().unwrap();
        generate_validator_bundle(dir.path(), OutputFormat::Hex, false).unwrap();

        assert!(dir.path().join("falcon.sk").exists());
        assert!(dir.path().join("falcon.vk").exists());
        assert!(dir.path().join("dilithium.sk").exists());
        assert!(dir.path().join("dilithium.vk").exists());
        assert!(dir.path().join("kyber.sk").exists());
        assert!(dir.path().join("kyber.vk").exists());
        assert!(dir.path().join("validator-public-keys.json").exists());
    }

    #[test]
    fn test_generate_falcon_no_overwrite_hex() {
        // Covers the Hex branch of check_overwrite failure.
        let dir = TempDir::new().unwrap();
        generate_falcon(dir.path(), OutputFormat::Hex, false).unwrap();
        let err = generate_falcon(dir.path(), OutputFormat::Hex, false).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn test_validator_bundle_overwrite() {
        let dir = TempDir::new().unwrap();
        generate_validator_bundle(dir.path(), OutputFormat::Json, false).unwrap();
        // With force=true should succeed even though files exist.
        generate_validator_bundle(dir.path(), OutputFormat::Json, true).unwrap();
    }

    #[test]
    fn test_ensure_dir_existing() {
        // ensure_dir on an already-existing directory should succeed.
        let dir = TempDir::new().unwrap();
        ensure_dir(dir.path()).unwrap();
    }
}
