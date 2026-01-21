//! Version information for the Vertex Swarm node.

/// The version string from Cargo.toml
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short version information for Vertex.
pub const SHORT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The git commit SHA (set by build.rs if available).
pub const GIT_SHA: &str = {
    match option_env!("VERTEX_GIT_SHA") {
        Some(sha) => sha,
        None => "unknown",
    }
};

/// The build timestamp (set by build.rs if available).
pub const BUILD_TIMESTAMP: &str = {
    match option_env!("VERGEN_BUILD_TIMESTAMP") {
        Some(ts) => ts,
        None => "unknown",
    }
};

/// The cargo features (set by build.rs if available).
pub const CARGO_FEATURES: &str = {
    match option_env!("VERGEN_CARGO_FEATURES") {
        Some(f) => f,
        None => "default",
    }
};

/// The long version information for Vertex (lazy static for runtime access).
pub static LONG_VERSION: once_cell::sync::Lazy<String> =
    once_cell::sync::Lazy::new(|| format!("Version: {}\nCommit SHA: {}", VERSION, GIT_SHA));

/// The user agent string for network communication.
pub const USER_AGENT: &str = concat!("vertex/", env!("CARGO_PKG_VERSION"));

/// The version information for libp2p identification.
pub const P2P_CLIENT_VERSION: &str = concat!("vertex/v", env!("CARGO_PKG_VERSION"));
