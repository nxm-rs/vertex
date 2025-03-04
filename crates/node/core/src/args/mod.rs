/// NeighbourhoodArgs struct for configuring the node's neighbourhood
mod neighbourhood;
use clap::Parser;
pub use neighbourhood::NeighbourhoodArgs;

/// NetworkArgs struct for configuring the network
mod network;
pub use network::NetworkArgs;

/// WalletArgs struct for configuring the wallet
mod wallet;
pub use wallet::WalletArgs;

#[derive(Debug, Parser)]
pub struct NodeCommand {
    /// All network related arguments
    #[command(flatten)]
    pub network: NetworkArgs,

    /// All wallet related arguments
    #[command(flatten)]
    pub wallet: WalletArgs,

    /// All neighbourhood related arguments
    #[command(flatten)]
    pub neighbourhood: NeighbourhoodArgs,
}
