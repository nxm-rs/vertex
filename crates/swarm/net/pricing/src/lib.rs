//! Pricing protocol for Swarm payment threshold announcement.
//!
//! Provides:
//! - Wire codec and stream upgrades for `/swarm/pricing/1.0.0/pricing`.
//! - A configurable [`PricingBehaviour`] (libp2p `NetworkBehaviour`) used by
//!   every node type that speaks the protocol. Bootnodes use the listen-only
//!   shape ([`PricingBehaviour::new_bootnode`]); clients and full nodes use
//!   the announce-on-connect shape ([`PricingBehaviour::new_announcer`]).
//!
//! Advertising this protocol is required for interop with peers that close
//! the connection when their pricing `ConnectIn` / `ConnectOut` hook fails.

mod behaviour;
mod codec;
mod error;
mod handler;
mod protocol;
mod stub;

pub use behaviour::{PricingBehaviour, PricingEvent, PricingMode, PricingRole};
pub use codec::AnnouncePaymentThreshold;
pub use error::PricingError;
pub use handler::{PricingHandler, PricingHandlerCommand, PricingHandlerEvent};
pub use protocol::{PricingInboundProtocol, PricingOutboundProtocol, inbound, outbound};
pub use stub::{PaymentThresholdObserver, StubObserver};

/// Protocol name for pricing.
pub const PROTOCOL_NAME: &str = "/swarm/pricing/1.0.0/pricing";
