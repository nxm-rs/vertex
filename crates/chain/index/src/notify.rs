//! Two `watch` push affordances: the block-tip clock and the post-commit nudge.
//!
//! Both are last-value-wins `tokio::sync::watch` channels (a lagging subscriber
//! never blocks the engine) that start at `None` until the first publish.
//! [`BlockTip`] fires on every finalized-head advance, including a head that
//! indexed zero logs (the block clock). [`IndexAdvance`] fires only after a
//! page's `tx.commit()` succeeds (a wakeup to re-query the committed projection).

use alloy_primitives::B256;
use tokio::sync::watch;

/// A finalized block the engine observed: `(number, hash)`, no domain payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockTip {
    /// The finalized block number.
    pub number: u64,
    /// The finalized block hash.
    pub hash: B256,
}

/// Receiver of the block-tip clock; `None` until the first finalized head.
pub type BlockTipRx = watch::Receiver<Option<BlockTip>>;

/// A post-commit nudge: which indexer advanced, and to which committed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexAdvance {
    /// The name of the indexer whose cursor just committed.
    pub indexer: &'static str,
    /// The last block committed by the page that triggered this nudge.
    pub last_block: u64,
}

/// Receiver of the post-commit nudge; `None` until the first committed page.
pub type IndexAdvanceRx = watch::Receiver<Option<IndexAdvance>>;
