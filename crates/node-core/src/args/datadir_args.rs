//! clap [Args](clap::Args) for datadir config

use crate::dirs::{SwarmPath, DataDirPath, MaybePlatformPath};
use clap::Args;
use beers_primitives::Swarm;
use std::path::PathBuf;

/// Parameters for datadir configuration
#[derive(Debug, Args, PartialEq, Eq, Default, Clone)]
#[command(next_help_heading = "Datadir")]
pub struct DatadirArgs {
    /// The path to the data dir for all beers files and subdirectories.
    ///
    /// Defaults to the OS-specific data directory:
    ///
    /// - Linux: `$XDG_DATA_HOME/beers/` or `$HOME/.local/share/beers/`
    /// - Windows: `{FOLDERID_RoamingAppData}/beers/`
    /// - macOS: `$HOME/Library/Application Support/beers/`
    #[arg(long, value_name = "DATA_DIR", verbatim_doc_comment, default_value_t)]
    pub datadir: MaybePlatformPath<DataDirPath>,

    /// The absolute path to store static files in.
    #[arg(long = "datadir.static_files", verbatim_doc_comment, value_name = "PATH")]
    pub static_files_path: Option<PathBuf>,
}

impl DatadirArgs {
    /// Resolves the final datadir path.
    pub fn resolve_datadir(self, swarm: Swarm) -> SwarmPath<DataDirPath> {
        let datadir = self.datadir.clone();
        datadir.unwrap_or_swarm_default(swarm, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// A helper type to parse Args more easily
    #[derive(Parser)]
    struct CommandParser<T: Args> {
        #[command(flatten)]
        args: T,
    }

    #[test]
    fn test_parse_datadir_args() {
        let default_args = DatadirArgs::default();
        let args = CommandParser::<DatadirArgs>::parse_from(["beers"]).args;
        assert_eq!(args, default_args);
    }
}
