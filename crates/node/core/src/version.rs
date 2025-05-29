//! Version information for vertex.
use bytes::Bytes;

/// The human readable name of the client
pub const NAME_CLIENT: &str = "Vertex";

/// The latest version from Cargo.toml.
pub const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The full SHA of the latest commit.
pub const VERGEN_GIT_SHA_LONG: &str = env!("VERGEN_GIT_SHA");

/// The 8 character short SHA of the latest commit.
pub const VERGEN_GIT_SHA: &str = env!("VERGEN_GIT_SHA_SHORT");

/// The build timestamp.
pub const VERGEN_BUILD_TIMESTAMP: &str = env!("VERGEN_BUILD_TIMESTAMP");

/// The target triple.
pub const VERGEN_CARGO_TARGET_TRIPLE: &str = env!("VERGEN_CARGO_TARGET_TRIPLE");

/// The build features.
pub const VERGEN_CARGO_FEATURES: &str = env!("VERGEN_CARGO_FEATURES");

/// The short version information for vertex.
pub const SHORT_VERSION: &str = env!("VERTEX_SHORT_VERSION");

/// The long version information for vertex.
pub const LONG_VERSION: &str = concat!(
    env!("VERTEX_LONG_VERSION_0"),
    "\n",
    env!("VERTEX_LONG_VERSION_1"),
    "\n",
    env!("VERTEX_LONG_VERSION_2"),
    "\n",
    env!("VERTEX_LONG_VERSION_3"),
    "\n",
    env!("VERTEX_LONG_VERSION_4")
);

/// The build profile name.
pub const BUILD_PROFILE_NAME: &str = env!("VERTEX_BUILD_PROFILE");

/// The version information for vertex formatted for P2P (devp2p).
///
/// - The latest version from Cargo.toml
/// - The target triple
///
/// # Example
///
/// ```text
/// vertex/v{major}.{minor}.{patch}-{sha1}/{target}
/// ```
/// e.g.: `vertex/v0.1.0-alpha.1-428a6dc2f/aarch64-apple-darwin`
pub const P2P_CLIENT_VERSION: &str = env!("VERTEX_P2P_CLIENT_VERSION");

/// The default extra data used for payload building.
///
/// - The latest version from Cargo.toml
/// - The OS identifier
///
/// # Example
///
/// ```text
/// vertex/v{major}.{minor}.{patch}/{OS}
/// ```
pub fn default_extra_data() -> String {
    format!(
        "vertex/v{}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    )
}

/// The default extra data in bytes.
/// See [`default_extra_data`].
pub fn default_extra_data_bytes() -> Bytes {
    Bytes::from(default_extra_data().as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_extra_data_less_32bytes() {
        let extra_data = default_extra_data();
        assert!(
            extra_data.len() <= 32,
            "extra data must be less than 32 bytes: {extra_data}"
        )
    }
}
