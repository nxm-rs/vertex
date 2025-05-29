//! Network-related primitive types

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

/// Network ID type
pub type NetworkId = u64;

/// Ethereum address (20 bytes)
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Address(pub [u8; 20]);

impl Address {
    /// Creates a new Address from raw bytes
    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Returns the underlying bytes
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl core::fmt::Debug for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Address(0x{})", hex::encode(&self.0))
    }
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "0x{}", hex::encode(&self.0))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Address {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("0x{}", hex::encode(&self.0)))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Address {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.strip_prefix("0x").unwrap_or(&s);
        let bytes = hex::decode(s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 20 {
            return Err(serde::de::Error::custom("Address must be exactly 20 bytes"));
        }
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&bytes);
        Ok(Address(arr))
    }
}

/// Information about the Swarm network
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwarmInfo {
    /// Network ID
    pub network_id: NetworkId,
    /// Block hash of the best block
    pub best_hash: [u8; 32],
    /// Block number of the best block
    pub best_number: u64,
    /// Estimated network size
    pub network_size: u64,
    /// Global postage price per chunk
    pub global_postage_price: u64,
    /// Neighborhood radius (proximity order)
    pub neighborhood_radius: u8,
    /// Connected peer count
    pub connected_peers: u16,
}

/// Network status information
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NetworkStatus {
    /// Number of connected peers
    pub connected_peers: usize,
    /// Neighborhood depth (radius of responsibility)
    pub neighborhood_depth: u8,
    /// Estimated network size
    pub estimated_network_size: usize,
    /// Whether the node is connected to the network
    pub is_connected: bool,
    /// Network bandwidth usage statistics
    pub bandwidth_stats: BandwidthStats,
}

/// Bandwidth usage statistics
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BandwidthStats {
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Current upload rate in bytes per second
    pub upload_rate_bps: u64,
    /// Current download rate in bytes per second
    pub download_rate_bps: u64,
}

/// Node mode (light, full, or incentivized)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NodeMode {
    /// Light client only
    Light,
    /// Full node with storage
    Full,
    /// Full node participating in storage incentives
    Incentivized,
}
