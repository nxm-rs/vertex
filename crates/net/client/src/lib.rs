//! Swarm client behaviour for the Vertex node.
//!
//! This crate provides the `SwarmClientBehaviour` which handles all client-side
//! protocols on the Swarm network:
//!
//! - **Pricing**: Payment threshold exchange
//! - **Retrieval**: Chunk request/response
//! - **PushSync**: Chunk push with receipt
//! - **Settlement**: SWAP cheques and pseudosettle
//!
//! # Architecture
//!
//! The client behaviour is **pure protocol plumbing**. It handles:
//! - Protocol negotiation and stream management
//! - Message encoding/decoding
//! - Per-peer connection state via handler
//!
//! Business logic (peer selection, threshold validation, settlement decisions)
//! lives in the trait implementations (client/core, bandwidth crates).
//!
//! # Handler Lifecycle
//!
//! The `SwarmClientHandler` is created in dormant state when a connection is
//! established. After the handshake completes (signaled by `TopologyEvent::PeerAuthenticated`),
//! the node sends an `ActivatePeer` command which transitions the handler to
//! active state with:
//! - Peer's overlay address
//! - Full node status
//! - PeerAvailability handle for bandwidth accounting
//!
//! # Event/Command Interface
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Business Logic Layer                      │
//! │        (client/core implements SwarmReader/Writer)          │
//! │        (bandwidth implements AvailabilityAccounting)        │
//! └─────────────────────────────────────────────────────────────┘
//!                              ▲ events    │ commands
//!                              │           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                  SwarmClientBehaviour                        │
//! │                  (protocol plumbing)                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

mod behaviour;
mod events;
mod handler;
pub mod protocol;

pub use behaviour::{Config as BehaviourConfig, SwarmClientBehaviour};
pub use events::{Cheque, ClientCommand, ClientEvent};
pub use handler::{Config as HandlerConfig, HandlerCommand, HandlerEvent, SwarmClientHandler};
pub use protocol::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundRequest, ClientOutboundUpgrade, ClientUpgradeError,
};
