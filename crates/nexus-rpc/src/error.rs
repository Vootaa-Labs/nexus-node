//! RPC error types.
//!
//! Provides a unified error type for the RPC layer that maps domain errors
//! from consensus, execution, and intent crates into HTTP-appropriate responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Unified error type for the RPC layer.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// A resource was not found (404).
    #[error("not found: {0}")]
    NotFound(String),

    /// The request was malformed or invalid (400).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Internal server error (500).
    #[error("internal error: {0}")]
    Internal(String),

    /// The service is temporarily unavailable (503).
    #[error("service unavailable: {0}")]
    Unavailable(String),

    /// Intent compilation or processing failure (422).
    #[error("intent error: {0}")]
    IntentError(String),

    /// Execution-layer error (422).
    #[error("execution error: {0}")]
    ExecutionError(String),

    /// Consensus-layer error (422).
    #[error("consensus error: {0}")]
    ConsensusError(String),

    /// JSON serialization/deserialization failure (400).
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Convenience alias.
pub type RpcResult<T> = Result<T, RpcError>;

/// JSON error body returned to clients.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Machine-readable error code.
    pub error: &'static str,
    /// Human-readable error message.
    pub message: String,
}

impl RpcError {
    /// HTTP status code for this error.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) | Self::Serialization(_) => StatusCode::BAD_REQUEST,
            Self::IntentError(_) | Self::ExecutionError(_) | Self::ConsensusError(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Machine-readable error code string.
    fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NOT_FOUND",
            Self::BadRequest(_) => "BAD_REQUEST",
            Self::Internal(_) => "INTERNAL_ERROR",
            Self::Unavailable(_) => "SERVICE_UNAVAILABLE",
            Self::IntentError(_) => "INTENT_ERROR",
            Self::ExecutionError(_) => "EXECUTION_ERROR",
            Self::ConsensusError(_) => "CONSENSUS_ERROR",
            Self::Serialization(_) => "SERIALIZATION_ERROR",
        }
    }
}

impl IntoResponse for RpcError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorResponse {
            error: self.error_code(),
            message: self.to_string(),
        };
        let json =
            serde_json::to_string(&body).unwrap_or_else(|_| r#"{"error":"INTERNAL_ERROR"}"#.into());
        (status, [("content-type", "application/json")], json).into_response()
    }
}

// ── Domain error conversions ────────────────────────────────────────────

impl From<nexus_intent::IntentError> for RpcError {
    fn from(err: nexus_intent::IntentError) -> Self {
        use nexus_intent::IntentError;
        match &err {
            IntentError::AccountNotFound { .. } | IntentError::ContractNotFound { .. } => {
                Self::NotFound(err.to_string())
            }
            IntentError::InvalidSignature { .. }
            | IntentError::IntentExpired { .. }
            | IntentError::StaleNonce { .. }
            | IntentError::IntentTooLarge { .. }
            | IntentError::ParseError { .. }
            | IntentError::AgentSpecError { .. } => Self::BadRequest(err.to_string()),
            IntentError::InsufficientBalance { .. } => Self::IntentError(err.to_string()),
            _ => Self::IntentError(err.to_string()),
        }
    }
}

impl From<nexus_execution::ExecutionError> for RpcError {
    fn from(err: nexus_execution::ExecutionError) -> Self {
        use nexus_execution::ExecutionError;
        match &err {
            ExecutionError::InvalidSignature { .. }
            | ExecutionError::SequenceNumberMismatch { .. }
            | ExecutionError::TransactionExpired { .. }
            | ExecutionError::PayloadTooLarge { .. }
            | ExecutionError::GasLimitTooLow { .. }
            | ExecutionError::ChainIdMismatch { .. } => Self::BadRequest(err.to_string()),
            _ => Self::ExecutionError(err.to_string()),
        }
    }
}

impl From<nexus_consensus::ConsensusError> for RpcError {
    fn from(err: nexus_consensus::ConsensusError) -> Self {
        Self::ConsensusError(err.to_string())
    }
}

impl From<serde_json::Error> for RpcError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_maps_to_404() {
        let err = RpcError::NotFound("tx abc".into());
        assert_eq!(err.status_code(), StatusCode::NOT_FOUND);
        assert_eq!(err.error_code(), "NOT_FOUND");
    }

    #[test]
    fn bad_request_maps_to_400() {
        let err = RpcError::BadRequest("invalid param".into());
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn intent_error_maps_to_422() {
        let err = RpcError::IntentError("insufficient balance".into());
        assert_eq!(err.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn internal_error_maps_to_500() {
        let err = RpcError::Internal("panic".into());
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn error_response_serializes_to_json() {
        let resp = ErrorResponse {
            error: "NOT_FOUND",
            message: "tx abc not found".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("NOT_FOUND"));
        assert!(json.contains("tx abc not found"));
    }

    #[test]
    fn into_response_produces_json_content_type() {
        let err = RpcError::NotFound("test".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn serde_json_error_converts() {
        let raw_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let rpc_err: RpcError = raw_err.into();
        assert_eq!(rpc_err.status_code(), StatusCode::BAD_REQUEST);
    }
}
