// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! State proof REST endpoints.
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET  | `/v2/state/commitment` | Current commitment summary |
//! | POST | `/v2/state/proof`      | Single key inclusion/exclusion proof |
//! | POST | `/v2/state/proofs`     | Batch key proofs |

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};

use super::AppState;
use crate::dto::{
    BatchStateProofRequest, BatchStateProofResponse, MerkleNeighborProofDto, MerkleProofDto,
    SingleProofDto, StateCommitmentDto, StateProofRequest, StateProofResponse,
};
use crate::error::{RpcError, RpcResult};
use crate::metrics;

/// Maximum number of keys in a single batch proof request.
const MAX_BATCH_KEYS: usize = 100;

/// Build the state-proof router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/state/commitment", get(state_commitment))
        .route("/v2/state/proof", post(state_proof))
        .route("/v2/state/proofs", post(batch_state_proof))
}

/// `GET /v2/state/commitment`
///
/// Returns the current state commitment summary including primary and
/// backup tree roots, entry count, and epoch-check statistics.
async fn state_commitment(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<StateCommitmentDto>> {
    let start = Instant::now();
    let backend = state.state_proof.as_ref().ok_or_else(|| {
        metrics::proof_request_err("commitment");
        RpcError::Unavailable("state commitment not available".into())
    })?;

    let info = backend.commitment_info()?;
    metrics::proof_commitment_queried();
    metrics::proof_request_ok("commitment");
    metrics::proof_request_duration("commitment", start.elapsed().as_secs_f64());
    Ok(Json(info))
}

/// `POST /v2/state/proof`
///
/// Generate an inclusion or exclusion proof for a single storage key.
async fn state_proof(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StateProofRequest>,
) -> RpcResult<Json<StateProofResponse>> {
    let start = Instant::now();
    let backend = state.state_proof.as_ref().ok_or_else(|| {
        metrics::proof_request_err("proof");
        RpcError::Unavailable("state proofs not available".into())
    })?;

    let key_bytes =
        hex::decode(&req.key).map_err(|e| RpcError::BadRequest(format!("invalid hex key: {e}")))?;

    let root = backend.commitment_root()?;
    let (value, proof) = backend.prove_key(&key_bytes)?;

    metrics::proof_request_ok("proof");
    metrics::proof_request_duration("proof", start.elapsed().as_secs_f64());
    Ok(Json(StateProofResponse {
        commitment_root: hex::encode(root.0),
        value: value.map(hex::encode),
        proof_version: "blake3-merkle-v1".into(),
        encoding_format: "bcs-hex".into(),
        proof: merkle_proof_to_dto(&proof),
    }))
}

/// `POST /v2/state/proofs`
///
/// Generate inclusion/exclusion proofs for multiple storage keys.
async fn batch_state_proof(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchStateProofRequest>,
) -> RpcResult<Json<BatchStateProofResponse>> {
    let start = Instant::now();
    let backend = state.state_proof.as_ref().ok_or_else(|| {
        metrics::proof_request_err("proofs");
        RpcError::Unavailable("state proofs not available".into())
    })?;

    if req.keys.is_empty() {
        return Err(RpcError::BadRequest("keys array must not be empty".into()));
    }
    if req.keys.len() > MAX_BATCH_KEYS {
        return Err(RpcError::BadRequest(format!(
            "at most {MAX_BATCH_KEYS} keys per request"
        )));
    }

    metrics::proof_batch_size(req.keys.len());

    let key_bytes: Vec<Vec<u8>> = req
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            hex::decode(k)
                .map_err(|e| RpcError::BadRequest(format!("invalid hex key at index {i}: {e}")))
        })
        .collect::<Result<_, _>>()?;

    let root = backend.commitment_root()?;
    let results = backend.prove_keys(&key_bytes)?;

    let proofs: Vec<SingleProofDto> = req
        .keys
        .iter()
        .zip(results.iter())
        .map(|(hex_key, (value, proof))| SingleProofDto {
            key: hex_key.clone(),
            value: value.as_ref().map(hex::encode),
            proof: merkle_proof_to_dto(proof),
        })
        .collect();

    metrics::proof_request_ok("proofs");
    metrics::proof_request_duration("proofs", start.elapsed().as_secs_f64());

    Ok(Json(BatchStateProofResponse {
        commitment_root: hex::encode(root.0),
        proof_version: "blake3-merkle-v1".into(),
        encoding_format: "bcs-hex".into(),
        proofs,
    }))
}

/// Convert an internal `MerkleProof` to WireDTO.
pub(crate) fn merkle_proof_to_dto(proof: &nexus_storage::MerkleProof) -> MerkleProofDto {
    match proof {
        nexus_storage::MerkleProof::Inclusion {
            leaf_index,
            leaf_count,
            siblings,
        } => MerkleProofDto {
            proof_type: "inclusion".into(),
            leaf_count: *leaf_count,
            leaf_index: Some(*leaf_index),
            siblings: siblings.iter().map(|s| hex::encode(s.0)).collect(),
            left_neighbor: None,
            right_neighbor: None,
        },
        nexus_storage::MerkleProof::Exclusion {
            leaf_count,
            left_neighbor,
            right_neighbor,
        } => MerkleProofDto {
            proof_type: "exclusion".into(),
            leaf_count: *leaf_count,
            leaf_index: None,
            siblings: vec![],
            left_neighbor: left_neighbor.as_ref().map(neighbor_proof_to_dto),
            right_neighbor: right_neighbor.as_ref().map(neighbor_proof_to_dto),
        },
    }
}

fn neighbor_proof_to_dto(
    proof: &nexus_storage::commitment::MerkleNeighborProof,
) -> MerkleNeighborProofDto {
    MerkleNeighborProofDto {
        key: hex::encode(&proof.key),
        value: hex::encode(&proof.value),
        leaf_index: proof.leaf_index,
        siblings: proof.siblings.iter().map(|s| hex::encode(s.0)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RpcError;
    use crate::rest::test_helpers::{mock_state, MockQueryBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_primitives::Blake3Digest;
    use nexus_storage::StateCommitment;
    use std::sync::Arc;
    use tower::ServiceExt;

    struct MockStateProofBackend {
        tree: nexus_storage::Blake3SmtCommitment,
        tamper_exclusion: bool,
    }

    impl MockStateProofBackend {
        fn new(entries: &[(&[u8], &[u8])]) -> Self {
            let mut tree = nexus_storage::Blake3SmtCommitment::new();
            tree.update(entries);
            Self {
                tree,
                tamper_exclusion: false,
            }
        }

        fn tampered(entries: &[(&[u8], &[u8])]) -> Self {
            let mut backend = Self::new(entries);
            backend.tamper_exclusion = true;
            backend
        }

        fn rpc_error(err: nexus_storage::StorageError) -> RpcError {
            RpcError::Internal(err.to_string())
        }
    }

    impl crate::rest::StateProofBackend for MockStateProofBackend {
        fn commitment_info(&self) -> RpcResult<StateCommitmentDto> {
            let root = self.tree.root_commitment();
            Ok(StateCommitmentDto {
                commitment_root: hex::encode(root.0),
                backup_root: hex::encode(root.0),
                entry_count: self.tree.len() as u64,
                updates_applied: self.tree.len() as u64,
                epoch_checks_passed: 0,
            })
        }

        fn prove_key(
            &self,
            key: &[u8],
        ) -> RpcResult<(Option<Vec<u8>>, nexus_storage::MerkleProof)> {
            let (value, mut proof) = self.tree.prove_key(key).map_err(Self::rpc_error)?;
            if self.tamper_exclusion {
                if let nexus_storage::MerkleProof::Exclusion {
                    leaf_count,
                    left_neighbor,
                    right_neighbor,
                } = proof
                {
                    let right_neighbor = right_neighbor.map(|mut neighbor| {
                        if !neighbor.siblings.is_empty() {
                            neighbor.siblings[0] = Blake3Digest::ZERO;
                        }
                        neighbor
                    });
                    proof = nexus_storage::MerkleProof::Exclusion {
                        leaf_count,
                        left_neighbor,
                        right_neighbor,
                    };
                }
            }
            Ok((value, proof))
        }

        fn prove_keys(
            &self,
            keys: &[Vec<u8>],
        ) -> RpcResult<Vec<(Option<Vec<u8>>, nexus_storage::MerkleProof)>> {
            keys.iter().map(|key| self.prove_key(key)).collect()
        }

        fn commitment_root(&self) -> RpcResult<Blake3Digest> {
            Ok(self.tree.root_commitment())
        }
    }

    fn mock_state_with_state_proof(backend: MockStateProofBackend) -> Arc<AppState> {
        Arc::new(AppState {
            query: Arc::new(MockQueryBackend::new()),
            intent: None,
            consensus: None,
            network: None,
            broadcaster: None,
            events: None,
            rate_limiter: None,
            faucet_addr_limiter: None,
            metrics_handle: None,
            faucet_enabled: false,
            faucet_amount: 0,
            max_ws_connections: 100,
            ws_connection_count: std::sync::atomic::AtomicUsize::new(0),
            intent_tracker: None,
            session_provenance: None,
            state_proof: Some(Arc::new(backend)),
            mcp_dispatcher: None,
            mcp_call_index: std::sync::atomic::AtomicU64::new(0),
            quota_manager: None,
            query_gas_budget: 10_000_000,
            query_timeout_ms: 5_000,
            num_shards: 1,
            tx_lifecycle: None,
            htlc: None,
            block: None,
            event_backend: None,
        })
    }

    async fn post_single_proof(app: Router, key_hex: &str) -> axum::response::Response {
        let req = Request::builder()
            .method("POST")
            .uri("/v2/state/proof")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"key":"{key_hex}"}}"#)))
            .unwrap();

        app.oneshot(req).await.unwrap()
    }

    async fn post_batch_proofs(app: Router, keys: &[String]) -> axum::response::Response {
        let req = Request::builder()
            .method("POST")
            .uri("/v2/state/proofs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&BatchStateProofRequest {
                    keys: keys.to_vec(),
                })
                .unwrap(),
            ))
            .unwrap();

        app.oneshot(req).await.unwrap()
    }

    async fn read_json<T: serde::de::DeserializeOwned>(resp: axum::response::Response) -> T {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn decode_digest(hex_value: &str) -> Blake3Digest {
        let bytes = hex::decode(hex_value).unwrap();
        let raw: [u8; 32] = bytes.try_into().unwrap();
        Blake3Digest::from_bytes(raw)
    }

    fn dto_to_proof(dto: MerkleProofDto) -> nexus_storage::MerkleProof {
        match dto.proof_type.as_str() {
            "inclusion" => nexus_storage::MerkleProof::Inclusion {
                leaf_index: dto
                    .leaf_index
                    .expect("inclusion proof must contain leaf index"),
                leaf_count: dto.leaf_count,
                siblings: dto.siblings.iter().map(|s| decode_digest(s)).collect(),
            },
            "exclusion" => nexus_storage::MerkleProof::Exclusion {
                leaf_count: dto.leaf_count,
                left_neighbor: dto.left_neighbor.map(dto_to_neighbor_proof),
                right_neighbor: dto.right_neighbor.map(dto_to_neighbor_proof),
            },
            other => panic!("unexpected proof type: {other}"),
        }
    }

    fn dto_to_neighbor_proof(
        dto: MerkleNeighborProofDto,
    ) -> nexus_storage::commitment::MerkleNeighborProof {
        nexus_storage::commitment::MerkleNeighborProof {
            key: hex::decode(dto.key).unwrap(),
            value: hex::decode(dto.value).unwrap(),
            leaf_index: dto.leaf_index,
            siblings: dto.siblings.iter().map(|s| decode_digest(s)).collect(),
        }
    }

    #[tokio::test]
    async fn commitment_returns_503_when_not_available() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .uri("/v2/state/commitment")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn proof_returns_503_when_not_available() {
        let app = router().with_state(mock_state());
        let req = Request::builder()
            .method("POST")
            .uri("/v2/state/proof")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":"aa"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn commitment_returns_summary_when_available() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"aa", b"11"),
            (b"bb", b"22"),
        ])));
        let req = Request::builder()
            .uri("/v2/state/commitment")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let response: StateCommitmentDto = read_json(resp).await;
        assert_eq!(response.entry_count, 2);
        assert_eq!(response.updates_applied, 2);
        assert!(!response.commitment_root.is_empty());
        assert_eq!(response.commitment_root, response.backup_root);
    }

    #[tokio::test]
    async fn proof_returns_inclusion_dto_for_existing_key() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"a", b"1"),
            (b"b", b"2"),
            (b"c", b"3"),
        ])));

        let resp = post_single_proof(app, &hex::encode(b"b")).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let response: StateProofResponse = read_json(resp).await;
        assert_eq!(response.value, Some(hex::encode(b"2")));
        assert_eq!(response.proof.proof_type, "inclusion");
        assert_eq!(response.proof.leaf_count, 3);
        assert_eq!(response.proof.leaf_index, Some(1));
        assert!(response.proof.left_neighbor.is_none());
        assert!(response.proof.right_neighbor.is_none());

        let proof = dto_to_proof(response.proof);
        let root = decode_digest(&response.commitment_root);
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"b", Some(b"2"), &proof).unwrap();
    }

    #[tokio::test]
    async fn proof_returns_exclusion_dto_for_missing_key() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"a", b"1"),
            (b"c", b"3"),
        ])));

        let resp = post_single_proof(app, &hex::encode(b"b")).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let response: StateProofResponse = read_json(resp).await;
        assert!(response.value.is_none());
        assert_eq!(response.proof.proof_type, "exclusion");
        assert_eq!(response.proof.leaf_count, 2);
        assert!(response.proof.leaf_index.is_none());
        assert!(response.proof.siblings.is_empty());
        assert!(response.proof.left_neighbor.is_some());
        assert!(response.proof.right_neighbor.is_some());

        let proof = dto_to_proof(response.proof);
        let root = decode_digest(&response.commitment_root);
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"b", None, &proof).unwrap();
    }

    #[tokio::test]
    async fn proof_returns_empty_tree_exclusion_dto() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[])));

        let resp = post_single_proof(app, &hex::encode(b"missing")).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let response: StateProofResponse = read_json(resp).await;
        assert!(response.value.is_none());
        assert_eq!(response.proof.proof_type, "exclusion");
        assert_eq!(response.proof.leaf_count, 0);
        assert!(response.proof.left_neighbor.is_none());
        assert!(response.proof.right_neighbor.is_none());

        let proof = dto_to_proof(response.proof);
        let root = decode_digest(&response.commitment_root);
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"missing", None, &proof).unwrap();
    }

    #[tokio::test]
    async fn proof_tampering_is_detectable_by_client_verification() {
        let app = router().with_state(mock_state_with_state_proof(
            MockStateProofBackend::tampered(&[(b"a", b"1"), (b"c", b"3"), (b"e", b"5")]),
        ));

        let resp = post_single_proof(app, &hex::encode(b"d")).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let response: StateProofResponse = read_json(resp).await;
        assert_eq!(response.proof.proof_type, "exclusion");

        let proof = dto_to_proof(response.proof);
        let root = decode_digest(&response.commitment_root);
        let result = nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"d", None, &proof);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn batch_proofs_return_mixed_dtos_and_verify() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"a", b"1"),
            (b"c", b"3"),
            (b"e", b"5"),
        ])));

        let keys = vec![
            hex::encode(b"a"),
            hex::encode(b"b"),
            hex::encode(b"e"),
            hex::encode(b"z"),
        ];
        let resp = post_batch_proofs(app, &keys).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let response: BatchStateProofResponse = read_json(resp).await;
        assert_eq!(response.proofs.len(), 4);
        let root = decode_digest(&response.commitment_root);

        assert_eq!(response.proofs[0].key, keys[0]);
        assert_eq!(response.proofs[0].value, Some(hex::encode(b"1")));
        assert_eq!(response.proofs[0].proof.proof_type, "inclusion");
        let proof0 = dto_to_proof(response.proofs[0].proof.clone());
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"a", Some(b"1"), &proof0).unwrap();

        assert_eq!(response.proofs[1].key, keys[1]);
        assert!(response.proofs[1].value.is_none());
        assert_eq!(response.proofs[1].proof.proof_type, "exclusion");
        let proof1 = dto_to_proof(response.proofs[1].proof.clone());
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"b", None, &proof1).unwrap();

        assert_eq!(response.proofs[2].key, keys[2]);
        assert_eq!(response.proofs[2].value, Some(hex::encode(b"5")));
        assert_eq!(response.proofs[2].proof.proof_type, "inclusion");
        let proof2 = dto_to_proof(response.proofs[2].proof.clone());
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"e", Some(b"5"), &proof2).unwrap();

        assert_eq!(response.proofs[3].key, keys[3]);
        assert!(response.proofs[3].value.is_none());
        assert_eq!(response.proofs[3].proof.proof_type, "exclusion");
        let proof3 = dto_to_proof(response.proofs[3].proof.clone());
        nexus_storage::Blake3SmtCommitment::verify_proof(&root, b"z", None, &proof3).unwrap();
    }

    #[tokio::test]
    async fn batch_proofs_reject_empty_key_list() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[])));

        let resp = post_batch_proofs(app, &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn batch_proofs_reject_invalid_hex_key() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"a", b"1"),
        ])));

        let resp = post_batch_proofs(app, &["not-hex".to_string()]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn batch_proofs_reject_oversized_request() {
        let app = router().with_state(mock_state_with_state_proof(MockStateProofBackend::new(&[
            (b"a", b"1"),
        ])));

        let keys: Vec<String> = (0..101)
            .map(|index| hex::encode(format!("key-{index}").as_bytes()))
            .collect();
        let resp = post_batch_proofs(app, &keys).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
