use alloy_chains::{Chain, NamedChain};
use core::{cmp::Ordering, fmt};
use num_enum::TryFromPrimitiveError;

// #[allow(unused_imports)]
// use alloc::string::String;

/// A named swarm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, strum::IntoStaticStr)] // Into<&'static str>, AsRef<str>, fmt::Display and serde::Serialize
#[derive(strum::VariantNames)] // NamedSwarm::VARIANTS
#[derive(strum::VariantArray)] // NamedSwarm::VARIANTS
#[derive(strum::EnumString)] // FromStr, TryFrom<&str>
#[derive(strum::EnumIter)] // NamedSwarm::iter
#[derive(strum::EnumCount)] // NamedSwarm::COUNT
#[derive(num_enum::TryFromPrimitive)] // TryFrom<u64>
#[cfg_attr(feature = "serde", derive(serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum(serialize_all = "kebab-case")]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[repr(u64)]
#[non_exhaustive]
pub enum NamedSwarm {
    #[strum(to_string = "mainnet")]
    Mainnet = 1,

    #[strum(to_string = "sepolia")]
    Testnet = 10,

    #[strum(to_string = "dev")]
    Dev = 1337,
}

// Manual Default implementation to avoid conflict with TryFromPrimitive
impl Default for NamedSwarm {
    #[inline]
    fn default() -> Self {
        Self::Mainnet
    }
}

macro_rules! impl_into_numeric {
    ($($t:ty)+) => {$(
        impl From<NamedSwarm> for $t {
            #[inline]
            fn from(swarm: NamedSwarm) -> Self {
                swarm as $t
            }
        }
    )+};
}

impl_into_numeric!(u64 i64 u128 i128);
#[cfg(target_pointer_width = "64")]
impl_into_numeric!(usize isize);

macro_rules! impl_try_from_numeric {
    ($($native:ty)+) => {
        $(
            impl TryFrom<$native> for NamedSwarm {
                type Error = TryFromPrimitiveError<NamedSwarm>;

                #[inline]
                fn try_from(value: $native) -> Result<Self, Self::Error> {
                    (value as u64).try_into()
                }
            }
        )+
    };
}

impl_try_from_numeric!(u8 i8 u16 i16 u32 i32 usize isize);

impl fmt::Display for NamedSwarm {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl AsRef<str> for NamedSwarm {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<u64> for NamedSwarm {
    #[inline]
    fn eq(&self, other: &u64) -> bool {
        (*self as u64) == *other
    }
}

impl PartialOrd<u64> for NamedSwarm {
    #[inline]
    fn partial_cmp(&self, other: &u64) -> Option<Ordering> {
        (*self as u64).partial_cmp(other)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for NamedSwarm {
    #[inline]
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_ref())
    }
}

impl NamedSwarm {
    /// Returns the string representation of the swarm.
    #[inline]
    pub fn as_str(&self) -> &'static str {
        self.into()
    }

    /// Returns true if this is the mainnet swarm
    pub const fn is_mainnet(&self) -> bool {
        matches!(self, NamedSwarm::Mainnet)
    }

    /// Returns true if this is a testnet swarm
    pub const fn is_testnet(&self) -> bool {
        matches!(self, NamedSwarm::Testnet)
    }

    /// Returns the chain that the swarm is on
    pub fn chain(&self) -> Chain {
        match self {
            NamedSwarm::Mainnet => NamedChain::Gnosis.into(),
            NamedSwarm::Testnet => NamedChain::Sepolia.into(),
            NamedSwarm::Dev => NamedChain::Dev.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::{EnumCount, IntoEnumIterator};

    #[test]
    fn enum_iter() {
        assert_eq!(NamedSwarm::COUNT, NamedSwarm::iter().size_hint().0);
    }

    #[test]
    fn roundtrip_string() {
        for swarm in NamedSwarm::iter() {
            let swarm_string = swarm.to_string();
            assert_eq!(swarm_string, format!("{swarm}"));
            assert_eq!(swarm_string.as_str(), swarm.as_ref());
            assert_eq!(swarm_string.parse::<NamedSwarm>().unwrap(), swarm);
        }
    }

    #[test]
    fn test_is_mainnet() {
        assert!(NamedSwarm::Mainnet.is_mainnet());
        assert!(!NamedSwarm::Testnet.is_mainnet());
    }

    #[test]
    fn test_is_testnet() {
        assert!(NamedSwarm::Testnet.is_testnet());
        assert!(!NamedSwarm::Mainnet.is_testnet());
    }

    #[test]
    fn test_swarm_equality() {
        assert_eq!(NamedSwarm::Mainnet, NamedSwarm::Mainnet);
        assert_ne!(NamedSwarm::Mainnet, NamedSwarm::Testnet);
        assert_ne!(NamedSwarm::Testnet, NamedSwarm::Dev);
    }

    #[test]
    fn test_partial_eq_ord_u64() {
        // Test PartialEq with u64
        assert!(NamedSwarm::Mainnet == 1u64);
        assert!(NamedSwarm::Testnet == 10u64);
        assert!(!(NamedSwarm::Mainnet == 2u64));

        // Test PartialOrd with u64
        assert!(NamedSwarm::Mainnet < 2u64);
        assert!(NamedSwarm::Testnet > 9u64);
        assert!(NamedSwarm::Dev > 1000u64);
    }

    #[test]
    fn test_swarm_chain_mapping() {
        assert_eq!(NamedSwarm::Mainnet.chain(), Chain::from(NamedChain::Gnosis));
        assert_eq!(
            NamedSwarm::Testnet.chain(),
            Chain::from(NamedChain::Sepolia)
        );
        assert_eq!(NamedSwarm::Dev.chain(), Chain::from(NamedChain::Dev));
    }
}
