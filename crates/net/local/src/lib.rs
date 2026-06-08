//! Protocol-agnostic local network utilities for libp2p.
//!
//! - [`scope`] - IP address classification (loopback, private, link-local, public)
//! - [`system`] - System interface queries for subnet detection
//! - [`capabilities`] - Local node network capabilities tracking

pub mod capabilities;
pub mod scope;
pub mod system;

pub use capabilities::{LocalCapabilities, advertise_filter};
pub use scope::{
    AddressFamily, AddressScope, IpCapability, classify_multiaddr, family_order, is_dialable,
    is_globally_routable_ipv6,
};
pub use system::{add_subnet, remove_subnet, same_subnet};
