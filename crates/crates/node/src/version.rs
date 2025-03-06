//! Version information for the Vertex Swarm node.

/// The version string: semver + git sha
pub const VERSION: &str = concat!(env!("VERTEX_VERSION"), " (", env!("VERTEX_GIT_SHA"), ")");

/// The short version information for Vertex.
pub const SHORT_VERSION: &str = env!("VERTEX_VERSION");

/// The git commit SHA.
pub const GIT_SHA: &str = env!("VERTEX_GIT_SHA");

/// The build timestamp.
pub const BUILD_TIMESTAMP: &str = env!("VERGEN_BUILD_TIMESTAMP");

/// The cargo features.
pub const CARGO_FEATURES: &str = env!("VERGEN_CARGO_FEATURES");

/// The long version information for Vertex.
///
/// Example:
///
/// ```text
/// Version: 0.1.0
/// Commit SHA: defa64b2
/// Build Timestamp: 2023-05-19T01:47:19.815651705Z
/// Build Features: default
/// ```
pub const LONG_VERSION: &str = concat!(
    "Version: ",
    env!("VERTEX_VERSION"),
    "\n",
    "Commit SHA: ",
    env!("VERTEX_GIT_SHA"),
    "\n",
    "Build Timestamp: ",
    env!("VERGEN_BUILD_TIMESTAMP"),
    "\n",
    "Build Features: ",
    env!("VERGEN_CARGO_FEATURES")
);

/// The user agent string for network communication.
pub const USER_AGENT: &str = concat!("vertex/", env!("VERTEX_VERSION"));

/// The version information for libp2p identification.
pub const P2P_CLIENT_VERSION: &str = concat!("vertex/v", env!("VERTEX_VERSION"));
