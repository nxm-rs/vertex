//! Fork digest for network compatibility verification.

use alloc::vec::Vec;
use alloy_primitives::{FixedBytes, keccak256};

/// A compact identifier representing a network's fork state at a point in time.
///
/// Used during peer handshake to verify network compatibility. Two peers with
/// matching digests can interoperate; mismatched digests indicate incompatible
/// protocol versions or different networks.
///
/// The digest is computed from:
/// - Network ID (ensures different networks have different digests)
/// - Genesis timestamp (distinguishes network incarnations)
/// - Active fork timestamps (ensures protocol version agreement)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ForkDigest(pub FixedBytes<4>);

impl ForkDigest {
    /// Creates a new fork digest from raw bytes.
    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(FixedBytes::new(bytes))
    }

    /// Returns the digest as a byte array.
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0.0
    }

    /// Computes a fork digest from the given components using keccak256.
    ///
    /// Takes the first 4 bytes of the keccak256 hash of the concatenated
    /// little-endian encoded values.
    pub fn compute(
        network_id: u64,
        genesis_timestamp: u64,
        active_fork_timestamps: &[u64],
    ) -> Self {
        let mut data = Vec::with_capacity(16 + active_fork_timestamps.len() * 8);

        data.extend_from_slice(&network_id.to_le_bytes());
        data.extend_from_slice(&genesis_timestamp.to_le_bytes());

        for &timestamp in active_fork_timestamps {
            data.extend_from_slice(&timestamp.to_le_bytes());
        }

        let hash = keccak256(&data);
        Self(FixedBytes::from_slice(&hash[..4]))
    }
}

impl core::fmt::Display for ForkDigest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fork_digest_display() {
        let digest = ForkDigest::new([0xab, 0xcd, 0xef, 0x12]);
        assert_eq!(format!("{}", digest), "0xabcdef12");
    }

    #[test]
    fn test_fork_digest_deterministic() {
        let d1 = ForkDigest::compute(1, 1000, &[2000, 3000]);
        let d2 = ForkDigest::compute(1, 1000, &[2000, 3000]);
        assert_eq!(d1, d2);
    }

    #[test]
    fn test_fork_digest_differs_by_network() {
        let d1 = ForkDigest::compute(1, 1000, &[]);
        let d2 = ForkDigest::compute(2, 1000, &[]);
        assert_ne!(d1, d2);
    }

    #[test]
    fn test_fork_digest_differs_by_forks() {
        let d1 = ForkDigest::compute(1, 1000, &[2000]);
        let d2 = ForkDigest::compute(1, 1000, &[3000]);
        assert_ne!(d1, d2);
    }
}
