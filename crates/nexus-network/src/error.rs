//! Unified error types for the `nexus-network` crate.
//!
//! Every network operation returns `Result<T, NetworkError>` — no panics,
//! no silent failures. Errors are classified as retryable or fatal.

/// Top-level error type for all network operations.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    /// Connection to a peer timed out.
    #[error("connection timeout to peer {peer_id}: {timeout_ms}ms")]
    ConnectionTimeout {
        /// Display-friendly peer identifier.
        peer_id: String,
        /// Timeout duration in milliseconds.
        timeout_ms: u64,
    },

    /// Peer sent a message that failed signature or authenticity check.
    #[error("authentication failed for peer {peer_id}: {reason}")]
    AuthenticationFailed {
        /// Display-friendly peer identifier.
        peer_id: String,
        /// Human-readable reason.
        reason: String,
    },

    /// Message decoding error (BCS deserialization, wire format magic, etc.).
    #[error("invalid message encoding: {reason}")]
    InvalidMessage {
        /// What was wrong with the message.
        reason: String,
    },

    /// Message exceeds the hard size limit for its type.
    #[error("message too large: {size} bytes (limit: {limit} bytes)")]
    MessageTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Maximum allowed size in bytes.
        limit: usize,
    },

    /// Peer is not connected or unreachable.
    #[error("peer unreachable: {peer_id}")]
    PeerUnreachable {
        /// Display-friendly peer identifier.
        peer_id: String,
    },

    /// Rate limit exceeded for a specific peer.
    #[error("rate limit exceeded for peer {peer_id}")]
    RateLimitExceeded {
        /// Display-friendly peer identifier.
        peer_id: String,
    },

    /// Peer is banned and cannot communicate until the ban expires.
    #[error("peer banned: {peer_id}")]
    PeerBanned {
        /// Display-friendly peer identifier.
        peer_id: String,
    },

    /// The topic is unknown or not subscribed.
    #[error("unknown or unsubscribed topic: {topic}")]
    UnknownTopic {
        /// Display-friendly topic name.
        topic: String,
    },

    /// DHT bootstrap or lookup failure.
    #[error("discovery error: {reason}")]
    DiscoveryError {
        /// What went wrong with DHT/discovery.
        reason: String,
    },

    /// The network service is shutting down.
    #[error("network service shutting down")]
    ShuttingDown,

    /// Generic I/O error from the underlying transport.
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Cryptographic operation failed (delegated from nexus-crypto).
    #[error("crypto error: {0}")]
    Crypto(#[from] nexus_crypto::NexusCryptoError),
}

impl NetworkError {
    /// Whether this error is transient and the caller should retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ConnectionTimeout { .. }
                | Self::PeerUnreachable { .. }
                | Self::RateLimitExceeded { .. }
                | Self::Io(_)
                | Self::DiscoveryError { .. }
        )
    }

    /// Whether this error indicates a permanent, non-recoverable situation.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::AuthenticationFailed { .. } | Self::PeerBanned { .. } | Self::ShuttingDown
        )
    }
}

/// Result alias for network operations.
pub type NetworkResult<T> = Result<T, NetworkError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_errors_classified_correctly() {
        let cases: Vec<(NetworkError, bool)> = vec![
            (
                NetworkError::ConnectionTimeout {
                    peer_id: "p".into(),
                    timeout_ms: 100,
                },
                true,
            ),
            (
                NetworkError::PeerUnreachable {
                    peer_id: "p".into(),
                },
                true,
            ),
            (
                NetworkError::RateLimitExceeded {
                    peer_id: "p".into(),
                },
                true,
            ),
            (NetworkError::DiscoveryError { reason: "r".into() }, true),
            (
                NetworkError::Io(std::io::Error::new(std::io::ErrorKind::Other, "e")),
                true,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.is_retryable(), expected, "{err}");
        }
    }

    #[test]
    fn fatal_errors_classified_correctly() {
        let cases: Vec<(NetworkError, bool)> = vec![
            (
                NetworkError::AuthenticationFailed {
                    peer_id: "p".into(),
                    reason: "r".into(),
                },
                true,
            ),
            (
                NetworkError::PeerBanned {
                    peer_id: "p".into(),
                },
                true,
            ),
            (NetworkError::ShuttingDown, true),
        ];
        for (err, expected) in cases {
            assert_eq!(err.is_fatal(), expected, "{err}");
        }
    }

    #[test]
    fn non_retryable_non_fatal_errors() {
        let err = NetworkError::InvalidMessage {
            reason: "bad".into(),
        };
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());

        let err = NetworkError::MessageTooLarge {
            size: 999,
            limit: 100,
        };
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());

        let err = NetworkError::UnknownTopic { topic: "t".into() };
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
    }
}
