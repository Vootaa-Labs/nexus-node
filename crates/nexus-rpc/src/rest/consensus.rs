// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Consensus, validator, and epoch REST endpoints.
//!
//! `GET  /v2/validators`           — list active validators.
//! `GET  /v2/validators/:index`    — single validator info.
//! `GET  /v2/consensus/status`     — consensus engine status.
//! `GET  /v2/consensus/epoch`      — current epoch information.
//! `GET  /v2/admin/epoch/history`  — epoch transition history.
//! `POST /v2/admin/epoch/advance`  — manual epoch advance.
//! `POST /v2/admin/validator/slash`— slash a validator.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::AppState;
use crate::dto::{
    ConsensusStatusDto, ElectionResultDto, EpochAdvanceRequest, EpochAdvanceResponse,
    EpochHistoryResponse, EpochInfoDto, RotationPolicyDto, SlashValidatorRequest,
    SlashValidatorResponse, StakingValidatorsResponse, ValidatorInfoDto,
};
use crate::error::{RpcError, RpcResult};
use nexus_primitives::ValidatorIndex;

/// Build the consensus / validator / epoch router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v2/validators", get(list_validators))
        .route("/v2/validators/:index", get(get_validator))
        .route("/v2/consensus/status", get(consensus_status))
        .route("/v2/consensus/epoch", get(epoch_info))
        .route("/v2/consensus/election/latest", get(election_result))
        .route("/v2/consensus/rotation-policy", get(rotation_policy))
        .route("/v2/staking/validators", get(staking_validators))
        .route("/v2/admin/epoch/history", get(epoch_history))
        .route("/v2/admin/epoch/advance", post(advance_epoch))
        .route("/v2/admin/validator/slash", post(slash_validator))
}

/// `GET /v2/validators`
///
/// Returns all active (non-slashed) validators.
async fn list_validators(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<ValidatorInfoDto>>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let validators = backend.active_validators()?;
    Ok(Json(validators))
}

/// `GET /v2/validators/:index`
///
/// Returns a single validator by committee index.
async fn get_validator(
    State(state): State<Arc<AppState>>,
    Path(index): Path<u32>,
) -> RpcResult<Json<ValidatorInfoDto>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let validator = backend.validator_info(ValidatorIndex(index))?;
    Ok(Json(validator))
}

/// `GET /v2/consensus/status`
///
/// Returns current consensus engine status (epoch, DAG size, commits).
async fn consensus_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<ConsensusStatusDto>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let status = backend.consensus_status()?;
    Ok(Json(status))
}

/// `GET /v2/consensus/epoch`
///
/// Returns current epoch information including committee size, epoch
/// start time, and configured epoch parameters.
async fn epoch_info(State(state): State<Arc<AppState>>) -> RpcResult<Json<EpochInfoDto>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let info = backend.epoch_info()?;
    Ok(Json(info))
}

/// `GET /v2/admin/epoch/history`
///
/// Returns the complete epoch transition history for audit purposes.
async fn epoch_history(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<EpochHistoryResponse>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let history = backend.epoch_history()?;
    Ok(Json(history))
}

/// `POST /v2/admin/epoch/advance`
///
/// Manually advances the epoch (governance / operator action).
async fn advance_epoch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EpochAdvanceRequest>,
) -> RpcResult<Json<EpochAdvanceResponse>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let resp = backend.advance_epoch(&req.reason)?;
    Ok(Json(resp))
}

/// `POST /v2/admin/validator/slash`
///
/// Slashes a validator by committee index (governance action).
async fn slash_validator(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SlashValidatorRequest>,
) -> RpcResult<Json<SlashValidatorResponse>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let resp = backend.slash_validator(ValidatorIndex(req.validator_index), &req.reason)?;
    Ok(Json(resp))
}

/// `GET /v2/consensus/election/latest`
///
/// Returns the most recent election result (if a committee election has
/// occurred since genesis).
async fn election_result(State(state): State<Arc<AppState>>) -> RpcResult<Json<ElectionResultDto>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let result = backend.election_result()?;
    Ok(Json(result))
}

/// `GET /v2/consensus/rotation-policy`
///
/// Returns the current committee rotation policy configuration.
async fn rotation_policy(State(state): State<Arc<AppState>>) -> RpcResult<Json<RotationPolicyDto>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let policy = backend.rotation_policy()?;
    Ok(Json(policy))
}

/// `GET /v2/staking/validators`
///
/// Returns the current staking snapshot: all validator staking records
/// with effective stake, status, and reputation.
async fn staking_validators(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<StakingValidatorsResponse>> {
    let backend = state
        .consensus
        .as_ref()
        .ok_or_else(|| RpcError::Unavailable("consensus service not available".into()))?;
    let validators = backend.staking_validators()?;
    Ok(Json(validators))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{EpochHistoryResponse, EpochTransitionDto};
    use crate::rest::test_helpers::{mock_state_with_consensus, MockConsensusBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_primitives::*;
    use tower::ServiceExt;

    fn sample_validator(index: u32) -> ValidatorInfoDto {
        ValidatorInfoDto {
            index: ValidatorIndex(index),
            public_key_hex: hex::encode([0xAA; 16]),
            stake: Amount(1_000_000),
            reputation: 9500,
            is_slashed: false,
            shard_id: Some(ShardId(0)),
        }
    }

    #[tokio::test]
    async fn list_validators_returns_200() {
        let validators = vec![sample_validator(0), sample_validator(1)];
        let backend = MockConsensusBackend::new().with_validators(validators.clone());
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/validators")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: Vec<ValidatorInfoDto> = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].index, ValidatorIndex(0));
    }

    #[tokio::test]
    async fn get_validator_returns_200() {
        let backend = MockConsensusBackend::new().with_validators(vec![sample_validator(0)]);
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/validators/0")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: ValidatorInfoDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.index, ValidatorIndex(0));
        assert_eq!(result.reputation, 9500);
    }

    #[tokio::test]
    async fn get_validator_returns_404_for_unknown() {
        let backend = MockConsensusBackend::new();
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/validators/99")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn consensus_status_returns_200() {
        let status = ConsensusStatusDto {
            epoch: EpochNumber(5),
            dag_size: 128,
            total_commits: 1000,
            pending_commits: 3,
        };
        let backend = MockConsensusBackend::new().with_status(status.clone());
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/consensus/status")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: ConsensusStatusDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.epoch, EpochNumber(5));
        assert_eq!(result.total_commits, 1000);
    }

    #[tokio::test]
    async fn list_validators_returns_503_when_no_backend() {
        let state = Arc::new(AppState {
            query: Arc::new(crate::rest::test_helpers::MockQueryBackend::new()),
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
            state_proof: None,
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
        });
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/validators")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn epoch_info_returns_200() {
        let backend = MockConsensusBackend::new().with_epoch_info(EpochInfoDto {
            epoch: EpochNumber(3),
            epoch_started_at: TimestampMs(1_700_000_000_000),
            committee_size: 7,
            epoch_commits: 500,
            epoch_length_commits: 10_000,
            epoch_length_seconds: 86_400,
        });
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/consensus/epoch")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: EpochInfoDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.epoch, EpochNumber(3));
        assert_eq!(result.committee_size, 7);
    }

    #[tokio::test]
    async fn epoch_info_returns_503_when_not_configured() {
        // Default mock has no epoch_info configured → falls back to trait default → 503
        let backend = MockConsensusBackend::new();
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/consensus/epoch")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn epoch_history_returns_200() {
        let backend = MockConsensusBackend::new().with_epoch_history(EpochHistoryResponse {
            transitions: vec![EpochTransitionDto {
                from_epoch: EpochNumber(0),
                to_epoch: EpochNumber(1),
                trigger: "CommitThreshold".into(),
                final_commit_count: 10_000,
                transitioned_at: TimestampMs(1_700_000_000_000),
            }],
            total: 1,
        });
        let state = mock_state_with_consensus(backend);
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/admin/epoch/history")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: EpochHistoryResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.transitions[0].to_epoch, EpochNumber(1));
    }

    #[tokio::test]
    async fn slash_validator_returns_503_when_no_backend() {
        let state = Arc::new(AppState {
            query: Arc::new(crate::rest::test_helpers::MockQueryBackend::new()),
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
            state_proof: None,
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
        });
        let app = router().with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/v2/admin/validator/slash")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"validator_index":0,"reason":"test"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
