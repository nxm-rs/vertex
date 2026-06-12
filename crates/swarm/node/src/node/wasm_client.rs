//! Deprecated browser client launch entrypoint.
//!
//! The browser launch path lives in [`super::launch`] now; [`launch_client`]
//! remains as a thin wrapper so existing callers keep compiling while they
//! migrate to [`ClientLauncher`].

use eyre::Result;
use libp2p::Multiaddr;
use vertex_swarm_identity::Identity;
use vertex_swarm_topology::TopologyHandle;

use super::launch::ClientLauncher;

/// Build and start a browser client node, returning its topology handle.
///
/// Thin wrapper over [`ClientLauncher`] kept for callers that predate it. The
/// returned handle keeps the client alive for the session; dropping it does
/// not stop the spawned tasks, which run until the page is torn down.
///
/// # Errors
///
/// Returns an error if the swarm fails to assemble (transport or behaviour
/// construction).
#[deprecated(note = "use ClientLauncher")]
pub async fn launch_client(
    identity: Identity,
    bootnodes: Vec<Multiaddr>,
) -> Result<TopologyHandle<Identity>> {
    let launched = ClientLauncher::new(identity)
        .with_bootnodes(bootnodes)
        .launch()
        .await?;
    Ok(launched.topology().clone())
}
