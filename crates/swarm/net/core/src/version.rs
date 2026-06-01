//! Semantic version newtype for Swarm protocol IDs.

use std::fmt;

/// A semantic version triple (`MAJOR.MINOR.PATCH`) as used in Swarm protocol
/// identifiers.
///
/// Swarm protocol IDs follow the layout `/swarm/{name}/{MAJOR.MINOR.PATCH}/{stream}`,
/// where the version segment is parsed and compared with the same rule used by
/// `go-libp2p` / Bee's `protocolSemverMatcher`: a remote (client) version
/// matches the local (server) version iff the majors are equal and the server's
/// minor is at least the client's minor. Patch differences are ignored.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct SemanticVersion(u16, u16, u16);

impl SemanticVersion {
    /// Construct a [`SemanticVersion`] from its three components.
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self(major, minor, patch)
    }

    /// Major version component.
    pub const fn major(self) -> u16 {
        self.0
    }

    /// Minor version component.
    pub const fn minor(self) -> u16 {
        self.1
    }

    /// Patch version component.
    pub const fn patch(self) -> u16 {
        self.2
    }

    /// Compatibility check between a `client`-advertised version and the local
    /// `server` version.
    ///
    /// Mirrors Bee's `protocolSemverMatcher`: a client request is accepted iff
    /// `server.major == client.major && server.minor >= client.minor`.
    pub const fn matches(client: Self, server: Self) -> bool {
        server.0 == client.0 && server.1 >= client.1
    }
}

impl fmt::Debug for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Compose a canonical Swarm protocol ID at compile time.
///
/// Expands to a `&'static str` of the form `/swarm/{name}/{MAJOR.MINOR.PATCH}/{stream}`.
///
/// # Example
///
/// ```
/// use vertex_swarm_net_core::swarm_protocol_id;
/// const ID: &str = swarm_protocol_id!("pingpong", 1, 0, 0, "pingpong");
/// assert_eq!(ID, "/swarm/pingpong/1.0.0/pingpong");
/// ```
#[macro_export]
macro_rules! swarm_protocol_id {
    ($name:literal, $major:literal, $minor:literal, $patch:literal, $stream:literal) => {
        ::core::concat!(
            "/swarm/",
            $name,
            "/",
            ::core::stringify!($major),
            ".",
            ::core::stringify!($minor),
            ".",
            ::core::stringify!($patch),
            "/",
            $stream,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_same_version_is_true() {
        let v = SemanticVersion::new(1, 0, 0);
        assert!(SemanticVersion::matches(v, v));
    }

    #[test]
    fn matches_requires_equal_major() {
        let client = SemanticVersion::new(1, 5, 0);
        let server = SemanticVersion::new(2, 5, 0);
        assert!(!SemanticVersion::matches(client, server));
        assert!(!SemanticVersion::matches(server, client));
    }

    #[test]
    fn matches_server_minor_must_be_at_least_client() {
        let client = SemanticVersion::new(1, 2, 0);
        let newer_server = SemanticVersion::new(1, 5, 0);
        let older_server = SemanticVersion::new(1, 1, 0);

        // server has a strictly newer minor — backwards compatible, accepted.
        assert!(SemanticVersion::matches(client, newer_server));
        // server has an older minor — cannot serve this client, rejected.
        assert!(!SemanticVersion::matches(client, older_server));
    }

    #[test]
    fn matches_ignores_patch() {
        let a = SemanticVersion::new(3, 4, 0);
        let b = SemanticVersion::new(3, 4, 99);
        assert!(SemanticVersion::matches(a, b));
        assert!(SemanticVersion::matches(b, a));
    }

    #[test]
    fn display_uses_dotted_triple() {
        let v = SemanticVersion::new(14, 0, 0);
        assert_eq!(v.to_string(), "14.0.0");
        assert_eq!(format!("{v:?}"), "14.0.0");
    }

    #[test]
    fn macro_composes_protocol_id() {
        const ID: &str = swarm_protocol_id!("handshake", 14, 0, 0, "handshake");
        assert_eq!(ID, "/swarm/handshake/14.0.0/handshake");
    }

    #[test]
    fn macro_composes_protocol_id_with_distinct_stream() {
        const ID: &str = swarm_protocol_id!("hive", 1, 1, 0, "peers");
        assert_eq!(ID, "/swarm/hive/1.1.0/peers");
    }
}
