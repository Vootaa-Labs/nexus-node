// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-network` — P2P network transport layer for Nexus.
//!
//! Provides authenticated, encrypted validator-to-validator communication.
//! Exposes stable [`NetworkTransport`], [`GossipNetwork`], and [`DhtDiscovery`]
//! traits consumed by the consensus and execution layers.
//!
//! # Modules
//! | Module | Purpose |
//! |--------|----------------------------------------------|
//! | [`error`] | Unified `NetworkError` type |
//! | [`types`] | `PeerId`, `ConnectionState`, `Topic`, wire constants |
//! | [`config`] | `NetworkConfig` with production defaults |
//! | [`traits`] | FROZEN-2 trait contracts |
//! | [`codec`] | Nexus wire format encoder/decoder (BCS) |
//!
//! # Quick import
//! ```no_run
//! use nexus_network::{
//!     // Traits
//!     NetworkTransport, GossipNetwork, DhtDiscovery,
//!     // Types
//!     PeerId, ConnectionState, Topic, MessageType,
//!     // Config
//!     NetworkConfig,
//!     // Error
//!     NetworkError,
//!     // Codec
//!     codec,
//! };
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codec;
pub mod config;
pub mod discovery;
pub mod error;
pub mod gossip;
pub mod metrics;
pub mod rate_limit;
pub mod service;
pub mod traits;
pub mod transport;
pub mod types;

// ── Convenience re-exports at crate root ─────────────────────────────────────

// Error
pub use error::{NetworkError, NetworkResult};

// Types
pub use types::{
    ConnectionState, MessageType, PeerId, PeerScore, RoutingHealth, Topic, PEER_SCORE_THRESHOLD,
    WIRE_HEADER_SIZE, WIRE_MAGIC, WIRE_VERSION,
};

// Config
pub use config::NetworkConfig;

// Transport
pub use transport::{TransportHandle, TransportManager};

// Gossip
pub use gossip::{GossipHandle, GossipService};

// Discovery
pub use discovery::{DiscoveryHandle, DiscoveryService, NodeRecord};

// Rate Limiting
pub use rate_limit::PeerRateLimiter;

// Service
pub use service::{NetworkService, NetworkServiceHandle};

// Traits
pub use traits::{DhtDiscovery, GossipNetwork, NetworkTransport};
