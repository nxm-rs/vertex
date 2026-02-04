//! Protocol-agnostic local network utilities for libp2p.
//!
//! - [`scope`] - IP address classification (loopback, private, link-local, public)
//! - [`system`] - System interface queries for subnet detection
//! - [`capabilities`] - Local node network capabilities tracking

pub mod capabilities;
pub mod scope;
pub mod system;

pub use capabilities::LocalCapabilities;
pub use scope::{
    AddressScope, IpCapability, IpVersion, NetworkCapability, TransportCapability,
    classify_multiaddr, extract_ip, ip_version, is_ipv4, is_ipv6, same_subnet,
};
pub use system::{
    LocalSubnets, is_directly_reachable, is_on_same_subnet, query_local_subnets, refresh_subnets,
};
