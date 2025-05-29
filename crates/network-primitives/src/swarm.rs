use crate::NamedSwarm;
use core::{cmp::Ordering, fmt, str::FromStr};

#[cfg(feature = "arbitrary")]
use proptest::{
    sample::Selector,
    strategy::{Map, TupleUnion, WA},
};
#[cfg(feature = "arbitrary")]
use strum::{EnumCount, IntoEnumIterator};

/// Either a known [`NamedSwarm`] or a custom swarm ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Swarm(SwarmKind);

/// The kind of swarm. Returned by [`Swarm::kind`]. Prefer using [`Swarm`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SwarmKind {
    /// Known swarm.
    Named(NamedSwarm),
    /// Custom swarm ID.
    Id(u64),
}

impl fmt::Debug for Swarm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Swarm::")?;
        self.kind().fmt(f)
    }
}

impl Default for Swarm {
    #[inline]
    fn default() -> Self {
        Self::from_named(NamedSwarm::default())
    }
}

impl From<NamedSwarm> for Swarm {
    #[inline]
    fn from(id: NamedSwarm) -> Self {
        Self::from_named(id)
    }
}

impl From<u64> for Swarm {
    #[inline]
    fn from(id: u64) -> Self {
        Self::from_id(id)
    }
}

impl TryFrom<Swarm> for NamedSwarm {
    type Error = <NamedSwarm as TryFrom<u64>>::Error;

    #[inline]
    fn try_from(swarm: Swarm) -> Result<Self, Self::Error> {
        match *swarm.kind() {
            SwarmKind::Named(swarm) => Ok(swarm),
            SwarmKind::Id(id) => id.try_into(),
        }
    }
}

impl FromStr for Swarm {
    type Err = core::num::ParseIntError;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(swarm) = NamedSwarm::from_str(s) {
            Ok(Self::from_named(swarm))
        } else {
            s.parse::<u64>().map(Self::from_id)
        }
    }
}

impl fmt::Display for Swarm {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            SwarmKind::Named(swarm) => swarm.fmt(f),
            SwarmKind::Id(id) => id.fmt(f),
        }
    }
}

impl PartialEq<u64> for Swarm {
    #[inline]
    fn eq(&self, other: &u64) -> bool {
        self.id().eq(other)
    }
}

impl PartialEq<Swarm> for u64 {
    #[inline]
    fn eq(&self, other: &Swarm) -> bool {
        other.eq(self)
    }
}

impl PartialOrd<u64> for Swarm {
    #[inline]
    fn partial_cmp(&self, other: &u64) -> Option<Ordering> {
        self.id().partial_cmp(other)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Swarm {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.kind() {
            SwarmKind::Named(swarm) => swarm.serialize(serializer),
            SwarmKind::Id(id) => id.serialize(serializer),
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Swarm {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SwarmVisitor;

        impl serde::de::Visitor<'_> for SwarmVisitor {
            type Value = Swarm;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("swarm name or ID")
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                if v.is_negative() {
                    Err(serde::de::Error::invalid_value(
                        serde::de::Unexpected::Signed(v),
                        &self,
                    ))
                } else {
                    Ok(Swarm::from_id(v as u64))
                }
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Swarm::from_id(value))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_any(SwarmVisitor)
    }
}

impl Swarm {
    /// Creates a new [`Swarm`] by wrapping a [`NamedSwarm`].
    #[inline]
    pub const fn from_named(named: NamedSwarm) -> Self {
        Self(SwarmKind::Named(named))
    }

    /// Creates a new [`Swarm`] by wrapping an ID.
    #[inline]
    pub fn from_id(id: u64) -> Self {
        if let Ok(named) = NamedSwarm::try_from(id) {
            Self::from_named(named)
        } else {
            Self::from_id_unchecked(id)
        }
    }

    /// Creates a new [`Swarm`] from the given ID, without checking if an associated [`NamedSwarm`]
    /// exists.
    #[inline]
    pub const fn from_id_unchecked(id: u64) -> Self {
        Self(SwarmKind::Id(id))
    }

    /// Returns the kind of this swarm.
    #[inline]
    pub const fn kind(&self) -> &SwarmKind {
        &self.0
    }

    /// Returns the ID of the swarm.
    #[inline]
    pub const fn id(self) -> u64 {
        match *self.kind() {
            SwarmKind::Named(named) => named as u64,
            SwarmKind::Id(id) => id,
        }
    }

    /// Attempts to convert the swarm into a named swarm.
    #[inline]
    pub const fn named(self) -> Option<NamedSwarm> {
        match *self.kind() {
            SwarmKind::Named(named) => Some(named),
            SwarmKind::Id(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id() {
        assert_eq!(Swarm::from_id(1234).id(), 1234);
    }

    #[test]
    fn test_named_id() {
        assert_eq!(Swarm::from_named(NamedSwarm::Testnet).id(), 10);
    }

    #[test]
    fn test_display_named_swarm() {
        assert_eq!(
            Swarm::from_named(NamedSwarm::Mainnet).to_string(),
            "mainnet"
        );
    }

    #[test]
    fn test_display_id_swarm() {
        assert_eq!(Swarm::from_id(1234).to_string(), "1234");
    }

    #[test]
    fn test_from_str_named_swarm() {
        let result = Swarm::from_str("mainnet");
        let expected = Swarm::from_named(NamedSwarm::Mainnet);
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_from_str_named_swarm_error() {
        let result = Swarm::from_str("swarm");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_str_id_swarm() {
        let result = Swarm::from_str("1234");
        let expected = Swarm::from_id(1234);
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_default() {
        let default = Swarm::default();
        let expected = Swarm::from_named(NamedSwarm::Mainnet);
        assert_eq!(default, expected);
    }

    #[test]
    fn test_named_roundtrip() {
        let original = NamedSwarm::Testnet;
        let swarm = Swarm::from_named(original);
        let converted: NamedSwarm = swarm.try_into().unwrap();
        assert_eq!(original, converted);
    }

    #[test]
    fn test_try_from_swarm() {
        // Test successful conversion for named swarm
        let named_swarm = Swarm::from_named(NamedSwarm::Mainnet);
        assert!(NamedSwarm::try_from(named_swarm).is_ok());

        // Test conversion failure for custom ID
        let custom_swarm = Swarm::from_id(999999);
        assert!(NamedSwarm::try_from(custom_swarm).is_err());
    }

    #[test]
    fn test_from_id_conversion() {
        // Test that known IDs are converted to named swarms
        let swarm = Swarm::from_id(10); // Assuming 10 is Testnet's ID
        assert_eq!(swarm.named(), Some(NamedSwarm::Testnet));

        // Test that unknown IDs remain as custom IDs
        let swarm = Swarm::from_id(999999);
        assert_eq!(swarm.named(), None);
    }

    #[test]
    fn test_equality_with_u64() {
        let swarm = Swarm::from_id(1234);
        assert_eq!(swarm, 1234u64);
        assert_eq!(1234u64, swarm);
        assert_ne!(swarm, 5678u64);
    }
}
