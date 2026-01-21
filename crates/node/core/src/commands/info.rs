//! Info command - Display node information

use crate::cli::InfoArgs;
use eyre::Result;
use tracing::info;

/// Run the info command
pub async fn run(args: InfoArgs) -> Result<()> {
    let show_all = args.all;

    if show_all || args.network {
        info!("Network Information:");
        // TODO: Display network info (peer count, connections, etc.)
    }

    if show_all || args.storage {
        info!("Storage Information:");
        // TODO: Display storage info (capacity, usage, chunk count)
    }

    if show_all || args.peers {
        info!("Peer Information:");
        // TODO: Display peer list
    }

    if !args.network && !args.storage && !args.peers && !args.all {
        info!("Use --all, --network, --storage, or --peers to display information");
    }

    Ok(())
}
