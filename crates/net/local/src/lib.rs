//! Protocol-agnostic local network utilities for libp2p.
//!
//! - [`scope`] - IP address classification (loopback, private, link-local, public)
//! - [`transport`] - Transport-suite classification and combined dial eligibility
//! - [`system`] - System interface queries for subnet detection
//! - [`capabilities`] - Local node network capabilities tracking

pub mod capabilities;
pub mod scope;
pub mod system;
pub mod transport;

pub use capabilities::{LocalCapabilities, advertise_filter};
pub use scope::{
    AddressFamily, AddressScope, IpCapability, classify_multiaddr, extract_ip, family_order,
    is_dialable,
};
pub use system::{add_subnet, remove_subnet, same_subnet};
pub use transport::{DialCapability, TransportCapability, TransportRequirement};
