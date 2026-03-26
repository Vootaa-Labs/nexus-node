// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! `nexus-rpc` — External API layer for Nexus.
//!
//! Provides REST, GraphQL, and WebSocket APIs for clients, explorers,
//! wallets, and AI agents to interact with the Nexus blockchain.
//!
//! # Architecture
//!
//! ```text
//! ┌─ REST (axum)   ← Intent submit, account query, tx status, health
//! ├─ GraphQL       ← Rich queries, subscriptions
//! ├─ WebSocket     ← Real-time event streaming
//! └─ Middleware     ← Rate limit, auth, CORS, tracing
//! ```
//!
//! All API handlers convert domain types to DTOs before serialization,
//! ensuring no private keys or internal state leaks to clients.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dto;
pub mod error;
pub mod intent_tracker;
pub mod mcp;
pub mod metrics;
pub mod middleware;
pub mod rest;
pub mod server;
pub mod tx_lifecycle;
pub mod ws;

pub use dto::*;
pub use error::{RpcError, RpcResult};
pub use intent_tracker::IntentTracker;
pub use middleware::{apply_middleware, RateLimiter};
pub use rest::{
    rest_router, AppState, ConsensusBackend, HtlcBackend, IntentBackend, NetworkBackend,
    QueryBackend, SessionProvenanceBackend, StateProofBackend, TransactionBroadcaster,
};
pub use server::{RpcService, RpcServiceBuilder};
pub use tx_lifecycle::TxLifecycleRegistry;
pub use ws::{event_channel, NodeEvent};
