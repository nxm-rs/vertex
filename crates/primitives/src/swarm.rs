use std::{cmp::Ordering, fmt, str::FromStr};
use num_enum::TryFromPrimitiveError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[derive(strum::VariantNames)] // NamedChain::VARIANTS
#[derive(strum::IntoStaticStr)] // Into<&'static str>, AsRef<str>, fmt::Display and serde::Serialize
#[derive(strum::EnumString)] // FromStr, TryFrom<&str>
#[derive(num_enum::TryFromPrimitive)] // TryFrom<u64>
#[repr(u64)]
pub enum NamedSwarm {
    #[strum(to_string = "mainnet")]
    Mainnet = 1,
    #[strum(to_string = "testnet")]
    Testnet = 10
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

impl Default for NamedSwarm {
    fn default() -> Self {
        Self::Mainnet
    }
}

impl fmt::Display for NamedSwarm {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl NamedSwarm {
    /// Returns the string representation of the swarm.
    #[inline]
    pub fn as_str(&self) -> &'static str {
        self.into()
    }
}

/// Either a known [`NamedSwarm`] or a custom swarm ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Swarm(SwarmKind);

/// The kind of swarm. Retuned by [`Swarm::kind`]. Prefer using [`Swarm`] instead.
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

impl From<Swarm> for u64 {
    #[inline]
    fn from(swarm: Swarm) -> u64 {
        swarm.id()
    }
}

impl TryFrom<Swarm> for NamedSwarm {
    type Error = <NamedSwarm as TryFrom<u64>>::Error;

    #[inline]
    fn try_from(swarm: Swarm) -> Result<Self, Self::Error> {
        match *swarm.kind() {
            SwarmKind::Named(swarm) => Ok(swarm),
            SwarmKind::Id(id) => Self::try_from(id),
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
        struct ChainVisitor;

        impl<'de> serde::de::Visitor<'de> for ChainVisitor {
            type Value = Swarm;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("swarm name or ID")
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                if v.is_negative() {
                    Err(serde::de::Error::invalid_value(serde::de::Unexpected::Signed(v), &self))
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

        deserializer.deserialize_any(ChainVisitor)
    }
}

impl Swarm {
    #[allow(non_snake_case)]
    #[doc(hidden)]
    #[deprecated(since = "0.1.0", note = "use `Self::from_named()` instead")]
    #[inline]
    pub const fn Named(named: NamedSwarm) -> Self {
        Self::from_named(named)
    }

    #[allow(non_snake_case)]
    #[doc(hidden)]
    #[deprecated(since = "0.1.0", note = "use `Self::from_id()` instead")]
    #[inline]
    pub const fn Id(id: u64) -> Self {
        Self::from_id_unchecked(id)
    }

    /// Creates a new [`Swarm`] by wrapping a [`NamedSwarm`].
    #[inline]
    pub const fn from_named(named: NamedSwarm) -> Self {
        Self(SwarmKind::Named(named))
    }

    /// Creates a new [`Swarm`] by wrapping a [`NamedSwarm`].
    #[inline]
    pub fn from_id(id: u64) -> Self {
        if let Ok(named) = NamedSwarm::try_from(id) {
            Self::from_named(named)
        } else {
            Self::from_id_unchecked(id)
        }
    }

    /// Creates a new [`Swarm`] from the given ID, without checking if an associated [`SwarmKind`]
    /// exists.
    ///
    /// This is discouraged, as other methods assume that the swarm ID is not known, but it is not
    /// unsafe.
    #[inline]
    pub const fn from_id_unchecked(id: u64) -> Self {
        Self(SwarmKind::Id(id))
    }

    /// Returns the kind of swarm.
    #[inline]
    pub const fn kind(&self) -> &SwarmKind {
        &self.0
    }

    /// Returns the kind of swarm.
    #[inline]
    pub fn into_kind(self) -> SwarmKind {
        self.0
    }

    /// Returns the mainnet swarm
    #[inline]
    pub const fn mainnet() -> Self {
        Self::from_named(NamedSwarm::Mainnet)
    }

    /// Returns the testnet swarm
    #[inline]
    pub const fn testnet() -> Self {
        Self::from_named(NamedSwarm::Testnet)
    }

    /// Attempts to convert the swarm into a named swarm.
    #[inline]
    pub const fn named(self) -> Option<NamedSwarm> {
        match *self.kind() {
            SwarmKind::Named(named) => Some(named),
            SwarmKind::Id(_) => None,
        }
    }

    /// The ID of the Swarm
    #[inline]
    pub const fn id(self) -> u64 {
        match *self.kind() {
            SwarmKind::Named(named) => named as u64,
            SwarmKind::Id(id) => id,
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
        assert_eq!(Swarm::from_named(NamedSwarm::Mainnet).id(), 1);
    }

    #[test]
    fn test_display_named_swarm() {
        assert_eq!(Swarm::from_named(NamedSwarm::Mainnet).to_string(), "mainnet");
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

    #[cfg(feature = "serde")]
    #[test]
    fn test_serde() {
        let swarms = r#"["mainnet",1,10,"testnet"]"#;
        let re = r#"["mainnet","mainnet","testnet","testnet"]"#;
        let expected = [
            Swarm::mainnet(),
            Swarm::mainnet(),
            Swarm::from_named(NamedSwarm::Testnet),
            Swarm::from_id(10),
        ];
        assert_eq!(serde_json::from_str::<alloc::vec::Vec<Swarm>>(swarms).unwrap(), expected);
        assert_eq!(serde_json::to_string(&expected).unwrap(), re);
    }
}
