//! Swarm client behaviour for the Vertex node.
//!
//! This module provides the `ClientBehaviour` which handles all client-side
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
//! The `ClientHandler` is created in dormant state when a connection is
//! established. After the handshake completes (signaled by `TopologyEvent::PeerAuthenticated`),
//! the node sends an `ActivatePeer` command which transitions the handler to
//! active state with:
//! - Peer's overlay address
//! - Storer node status (is_full_node flag)
//! - PeerBandwidth handle for bandwidth accounting
//!
//! # Event/Command Interface
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Business Logic Layer                      │
//! │        (client/core implements SwarmClient)                 │
//! │        (bandwidth implements BandwidthAccounting)           │
//! └─────────────────────────────────────────────────────────────┘
//!                              ▲ events    │ commands
//!                              │           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                  ClientBehaviour                        │
//! │                  (protocol plumbing)                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```

mod behaviour;
mod events;
mod handler;
pub mod upgrade;

pub use behaviour::{ClientBehaviour, Config as BehaviourConfig};
pub use events::{ClientCommand, ClientEvent, PseudosettleEvent, SwapEvent};
pub use handler::{ClientHandler, Config as HandlerConfig, HandlerCommand, HandlerEvent};
pub use upgrade::{
    ClientInboundOutput, ClientInboundUpgrade, ClientOutboundInfo, ClientOutboundOutput,
    ClientOutboundRequest, ClientOutboundUpgrade, ClientUpgradeError,
};
