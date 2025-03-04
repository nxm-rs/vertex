use clap::{arg, command, Args};
use libp2p::Multiaddr;

use crate::version::P2P_CLIENT_VERSION;

/// Parameters for configuring the network
#[derive(Debug, Clone, Args, PartialEq, Eq)]
#[command(next_help_heading = "Networking")]
pub struct NetworkArgs {
    /// Custom node identity
    #[arg(long, value_name = "IDENTITY", default_value = P2P_CLIENT_VERSION)]
    pub identity: String,

    /// Comma separated multiaddrs of trusted peers for P2P connections.
    ///
    /// --trusted-peers /ip4/123.123.123.123/tcp/1234/p2p/PeerID
    #[arg(long, value_delimiter = ',')]
    pub trusted_peers: Vec<Multiaddr>,

    /// Connect to or accept from trusted peers only
    #[arg(long, default_value_t = false)]
    pub trusted_only: bool,

    /// Comma separated Multiaddr URLs for P2P discovery bootstrap.
    ///
    /// --bootstrap-peers /ip4/123.123.123.123/tcp/1234/p2p/PeerID
    #[arg(long, value_delimiter = ',')]
    pub bootnodes: Vec<Multiaddr>,

    /// Welcome message shown to peers on connection
    #[arg(
        long,
        value_name = "WELCOME_MESSAGE",
        default_value = "Vertex into the Swarm!"
    )]
    pub welcome_message: String,
}
