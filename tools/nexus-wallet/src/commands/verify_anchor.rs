// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `verify-anchor` — fetch an anchor receipt and optionally verify its digest.

use anyhow::{Context, Result};
use clap::Args;
use nexus_intent::compute_anchor_digest;
use nexus_primitives::Blake3Digest;
use serde::Deserialize;

use crate::rpc_client;

#[derive(Args)]
pub struct VerifyAnchorArgs {
    /// Hex-encoded anchor digest to look up.
    #[arg(long)]
    pub anchor_digest: String,

    /// Comma-separated hex-encoded provenance record IDs to verify
    /// against the anchor digest. If omitted, the command simply
    /// displays the anchor receipt without verification.
    #[arg(long)]
    pub record_ids: Option<String>,

    /// Nexus RPC endpoint URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub rpc_url: String,
}

#[derive(Debug, Deserialize)]
struct AnchorReceiptResp {
    batch_seq: u64,
    anchor_digest: String,
    tx_hash: String,
    block_height: u64,
    anchored_at_ms: u64,
}

pub fn run(args: VerifyAnchorArgs) -> Result<()> {
    rpc_client::validate_rpc_url(&args.rpc_url)?;

    let url = format!(
        "{}/v2/provenance/anchor/{}",
        args.rpc_url.trim_end_matches('/'),
        args.anchor_digest,
    );

    let agent = rpc_client::http_agent();
    let resp: AnchorReceiptResp = agent
        .get(&url)
        .call()
        .context("failed to fetch anchor receipt")?
        .into_json()
        .context("failed to parse anchor receipt response")?;

    println!("Anchor Receipt");
    println!("  batch_seq:     {}", resp.batch_seq);
    println!("  anchor_digest: {}", resp.anchor_digest);
    println!("  tx_hash:       {}", resp.tx_hash);
    println!("  block_height:  {}", resp.block_height);
    println!("  anchored_at:   {} ms", resp.anchored_at_ms);

    if let Some(ref ids_csv) = args.record_ids {
        let record_ids = parse_record_ids(ids_csv)?;
        println!(
            "\nVerifying anchor digest with {} record IDs…",
            record_ids.len()
        );

        let recomputed =
            compute_anchor_digest(&record_ids).context("failed to compute anchor digest")?;

        let expected = parse_hex32(&args.anchor_digest)?;

        if recomputed == expected {
            println!("  ✅ Anchor digest MATCHES — provenance is intact.");
        } else {
            println!("  ❌ Anchor digest MISMATCH — provenance may be tampered.");
            println!("     expected: {}", hex::encode(expected.0));
            println!("     computed: {}", hex::encode(recomputed.0));
            std::process::exit(1);
        }
    } else {
        println!("\nTip: pass --record-ids to verify the digest offline.");
    }

    Ok(())
}

fn parse_hex32(hex_str: &str) -> Result<Blake3Digest> {
    let bytes = hex::decode(hex_str).context("invalid hex")?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Blake3Digest(arr))
}

fn parse_record_ids(csv: &str) -> Result<Vec<Blake3Digest>> {
    csv.split(',').map(|s| parse_hex32(s.trim())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    // ── parse_hex32 ────────────────────────────────────────────────

    #[test]
    fn parse_hex32_valid_32_bytes() {
        let h = hex32(0xAB);
        let digest = parse_hex32(&h).unwrap();
        assert_eq!(digest.0, [0xABu8; 32]);
    }

    #[test]
    fn parse_hex32_wrong_length_fails() {
        let short = hex::encode([1u8; 16]);
        assert!(parse_hex32(&short).is_err());
    }

    #[test]
    fn parse_hex32_invalid_hex_fails() {
        assert!(parse_hex32("not_hex_at_all").is_err());
    }

    #[test]
    fn parse_hex32_accepts_uppercase_hex() {
        let h = hex32(0x0F).to_uppercase();
        let digest = parse_hex32(&h).unwrap();
        assert_eq!(digest.0, [0x0Fu8; 32]);
    }

    // ── parse_record_ids ───────────────────────────────────────────

    #[test]
    fn parse_record_ids_single_entry() {
        let csv = hex32(0x01);
        let ids = parse_record_ids(&csv).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].0, [0x01u8; 32]);
    }

    #[test]
    fn parse_record_ids_multiple_csv_entries() {
        let csv = format!("{},{}", hex32(0x01), hex32(0x02));
        let ids = parse_record_ids(&csv).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].0, [0x01u8; 32]);
        assert_eq!(ids[1].0, [0x02u8; 32]);
    }

    #[test]
    fn parse_record_ids_trims_whitespace_around_entries() {
        let csv = format!(" {} , {} ", hex32(0xAA), hex32(0xBB));
        let ids = parse_record_ids(&csv).unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn parse_record_ids_propagates_parse_error() {
        let csv = format!("{},invalid_hex", hex32(0x01));
        assert!(parse_record_ids(&csv).is_err());
    }

    #[test]
    fn parse_record_ids_three_entries() {
        let csv = format!("{},{},{}", hex32(0x01), hex32(0x02), hex32(0x03));
        let ids = parse_record_ids(&csv).unwrap();
        assert_eq!(ids.len(), 3);
    }
}
