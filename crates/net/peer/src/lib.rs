//! Protocol-agnostic network utilities for libp2p.
//!
//! - [`scope`] - IP address classification (loopback, private, link-local, public)
//! - [`local_network`] - System interface queries for subnet detection
//! - [`address_manager`] - Smart address selection and NAT discovery

pub mod address_manager;
pub mod local_network;
pub mod scope;

pub use address_manager::AddressManager;
pub use local_network::{
    LocalNetworkInfo, get_local_network_info, is_directly_reachable, is_on_same_local_network,
};
pub use scope::{
    AddressScope, IpCapability, IpVersion, classify_multiaddr, extract_ip, ip_version, is_ipv4,
    is_ipv6, same_subnet,
};
