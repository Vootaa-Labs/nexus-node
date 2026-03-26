// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Internal error → MCP tool error mapping.
//!
//! Translates [`IntentError`] and [`RpcError`] into MCP-compatible
//! error codes that LLM clients can interpret programmatically.

use nexus_intent::IntentError;

use crate::error::RpcError;

// ── MCP error codes ─────────────────────────────────────────────────────

/// MCP-level error code returned in tool error responses.
///
/// LLM clients use these to decide retry strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorCode {
    /// Request was malformed (do not retry as-is).
    InvalidParams,
    /// Tool not found in registry.
    ToolNotFound,
    /// Agent capabilities insufficient.
    CapabilityDenied,
    /// Request expired before processing.
    Expired,
    /// Plan hash mismatch or missing confirmation.
    PlanBindingError,
    /// Value limit exceeded.
    ValueLimitExceeded,
    /// Internal server error (may retry).
    InternalError,
    /// Service temporarily unavailable (retry with backoff).
    Unavailable,
}

impl McpErrorCode {
    /// Numeric code for wire format.
    pub fn code(self) -> i32 {
        match self {
            Self::InvalidParams => -32602,
            Self::ToolNotFound => -32601,
            Self::CapabilityDenied => -32001,
            Self::Expired => -32002,
            Self::PlanBindingError => -32003,
            Self::ValueLimitExceeded => -32004,
            Self::InternalError => -32603,
            Self::Unavailable => -32000,
        }
    }

    /// Whether the client should retry this error.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::InternalError | Self::Unavailable)
    }
}

/// Map an [`IntentError`] to an MCP error code.
pub fn map_intent_error(err: &IntentError) -> McpErrorCode {
    match err {
        IntentError::InvalidSignature { .. }
        | IntentError::ParseError { .. }
        | IntentError::AgentSpecError { .. }
        | IntentError::StaleNonce { .. }
        | IntentError::IntentTooLarge { .. } => McpErrorCode::InvalidParams,

        IntentError::IntentExpired { .. } | IntentError::CompileTimeout { .. } => {
            McpErrorCode::Expired
        }

        IntentError::AgentCapabilityDenied { .. } => McpErrorCode::CapabilityDenied,

        IntentError::AgentValueLimitExceeded { .. } => McpErrorCode::ValueLimitExceeded,

        IntentError::AccountNotFound { .. }
        | IntentError::ContractNotFound { .. }
        | IntentError::InsufficientBalance { .. } => McpErrorCode::InvalidParams,

        IntentError::Codec(_)
        | IntentError::Internal(_)
        | IntentError::Storage(_)
        | IntentError::Execution(_)
        | IntentError::TooManySteps { .. }
        | IntentError::NoRoute { .. }
        | IntentError::GasBudgetExceeded { .. } => McpErrorCode::InternalError,
    }
}

/// Map an [`RpcError`] to an MCP error code.
pub fn map_rpc_error(err: &RpcError) -> McpErrorCode {
    match err {
        RpcError::NotFound(_) => McpErrorCode::ToolNotFound,
        RpcError::BadRequest(_) | RpcError::Serialization(_) => McpErrorCode::InvalidParams,
        RpcError::Unavailable(_) => McpErrorCode::Unavailable,
        RpcError::IntentError(_) | RpcError::ExecutionError(_) | RpcError::ConsensusError(_) => {
            McpErrorCode::InternalError
        }
        RpcError::Internal(_) => McpErrorCode::InternalError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_expired_maps_to_expired() {
        let err = IntentError::IntentExpired {
            deadline_ms: 100,
            current_ms: 200,
        };
        assert_eq!(map_intent_error(&err), McpErrorCode::Expired);
    }

    #[test]
    fn capability_denied_maps_correctly() {
        let err = IntentError::AgentCapabilityDenied {
            reason: "test".into(),
        };
        assert_eq!(map_intent_error(&err), McpErrorCode::CapabilityDenied);
    }

    #[test]
    fn value_limit_maps_correctly() {
        let err = IntentError::AgentValueLimitExceeded {
            value: 1000,
            limit: 500,
        };
        assert_eq!(map_intent_error(&err), McpErrorCode::ValueLimitExceeded);
    }

    #[test]
    fn rpc_bad_request_maps_to_invalid_params() {
        let err = RpcError::BadRequest("bad".into());
        assert_eq!(map_rpc_error(&err), McpErrorCode::InvalidParams);
    }

    #[test]
    fn rpc_unavailable_maps_correctly() {
        let err = RpcError::Unavailable("down".into());
        assert_eq!(map_rpc_error(&err), McpErrorCode::Unavailable);
    }

    #[test]
    fn retryable_codes() {
        assert!(McpErrorCode::InternalError.is_retryable());
        assert!(McpErrorCode::Unavailable.is_retryable());
        assert!(!McpErrorCode::InvalidParams.is_retryable());
        assert!(!McpErrorCode::CapabilityDenied.is_retryable());
    }

    #[test]
    fn error_codes_unique() {
        let codes = [
            McpErrorCode::InvalidParams,
            McpErrorCode::ToolNotFound,
            McpErrorCode::CapabilityDenied,
            McpErrorCode::Expired,
            McpErrorCode::PlanBindingError,
            McpErrorCode::ValueLimitExceeded,
            McpErrorCode::InternalError,
            McpErrorCode::Unavailable,
        ];
        let mut nums: Vec<i32> = codes.iter().map(|c| c.code()).collect();
        let original = nums.len();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(nums.len(), original, "duplicate error codes");
    }
}
