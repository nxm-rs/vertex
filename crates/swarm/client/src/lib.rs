//! Swarm client protocol behaviour and service.
//!
//! Provides the [`ClientBehaviour`] (composed libp2p `NetworkBehaviour` multiplexing
//! credit, retrieval, pushsync, pseudosettle) and the [`ClientService`]/[`ClientHandle`]
//! business-logic bridge.
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
//! - Node type (full or light)
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

#![cfg_attr(not(feature = "std"), no_std)]

mod behaviour;
mod events;
mod handler;
mod queue;
mod resolver;
mod service;
pub(crate) mod upgrade;

pub use behaviour::{ClientBehaviour, Config as BehaviourConfig};
pub use events::{ClientCommand, ClientEvent, ClientProtocol, PseudosettleEvent};
pub use resolver::PeerAddressResolver;
pub use service::{ClientHandle, ClientService, RetrievalError, RetrievalResult};

pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;
