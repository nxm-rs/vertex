//! Protocol-agnostic local network utilities for libp2p.
//!
//! - [`scope`] - IP address classification (loopback, private, link-local, public)
//! - [`system`] - System interface queries for subnet detection
//! - [`capabilities`] - Local node network capabilities tracking

pub mod capabilities;
pub mod scope;
pub mod system;

pub use capabilities::LocalCapabilities;
pub use scope::{AddressScope, IpCapability, classify_multiaddr, is_dialable};
pub use system::{add_subnet, remove_subnet, same_subnet};
