//! Chunk retrieval CLI arguments.
//!
//! Controls the verification applied to chunks downloaded from the network.
//! Content integrity is already enforced when the retrieval codec reconstructs a
//! chunk against the requested address, so the residual checks here are cheap: a
//! defensive address re-assertion (on by default) and postage stamp signer
//! recovery (off by default).

use clap::Args;
use serde::{Deserialize, Serialize};

/// Default for the download-side content (address) re-assertion.
const DEFAULT_VERIFY_CONTENT: bool = true;
/// Default for the download-side postage stamp signer recovery.
const DEFAULT_VERIFY_STAMP: bool = false;

/// Chunk retrieval verification arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Retrieval")]
#[serde(default)]
pub struct RetrievalArgs {
    /// Re-assert that a downloaded chunk's address matches the request.
    #[arg(long = "retrieval.verify-content", default_value_t = DEFAULT_VERIFY_CONTENT)]
    pub verify_content: bool,

    /// Recover the postage stamp signer for downloaded chunks.
    #[arg(long = "retrieval.verify-stamp", default_value_t = DEFAULT_VERIFY_STAMP)]
    pub verify_stamp: bool,
}

impl Default for RetrievalArgs {
    fn default() -> Self {
        Self {
            verify_content: DEFAULT_VERIFY_CONTENT,
            verify_stamp: DEFAULT_VERIFY_STAMP,
        }
    }
}
