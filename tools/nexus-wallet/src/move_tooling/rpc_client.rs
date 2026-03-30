// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Lightweight RPC client for submitting transactions to a Nexus node.
//!
//! Uses `ureq` (synchronous HTTP) to keep the CLI dependency footprint small.

use std::time::Duration;

use anyhow::{Context, Result};
use nexus_crypto::{DilithiumSigner, DilithiumSigningKey, DilithiumVerifyKey, Signer};
use nexus_execution::types::{compute_tx_digest, TransactionBody};
use nexus_execution::SignedTransaction;
use nexus_primitives::AccountAddress;
use serde::de::Error as _;
use serde::Deserialize;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build()
}

pub fn validate_rpc_url(url: &str) -> Result<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("RPC URL must use http:// or https:// scheme, got: {url}");
    }
    Ok(())
}

#[derive(Deserialize)]
struct KeyFileJson {
    #[serde(rename = "algorithm")]
    _algorithm: String,
    #[serde(rename = "key_type")]
    _key_type: String,
    hex: String,
}

pub struct Identity {
    pub sk: DilithiumSigningKey,
    pub pk: DilithiumVerifyKey,
    pub address: AccountAddress,
}

pub fn load_identity(path: &std::path::Path) -> Result<Identity> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let content = content.trim();

    let sk_hex = if content.starts_with('{') {
        let kf: KeyFileJson = serde_json::from_str(content)
            .with_context(|| format!("parsing key file JSON at {}", path.display()))?;
        kf.hex
    } else {
        content.to_string()
    };

    let sk_bytes = hex::decode(&sk_hex).context("decoding secret key hex")?;
    let sk = DilithiumSigningKey::from_bytes(&sk_bytes).context("invalid Dilithium secret key")?;
    let (_, pk) = derive_keypair_from_sk(&sk)?;
    let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());

    Ok(Identity { sk, pk, address })
}

pub fn ephemeral_identity() -> Identity {
    let (sk, pk) = DilithiumSigner::generate_keypair();
    let address = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
    Identity { sk, pk, address }
}

fn derive_keypair_from_sk(
    sk: &DilithiumSigningKey,
) -> Result<(DilithiumSigningKey, DilithiumVerifyKey)> {
    let seed: [u8; 32] = sk
        .as_bytes()
        .try_into()
        .context("DilithiumSigningKey seed must be 32 bytes")?;
    Ok(DilithiumSigner::keypair_from_seed(&seed))
}

pub fn sign_transaction(identity: &Identity, body: TransactionBody) -> Result<SignedTransaction> {
    let digest = compute_tx_digest(&body).context("computing tx digest")?;
    let sig = DilithiumSigner::sign(
        &identity.sk,
        nexus_execution::types::TX_DOMAIN,
        digest.as_bytes(),
    );
    Ok(SignedTransaction {
        body,
        signature: sig,
        sender_pk: identity.pk.clone(),
        digest,
    })
}

/// Query the node for num_shards and compute target_shard for `sender`
/// using Jump Consistent Hash. Returns `ShardId` to set on the
/// `TransactionBody` **before** signing.
pub fn resolve_target_shard(
    rpc_url: &str,
    sender: &AccountAddress,
) -> Result<nexus_primitives::ShardId> {
    let url = format!("{}/v2/shards", rpc_url.trim_end_matches('/'));
    let resp = http_agent()
        .get(&url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let topo: ShardTopologyResponse = resp.into_json().context("parsing shard topology")?;
    Ok(nexus_intent::resolver::shard_lookup::jump_consistent_hash(
        sender,
        topo.num_shards,
    ))
}

#[derive(Deserialize)]
struct ShardTopologyResponse {
    num_shards: u16,
}

#[derive(Debug, Deserialize)]
pub struct TxSubmitResponse {
    #[serde(deserialize_with = "deserialize_digest_string")]
    pub tx_digest: String,
    pub accepted: bool,
}

pub fn submit_transaction(rpc_url: &str, tx: &SignedTransaction) -> Result<TxSubmitResponse> {
    let url = format!("{}/v2/tx/submit", rpc_url.trim_end_matches('/'));
    let body = serde_json::to_value(tx).context("serializing transaction to JSON")?;

    let response = http_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .with_context(|| format!("POST {url}"))?;

    let resp: TxSubmitResponse = response.into_json().context("parsing submit response")?;
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct TxStatusResponse {
    #[serde(rename = "tx_digest")]
    #[serde(deserialize_with = "deserialize_digest_string")]
    pub _tx_digest: String,
    pub status: serde_json::Value,
    pub gas_used: u64,
}

pub fn poll_tx_status(
    rpc_url: &str,
    tx_digest: &str,
    max_attempts: u32,
    interval_ms: u64,
) -> Result<Option<TxStatusResponse>> {
    let url = format!(
        "{}/v2/tx/{}/status",
        rpc_url.trim_end_matches('/'),
        tx_digest
    );

    let agent = http_agent();
    for _ in 0..max_attempts {
        match agent.get(&url).call() {
            Ok(resp) => {
                let status: TxStatusResponse =
                    resp.into_json().context("parsing tx status response")?;
                return Ok(Some(status));
            }
            Err(ureq::Error::Status(404, _)) => {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
            Err(e) => return Err(e).context("querying tx status"),
        }
    }

    Ok(None)
}

#[derive(serde::Serialize)]
pub struct ContractQueryRequest {
    pub contract: String,
    pub function: String,
    pub type_args: Vec<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContractQueryResponse {
    pub return_value: Option<String>,
    pub gas_used: u64,
    #[serde(default)]
    pub gas_budget: u64,
}

pub fn query_view_function(
    rpc_url: &str,
    request: &ContractQueryRequest,
) -> Result<ContractQueryResponse> {
    let url = format!("{}/v2/contract/query", rpc_url.trim_end_matches('/'));
    let body = serde_json::to_value(request).context("serializing query request")?;

    let response = http_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .with_context(|| format!("POST {url}"))?;

    let resp: ContractQueryResponse = response.into_json().context("parsing query response")?;
    Ok(resp)
}

#[derive(Debug, Deserialize)]
struct TokenBalance {
    #[allow(dead_code)]
    pub token: String,
    pub amount: u64,
}

#[derive(Debug, Deserialize)]
struct RawBalanceResponse {
    #[allow(dead_code)]
    pub address: serde_json::Value,
    pub balances: Vec<TokenBalance>,
}

#[derive(Debug)]
pub struct BalanceResponse {
    pub balance: u64,
}

pub fn query_balance(rpc_url: &str, address: &AccountAddress) -> Result<BalanceResponse> {
    let url = format!(
        "{}/v2/account/{}/balance",
        rpc_url.trim_end_matches('/'),
        address.to_hex(),
    );

    let response = http_agent()
        .get(&url)
        .call()
        .with_context(|| format!("GET {url}"))?;

    let raw: RawBalanceResponse = response.into_json().context("parsing balance response")?;
    let balance = raw.balances.first().map(|b| b.amount).unwrap_or(0);
    Ok(BalanceResponse { balance })
}

#[derive(Debug, Deserialize)]
pub struct FaucetResponse {
    #[serde(deserialize_with = "deserialize_digest_string")]
    pub tx_digest: String,
    pub amount: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DigestField {
    Hex(String),
    Bytes([u8; 32]),
    ByteVec(Vec<u8>),
}

fn deserialize_digest_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match DigestField::deserialize(deserializer)? {
        DigestField::Hex(hex) => Ok(hex),
        DigestField::Bytes(bytes) => Ok(hex::encode(bytes)),
        DigestField::ByteVec(bytes) => {
            if bytes.len() != 32 {
                return Err(D::Error::custom(format!(
                    "digest must be 32 bytes, got {}",
                    bytes.len()
                )));
            }
            Ok(hex::encode(bytes))
        }
    }
}

pub fn request_faucet(rpc_url: &str, address: &AccountAddress) -> Result<FaucetResponse> {
    let url = format!("{}/v2/faucet/mint", rpc_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "recipient": address.to_hex(),
    });

    let response = http_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
        .with_context(|| format!("POST {url}"))?;

    let resp: FaucetResponse = response.into_json().context("parsing faucet response")?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::{
        ephemeral_identity, sign_transaction, validate_rpc_url, TxStatusResponse, TxSubmitResponse,
    };
    use nexus_crypto::{DilithiumSigner, Signer};
    use nexus_execution::types::{TransactionBody, TransactionPayload};
    use nexus_primitives::{AccountAddress, Amount, EpochNumber, TokenId};

    #[test]
    fn submit_response_accepts_hex_digest() {
        let response: TxSubmitResponse = serde_json::from_str(
            r#"{"tx_digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","accepted":true}"#,
        )
        .expect("hex digest response should parse");

        assert_eq!(
            response.tx_digest,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(response.accepted);
    }

    #[test]
    fn submit_response_accepts_byte_array_digest() {
        let response: TxSubmitResponse = serde_json::from_str(
            r#"{"tx_digest":[1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239,1,35,69,103,137,171,205,239],"accepted":true}"#,
        )
        .expect("byte array digest response should parse");

        assert_eq!(
            response.tx_digest,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(response.accepted);
    }

    #[test]
    fn status_response_accepts_byte_array_digest() {
        let response: TxStatusResponse = serde_json::from_str(
            r#"{"tx_digest":[202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202,202],"status":"success","gas_used":42}"#,
        )
        .expect("byte array tx status should parse");

        assert_eq!(response._tx_digest, "ca".repeat(32));
        assert_eq!(response.status, serde_json::json!("success"));
        assert_eq!(response.gas_used, 42);
    }

    // ── validate_rpc_url ─────────────────────────────────────────────

    #[test]
    fn validate_rpc_url_accepts_http_scheme() {
        assert!(validate_rpc_url("http://localhost:8080").is_ok());
    }

    #[test]
    fn validate_rpc_url_accepts_https_scheme() {
        assert!(validate_rpc_url("https://node.example.com:8080/rpc").is_ok());
    }

    #[test]
    fn validate_rpc_url_rejects_bare_hostname() {
        let err = validate_rpc_url("localhost:8080").unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn validate_rpc_url_rejects_ws_scheme() {
        assert!(validate_rpc_url("ws://example.com").is_err());
    }

    #[test]
    fn validate_rpc_url_rejects_wss_scheme() {
        assert!(validate_rpc_url("wss://example.com").is_err());
    }

    // ── ephemeral_identity ───────────────────────────────────────────

    #[test]
    fn ephemeral_identity_derives_address_from_pubkey() {
        let id = ephemeral_identity();
        let expected = AccountAddress::from_dilithium_pubkey(id.pk.as_bytes());
        assert_eq!(id.address, expected);
    }

    #[test]
    fn ephemeral_identities_are_distinct() {
        let a = ephemeral_identity();
        let b = ephemeral_identity();
        assert_ne!(a.address, b.address);
    }

    // ── sign_transaction ─────────────────────────────────────────────

    fn sample_body(sender: AccountAddress) -> TransactionBody {
        TransactionBody {
            sender,
            sequence_number: 0,
            expiry_epoch: EpochNumber(9999),
            gas_limit: 10_000,
            gas_price: 1,
            target_shard: None,
            payload: TransactionPayload::Transfer {
                recipient: AccountAddress([0xBB; 32]),
                amount: Amount(100),
                token: TokenId::Native,
            },
            chain_id: 1,
        }
    }

    #[test]
    fn sign_transaction_produces_signed_tx_with_matching_body() {
        let id = ephemeral_identity();
        let body = sample_body(id.address);
        let signed = sign_transaction(&id, body.clone()).unwrap();
        assert_eq!(signed.body, body);
        assert_eq!(signed.sender_pk.as_bytes(), id.pk.as_bytes());
    }

    #[test]
    fn sign_transaction_digest_is_deterministic() {
        let id = ephemeral_identity();
        let body = sample_body(id.address);
        let s1 = sign_transaction(&id, body.clone()).unwrap();
        let s2 = sign_transaction(&id, body).unwrap();
        assert_eq!(s1.digest, s2.digest);
    }

    // ── deserialize_digest_string ByteVec wrong length ───────────────

    #[test]
    fn submit_response_rejects_short_byte_vec_digest() {
        let result: serde_json::Result<TxSubmitResponse> =
            serde_json::from_str(r#"{"tx_digest":[1,2,3,4],"accepted":true}"#);
        assert!(result.is_err(), "short ByteVec should fail deserialization");
    }

    #[test]
    fn submit_response_rejects_empty_byte_vec_digest() {
        let result: serde_json::Result<TxSubmitResponse> =
            serde_json::from_str(r#"{"tx_digest":[],"accepted":true}"#);
        assert!(result.is_err(), "empty ByteVec should fail deserialization");
    }

    // ── load_identity ────────────────────────────────────────────────

    #[test]
    fn load_identity_json_format() {
        use nexus_crypto::{DilithiumSigner, Signer};
        let dir = tempfile::tempdir().unwrap();
        let (sk, _pk) = DilithiumSigner::generate_keypair();
        let key_json = serde_json::json!({
            "algorithm": "Dilithium3",
            "key_type": "secret",
            "hex": hex::encode(sk.as_bytes()),
        });
        let path = dir.path().join("dilithium-secret.json");
        std::fs::write(&path, key_json.to_string()).unwrap();

        let id = super::load_identity(&path).unwrap();
        assert_ne!(id.address, AccountAddress([0u8; 32]));
    }

    #[test]
    fn load_identity_hex_format() {
        use nexus_crypto::{DilithiumSigner, Signer};
        let dir = tempfile::tempdir().unwrap();
        let (sk, _pk) = DilithiumSigner::generate_keypair();
        let path = dir.path().join("dilithium.sk");
        std::fs::write(&path, hex::encode(sk.as_bytes())).unwrap();

        let id = super::load_identity(&path).unwrap();
        assert_ne!(id.address, AccountAddress([0u8; 32]));
    }

    #[test]
    fn load_identity_missing_file_fails() {
        let result = super::load_identity(std::path::Path::new("/nonexistent/key.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_identity_bad_json_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let result = super::load_identity(&path);
        assert!(result.is_err());
    }

    #[test]
    fn load_identity_bad_hex_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.hex");
        std::fs::write(&path, "ZZZZ_not_hex").unwrap();
        let result = super::load_identity(&path);
        assert!(result.is_err());
    }

    // ── ContractQueryRequest serialization ───────────────────────────

    #[test]
    fn contract_query_request_serializes() {
        let req = super::ContractQueryRequest {
            contract: hex::encode([0u8; 32]),
            function: "mod::func".to_string(),
            type_args: vec![],
            args: vec!["01".to_string()],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["function"], "mod::func");
    }

    // ── http_agent ───────────────────────────────────────────────────

    #[test]
    fn http_agent_is_constructible() {
        let _agent = super::http_agent();
    }

    // ── derive_keypair_from_sk ──────────────────────────────────────

    #[test]
    fn derive_keypair_from_sk_round_trips() {
        let (sk, _pk) = DilithiumSigner::generate_keypair();
        let result = super::derive_keypair_from_sk(&sk);
        assert!(result.is_ok());
        let (derived_sk, derived_pk) = result.unwrap();
        // Deriving from the same seed should yield the same keypair.
        let (sk2, pk2) = super::derive_keypair_from_sk(&derived_sk).unwrap();
        assert_eq!(derived_pk.as_bytes(), pk2.as_bytes());
        let _ = sk2; // use binding
    }

    #[test]
    fn derive_keypair_address_matches_pubkey() {
        let (sk, _) = DilithiumSigner::generate_keypair();
        let (_, pk) = super::derive_keypair_from_sk(&sk).unwrap();
        let addr = AccountAddress::from_dilithium_pubkey(pk.as_bytes());
        // Just verify address is non-zero (derived from pubkey)
        assert_ne!(addr.0, [0u8; 32]);
    }
}
