// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Nexus wire format encoder / decoder.
//!
//! Implements the **FROZEN-3** wire protocol:
//!
//! ```text
//! ┌─────────────┬──────┬──────┬──────────────────┐
//! │ MAGIC (4B)  │ VER  │ TYPE │ BCS PAYLOAD      │
//! │ "NEXU"      │ 0x01 │ enum │ variable-length  │
//! └─────────────┴──────┴──────┴──────────────────┘
//! ```
//!
//! All payloads are serialized with [BCS](https://docs.rs/bcs) for
//! deterministic, canonical encoding.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::NetworkError;
use crate::types::{MessageType, WIRE_HEADER_SIZE, WIRE_MAGIC, WIRE_VERSION};

/// Encode a typed message into the Nexus wire format.
///
/// Returns the complete frame: `MAGIC ‖ VERSION ‖ TYPE ‖ BCS(payload)`.
///
/// # Errors
/// Returns [`NetworkError::InvalidMessage`] if BCS serialization fails.
/// Returns [`NetworkError::MessageTooLarge`] if the payload exceeds the
/// type-specific size limit.
pub fn encode<T: Serialize>(msg_type: MessageType, payload: &T) -> Result<Vec<u8>, NetworkError> {
    let bcs_bytes = bcs::to_bytes(payload).map_err(|e| NetworkError::InvalidMessage {
        reason: format!("BCS serialization failed: {}", e),
    })?;

    let limit = msg_type.max_payload_size();
    if bcs_bytes.len() > limit {
        return Err(NetworkError::MessageTooLarge {
            size: bcs_bytes.len(),
            limit,
        });
    }

    let mut frame = Vec::with_capacity(WIRE_HEADER_SIZE + bcs_bytes.len());
    frame.extend_from_slice(&WIRE_MAGIC);
    frame.push(WIRE_VERSION);
    frame.push(msg_type as u8);
    frame.extend_from_slice(&bcs_bytes);
    Ok(frame)
}

/// Decode a Nexus wire frame, returning the message type and deserialized payload.
///
/// # Errors
/// - [`NetworkError::InvalidMessage`] if the magic, version, or type byte is wrong,
///   or if BCS deserialization fails.
/// - [`NetworkError::MessageTooLarge`] if the payload exceeds the type-specific limit.
pub fn decode<T: DeserializeOwned>(frame: &[u8]) -> Result<(MessageType, T), NetworkError> {
    if frame.len() < WIRE_HEADER_SIZE {
        return Err(NetworkError::InvalidMessage {
            reason: format!(
                "frame too short: {} bytes (minimum {})",
                frame.len(),
                WIRE_HEADER_SIZE
            ),
        });
    }

    // Validate magic
    if frame[0..4] != WIRE_MAGIC {
        return Err(NetworkError::InvalidMessage {
            reason: format!(
                "bad magic: expected {:02X?}, got {:02X?}",
                WIRE_MAGIC,
                &frame[0..4]
            ),
        });
    }

    // Validate version
    if frame[4] != WIRE_VERSION {
        return Err(NetworkError::InvalidMessage {
            reason: format!(
                "unsupported wire version: {} (expected {})",
                frame[4], WIRE_VERSION
            ),
        });
    }

    // Parse message type
    let msg_type =
        MessageType::from_byte(frame[5]).ok_or_else(|| NetworkError::InvalidMessage {
            reason: format!("unknown message type byte: 0x{:02X}", frame[5]),
        })?;

    let payload_bytes = &frame[WIRE_HEADER_SIZE..];

    // Check payload size limit
    let limit = msg_type.max_payload_size();
    if payload_bytes.len() > limit {
        return Err(NetworkError::MessageTooLarge {
            size: payload_bytes.len(),
            limit,
        });
    }

    let value = bcs::from_bytes(payload_bytes).map_err(|e| NetworkError::InvalidMessage {
        reason: format!("BCS deserialization failed: {}", e),
    })?;

    Ok((msg_type, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct TestPayload {
        data: Vec<u8>,
        seq: u64,
    }

    #[test]
    fn encode_decode_roundtrip() {
        let payload = TestPayload {
            data: vec![1, 2, 3, 4],
            seq: 42,
        };
        let frame = encode(MessageType::Transaction, &payload).unwrap();

        // Verify header
        assert_eq!(&frame[0..4], &WIRE_MAGIC);
        assert_eq!(frame[4], WIRE_VERSION);
        assert_eq!(frame[5], MessageType::Transaction as u8);

        let (mt, decoded): (MessageType, TestPayload) = decode(&frame).unwrap();
        assert_eq!(mt, MessageType::Transaction);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn all_message_types_roundtrip() {
        let payload = TestPayload {
            data: vec![0xAB],
            seq: 1,
        };
        for mt in [
            MessageType::Handshake,
            MessageType::Consensus,
            MessageType::Transaction,
            MessageType::Intent,
            MessageType::StateSync,
        ] {
            let frame = encode(mt, &payload).unwrap();
            let (decoded_mt, decoded_payload): (MessageType, TestPayload) = decode(&frame).unwrap();
            assert_eq!(decoded_mt, mt);
            assert_eq!(decoded_payload, payload);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut frame = encode(MessageType::Handshake, &42u64).unwrap();
        frame[0] = 0xFF; // corrupt magic
        let err = decode::<u64>(&frame).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidMessage { .. }));
    }

    #[test]
    fn rejects_bad_version() {
        let mut frame = encode(MessageType::Handshake, &42u64).unwrap();
        frame[4] = 0xFF; // unknown version
        let err = decode::<u64>(&frame).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidMessage { .. }));
    }

    #[test]
    fn rejects_unknown_message_type() {
        let mut frame = encode(MessageType::Handshake, &42u64).unwrap();
        frame[5] = 0xFF; // unknown type
        let err = decode::<u64>(&frame).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidMessage { .. }));
    }

    #[test]
    fn rejects_too_short_frame() {
        let err = decode::<u64>(&[0x4E, 0x45]).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidMessage { .. }));
    }

    #[test]
    fn rejects_oversized_payload() {
        // Handshake limit is 8 KB
        let big = TestPayload {
            data: vec![0u8; 9 * 1024],
            seq: 0,
        };
        let err = encode(MessageType::Handshake, &big).unwrap_err();
        assert!(matches!(err, NetworkError::MessageTooLarge { .. }));
    }

    #[test]
    fn empty_frame_rejected() {
        let err = decode::<u64>(&[]).unwrap_err();
        assert!(matches!(err, NetworkError::InvalidMessage { .. }));
    }

    #[test]
    fn wire_header_size_matches() {
        let frame = encode(MessageType::Handshake, &0u8).unwrap();
        // header = 6 bytes, payload = BCS(0u8) = 1 byte
        assert!(frame.len() >= WIRE_HEADER_SIZE);
    }
}
