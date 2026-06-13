//! Swarm client behaviour for the Vertex node.
//!
//! This module provides the `ClientBehaviour` which handles all client-side
//! protocols on the Swarm network:
//!
//! - **Pricing**: Payment threshold exchange
//! - **Retrieval**: Chunk request/response
//! - **PushSync**: Chunk push with receipt
//! - **Pseudosettle**: Bandwidth accounting settlement
//! - **Swap**: Cheque-based settlement wire plumbing (behind the `swap` feature)
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
//! # Swap layering
//!
//! The swap integration here is the **wire plumbing only**. It composes the
//! `/swarm/swap` protocol into the handler and upgrade, emits a cheque on
//! [`ClientCommand::SendCheque`], and surfaces a received cheque as
//! [`SwapEvent::ChequeReceived`] (and an outbound completion as
//! [`SwapEvent::ChequeSent`]) with strong types: typed peer, the full
//! `SignedCheque`, and the peer's exchange rate from the headers exchange.
//!
//! The settlement **policy** lives downstream, mirroring how pseudosettle is
//! layered. When to issue a cheque (debt crossing the payment threshold), when
//! to expect a cheque, and when to disconnect a peer that exceeds the
//! disconnect threshold without paying are decided by the swap
//! `SwarmSettlementProvider` in the `vertex-swarm-bandwidth-swap` crate, driven
//! by the per-peer balance state, and connected to this wire layer by the node
//! builder's accounting wiring. This crate carries no thresholds and makes no
//! settlement decisions.
//!
//! # Handler Lifecycle
//!
//! The `ClientHandler` is created in dormant state when a connection is
//! established. After the handshake completes (signaled by `TopologyEvent::PeerAuthenticated`),
//! the node sends an `ActivatePeer` command which transitions the handler to
//! active state with:
//! - Peer's overlay address
//! - Node type (`SwarmNodeType`)
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
mod forward;
mod handler;
pub(crate) mod upgrade;

pub(crate) use behaviour::{ClientBehaviour, Config as BehaviourConfig};
#[cfg(feature = "swap")]
pub use events::SwapEvent;
pub use events::{
    ClientCommand, ClientEvent, FailureKind, PseudosettleEvent, PushResponseTx, RetrievalResponseTx,
};
pub(crate) use forward::{NetworkForwarder, StubForwarder};
