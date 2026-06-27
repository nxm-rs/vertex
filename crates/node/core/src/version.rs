//! Version information for the Vertex Swarm node.
//!
//! The single canonical version source: `build.rs` stamps the short commit sha
//! into `VERGEN_GIT_SHA`, and everything operator- or peer-facing (the
//! `--version` string and the libp2p identify agent string) derives from here.

use std::sync::LazyLock;

/// Package version from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit sha stamped by `build.rs`, or `unknown` outside a checkout
/// (the Docker build context excludes `.git`).
pub const GIT_SHA: &str = match option_env!("VERGEN_GIT_SHA") {
    Some(sha) => sha,
    None => "unknown",
};

/// `--version` string: package version plus the short commit sha, e.g.
/// `0.1.0 (abc1234)`. Degrades to `0.1.0 (unknown)` outside a git checkout.
pub static LONG_VERSION: LazyLock<String> = LazyLock::new(|| format!("{VERSION} ({GIT_SHA})"));

/// libp2p identify agent string. Carries the build sha as `vertex/<version>-<sha>`
/// so a peer can attribute behaviour to an exact build; a git-less build drops the
/// suffix to a bare `vertex/<version>` rather than announcing `-unknown`.
pub static AGENT_VERSION: LazyLock<String> = LazyLock::new(|| {
    if GIT_SHA == "unknown" {
        format!("vertex/{VERSION}")
    } else {
        format!("vertex/{VERSION}-{GIT_SHA}")
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_version_carries_the_sha() {
        // The workspace builds inside a git checkout, so the sha must resolve and
        // flow into the announced agent string.
        assert_ne!(GIT_SHA, "unknown", "build.rs did not stamp VERGEN_GIT_SHA");
        let agent = AGENT_VERSION.as_str();
        assert!(agent.starts_with("vertex/"));
        assert!(
            agent.contains(GIT_SHA),
            "agent version {agent} is missing sha {GIT_SHA}"
        );
        assert_eq!(agent, format!("vertex/{VERSION}-{GIT_SHA}"));
    }
}
