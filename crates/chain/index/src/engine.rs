//! The [`EventEngine`]: the reorg/cursor/paging story, written once.
//!
//! One engine instance drives one [`Indexer`] against one [`ChainReader`] and
//! one `vertex-storage` [`Database`]. It backfills the indexer's filter from the
//! deployment block (or the persisted cursor) up to the chain's finalized head,
//! then follows the head and indexes each newly-finalized range the same way.
//!
//! # Reorg strategy
//!
//! Safety keys off the chain's `finalized` tag, not a fixed block lag. The
//! engine never applies a log above the finalized head, and finalized blocks do
//! not reorg, so the backfill needs no rollback logic and the cursor never has
//! to walk backwards. The latency cost is one finality window (minutes on
//! Gnosis); the benefit is that the entire indexed range is canonical by
//! construction.
//!
//! Optimistic head-tracking (indexing the `finalized..latest` window and
//! reverting on the WS `log.removed` flag or a parent-hash mismatch) is a
//! documented enhancement, not implemented here. The hooks for it already exist:
//! [`Indexer::revert`] and the block hash stored in the [`Cursor`]. The MVP
//! leaves `revert` a default no-op and never calls it.
//!
//! # Atomicity and idempotency
//!
//! A page's cursor is written and committed in one `vertex-storage` write
//! transaction. The cursor advances only on a clean commit, so it never outruns
//! a page that failed to apply: an [`Indexer::apply`] error aborts the page
//! before the cursor moves, and the failed range is retried on the next run.
//!
//! The [`Indexer`] trait folds state through `apply` without taking the engine's
//! write transaction, so an indexer that persists into its own
//! [`Database`](vertex_storage::Database) commits separately from the cursor. The
//! engine bridges that gap with idempotency rather than a shared transaction: a
//! crash after an indexer commit but before the cursor commit re-delivers the
//! same finalized range on restart, and because the range is canonical and
//! cannot have changed, re-applying it must be a no-op for a correctly-written
//! indexer. The MVP keeps the cursor commit last so the worst case is replay,
//! never a skipped range. A shared-transaction variant (an `apply` that takes
//! the engine's `DbTxMut`) is a possible future tightening, not the MVP.
//!
//! [`Indexer`]: crate::Indexer
//! [`Indexer::revert`]: crate::Indexer::revert
//! [`ChainReader`]: crate::ChainReader

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use alloy_rpc_types_eth::Log;
use tokio::sync::watch;
use tracing::{debug, info, warn};
use vertex_storage::{Database, DbTxMut, Tables};

use crate::cursor::{Cursor, CursorTables};
use crate::metrics;
use crate::notify::{BlockTip, BlockTipRx, IndexAdvance, IndexAdvanceRx};
use crate::reader::{ChainReader, FinalizedHead};
use crate::{IndexError, Indexer};

/// The largest block span requested in a single `eth_getLogs` page.
///
/// Public RPC providers cap log queries by block range and by result count;
/// this is a conservative starting span that the adaptive loop shrinks on a
/// provider range/limit error.
pub const DEFAULT_PAGE_SIZE: u64 = 10_000;

/// The smallest the adaptive page span is allowed to shrink to.
///
/// A single block always fits, so the floor is one; the engine returns the
/// provider error rather than looping forever if even one block is rejected.
const MIN_PAGE_SIZE: u64 = 1;

/// How often the follow loop re-checks the finalized head.
///
/// Gnosis produces a block roughly every five seconds and finalizes in minutes,
/// so polling the finalized tag this often keeps follow latency well inside one
/// block of the finality window without hammering the RPC.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Builder for an [`EventEngine`]: holds the reader and store before an indexer
/// is attached.
///
/// [`EventEngine::new`] returns this; [`register`](EventEngineBuilder::register)
/// attaches the indexer and produces a runnable [`EventEngine`]. Splitting the
/// builder out makes "an engine without an indexer" unrepresentable, so
/// [`run`](EventEngine::run) never has to handle a missing indexer.
pub struct EventEngineBuilder<R, DB> {
    reader: Arc<R>,
    db: Arc<DB>,
    /// The block-tip clock sender. Always present (the head clock is cheap and
    /// always-on); the engine republishes the finalized head on every advance.
    block_tip: watch::Sender<Option<BlockTip>>,
    /// The post-commit nudge sender, set only when [`with_notifier`] is called.
    ///
    /// [`with_notifier`]: EventEngineBuilder::with_notifier
    index_advance: Option<watch::Sender<Option<IndexAdvance>>>,
}

impl<R, DB> EventEngineBuilder<R, DB>
where
    R: ChainReader + 'static,
    DB: Database,
{
    /// Subscribe to the raw head clock.
    ///
    /// Observes `Some(BlockTip { number, hash })` on every finalized-head
    /// advance, including a head that indexed zero logs. Always available (a
    /// cheap, always-on `watch`); starts at `None` until the first finalized head.
    pub fn block_tip(&self) -> BlockTipRx {
        self.block_tip.subscribe()
    }

    /// Opt into the post-commit projection nudge.
    ///
    /// Returns the builder (to keep the fluent chain) paired with an
    /// [`IndexAdvanceRx`] that observes `Some(IndexAdvance { indexer, last_block })`
    /// only after each page's `tx.commit()` succeeds, never on an empty block.
    /// Starts at `None`; calling it again replaces the single sender.
    pub fn with_notifier(mut self) -> (Self, IndexAdvanceRx) {
        let (tx, rx) = watch::channel(None);
        self.index_advance = Some(tx);
        (self, rx)
    }

    /// Attach the indexer this engine drives, producing a runnable engine.
    ///
    /// One engine instance drives one indexer with its own cursor; a
    /// combined-filter multi-indexer mode is an explicit future optimization,
    /// not the MVP.
    pub fn register(self, indexer: Arc<dyn Indexer>) -> EventEngine<R, DB> {
        EventEngine {
            reader: self.reader,
            db: self.db,
            indexer,
            page_size: DEFAULT_PAGE_SIZE,
            poll_interval: DEFAULT_POLL_INTERVAL,
            block_tip: self.block_tip,
            index_advance: self.index_advance,
        }
    }
}

/// Drives a single [`Indexer`] over a [`ChainReader`] with a persisted cursor.
///
/// Build with [`EventEngine::new`] then [`register`](EventEngineBuilder::register)
/// an indexer, then [`run`](EventEngine::run) until the supplied shutdown future
/// resolves.
pub struct EventEngine<R, DB> {
    reader: Arc<R>,
    db: Arc<DB>,
    indexer: Arc<dyn Indexer>,
    page_size: u64,
    poll_interval: Duration,
    /// The block-tip clock sender; republished on every finalized-head advance.
    block_tip: watch::Sender<Option<BlockTip>>,
    /// The post-commit nudge sender, present only when the builder opted in via
    /// [`EventEngineBuilder::with_notifier`].
    index_advance: Option<watch::Sender<Option<IndexAdvance>>>,
}

impl<R, DB> EventEngine<R, DB>
where
    R: ChainReader + 'static,
    DB: Database,
{
    /// Begin building an engine over a chain reader and a cursor store.
    ///
    /// The reader is any `alloy_provider::Provider<Ethereum>` (or a test double);
    /// the database is any `vertex-storage` [`Database`]. Attach an indexer with
    /// [`register`](EventEngineBuilder::register) to get a runnable engine.
    ///
    /// Returns the builder rather than `Self` so an engine cannot exist without
    /// an indexer; the `EventEngine::new(...).register(...)` chain reads as one
    /// fluent construction.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(reader: Arc<R>, db: Arc<DB>) -> EventEngineBuilder<R, DB> {
        // The block-tip clock is always-on: a `watch` with no live receivers is
        // a cheap atomic store, so holding the sender unconditionally costs
        // nothing and keeps `block_tip()` available without an opt-in.
        let (block_tip, _) = watch::channel(None);
        EventEngineBuilder {
            reader,
            db,
            block_tip,
            index_advance: None,
        }
    }

    /// Override the initial `eth_getLogs` page span. Mainly for tests.
    pub fn with_page_size(mut self, page_size: u64) -> Self {
        self.page_size = page_size.max(MIN_PAGE_SIZE);
        self
    }

    /// Override the finalized-head poll interval. Mainly for tests.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Run the engine until `shutdown` resolves.
    ///
    /// Initializes the cursor table, backfills to the current finalized head,
    /// then follows: each time the finalized head advances, the newly-finalized
    /// range is indexed the same way. Returns when the shutdown future resolves
    /// or when an indexer callback errors.
    ///
    /// The initial catch-up runs to completion before the follow loop, so the
    /// engine has fully backfilled the chain as it stood at startup before it
    /// begins waiting on the shutdown signal. Within the follow loop the shutdown
    /// future is checked between sync passes, so a shutdown lands on a page
    /// boundary and never tears a page (each page commits or rolls back whole).
    pub async fn run<F>(mut self, shutdown: F) -> Result<(), IndexError>
    where
        F: Future<Output = ()>,
    {
        let indexer = Arc::clone(&self.indexer);
        let name = indexer.name();

        CursorTables::init(self.db.as_ref())?;

        info!(indexer = name, "starting chain event engine");

        // Backfill the chain as it stands at startup before entering the follow
        // loop. This makes one full catch-up a guaranteed, shutdown-independent
        // step rather than racing the shutdown future on the first poll.
        self.sync_once(indexer.as_ref()).await?;

        let follow = self.follow(indexer.as_ref());
        tokio::pin!(follow);
        tokio::pin!(shutdown);

        tokio::select! {
            result = &mut follow => result,
            () = &mut shutdown => {
                info!(indexer = name, "chain event engine shutting down");
                Ok(())
            }
        }
    }

    /// The follow loop: re-check the finalized head each interval and index any
    /// newly-finalized range.
    async fn follow(&mut self, indexer: &dyn Indexer) -> Result<(), IndexError> {
        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; consume it so the initial wait is a
        // full interval (the startup catch-up already covered the current head).
        ticker.tick().await;

        loop {
            ticker.tick().await;
            self.sync_once(indexer).await?;
        }
    }

    /// Fetch the finalized head and index up to it, if the chain has finalized.
    async fn sync_once(&mut self, indexer: &dyn Indexer) -> Result<(), IndexError> {
        match self.reader.finalized_head().await? {
            Some(head) => self.sync_to(indexer, head).await,
            None => {
                debug!(indexer = indexer.name(), "chain has no finalized head yet");
                Ok(())
            }
        }
    }

    /// Test-only access to [`sync_to`](Self::sync_to) so a unit test can drive a
    /// single sync against a chosen head without spinning the follow loop.
    #[cfg(test)]
    pub(crate) async fn sync_to_for_test(
        &mut self,
        indexer: &dyn Indexer,
        head: FinalizedHead,
    ) -> Result<(), IndexError> {
        self.sync_to(indexer, head).await
    }

    /// Index every page from the cursor (or start block) up to `head`.
    async fn sync_to(
        &mut self,
        indexer: &dyn Indexer,
        head: FinalizedHead,
    ) -> Result<(), IndexError> {
        let name = indexer.name();

        // Publish the raw head clock on every finalized-head advance, before any
        // paging: the tick must fire even if the head's range indexes zero logs.
        // Skip a republish when the head has not moved so a no-op poll does not
        // churn the channel. `send_if_modified` only marks the value changed
        // (waking subscribers' `changed()`) when the head number actually
        // advanced; a lagging receiver still converges to the latest value.
        self.block_tip.send_if_modified(|current| {
            let advanced = current.is_none_or(|tip| tip.number < head.number);
            if advanced {
                *current = Some(BlockTip {
                    number: head.number,
                    hash: head.hash,
                });
            }
            advanced
        });

        // Resume from one past the last applied block, but never before the
        // contract's deployment block.
        let resume = match Cursor::load(self.db.as_ref(), name)? {
            Some(cursor) => cursor.next_block(),
            None => indexer.start_block(),
        }
        .max(indexer.start_block());

        let mut from = resume;
        while from <= head.number {
            let requested_to = self.page_end(from, head.number);
            // `fetch_page` may shrink the page, so checkpoint and advance by the
            // range it actually covered, not the one requested.
            let (to, logs) = self.fetch_page(indexer, from, requested_to).await?;

            // The page's checkpoint hash is the finalized head hash when the
            // page reaches the head, otherwise unknown (zero) for an interior
            // page; only the head boundary needs a verifiable hash for future
            // head-tracking, and interior finalized blocks never reorg.
            let block_hash = if to == head.number {
                head.hash
            } else {
                alloy_primitives::B256::ZERO
            };

            self.commit_page(indexer, &logs, to, block_hash)?;

            metrics::pages_total(name).increment(1);
            debug!(
                indexer = name,
                from,
                to,
                logs = logs.len(),
                "applied chain log page"
            );

            from = to.saturating_add(1);
        }

        Ok(())
    }

    /// The inclusive end block for a page starting at `from`, clamped to `head`.
    fn page_end(&self, from: u64, head: u64) -> u64 {
        from.saturating_add(self.page_size.saturating_sub(1))
            .min(head)
    }

    /// Fetch one page of logs, shrinking the page span and retrying on a
    /// provider range/limit error.
    ///
    /// Returns the logs together with the block the page actually ended at: a
    /// shrunk page covers a narrower range than requested, and the caller must
    /// checkpoint and advance by the covered range, not the requested one.
    async fn fetch_page(
        &mut self,
        indexer: &dyn Indexer,
        from: u64,
        mut to: u64,
    ) -> Result<(u64, Vec<Log>), IndexError> {
        loop {
            let filter = indexer.filter().from_block(from).to_block(to);
            match self.reader.get_logs(&filter).await {
                Ok(logs) => return Ok((to, logs)),
                Err(IndexError::Transport(err)) if is_range_error(&err) && to > from => {
                    // Halve the span (round up so it always shrinks) and retry.
                    let span = (to - from).div_ceil(2);
                    to = from.saturating_add(span.saturating_sub(1)).max(from);
                    self.page_size = self.page_size.div_ceil(2).max(MIN_PAGE_SIZE);
                    metrics::PAGE_SHRINKS_TOTAL.increment(1);
                    warn!(
                        indexer = indexer.name(),
                        from, to, "shrinking log page after provider range error"
                    );
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Apply a page's logs in order, then checkpoint the cursor.
    ///
    /// The cursor write commits last, in its own transaction, so it advances only
    /// after every log in the page applied cleanly; an `apply` error returns
    /// before the cursor moves and the page is retried on the next run. See the
    /// module-level atomicity note for why the indexer's own state and the cursor
    /// need not share one transaction.
    fn commit_page(
        &self,
        indexer: &dyn Indexer,
        logs: &[Log],
        last_block: u64,
        block_hash: alloy_primitives::B256,
    ) -> Result<(), IndexError> {
        let name = indexer.name();

        // Order by (block_number, log_index). A single eth_getLogs response is
        // already canonical-ordered, but sorting makes the contract explicit and
        // tolerates a provider that does not guarantee it.
        let mut ordered: Vec<&Log> = logs.iter().collect();
        ordered.sort_by_key(|log| (log.block_number, log.log_index));

        // Apply every log, then commit the cursor. The cursor commit is last, so
        // a failure mid-page leaves the cursor where it was and the page replays.
        let tx = self.db.tx_mut()?;
        for log in ordered {
            let block = log.block_number.ok_or(IndexError::MalformedLog {
                field: "block_number",
            })?;
            indexer.apply(block, log)?;
            metrics::logs_total(name).increment(1);
        }
        Cursor {
            last_block,
            block_hash,
        }
        .write(&tx, name)?;
        tx.commit()?;

        metrics::checkpoints_total(name).increment(1);

        // Post-commit nudge: only after `tx.commit()` succeeded, so consumers
        // never see an advance the cursor has not yet recorded, and a `?`-bailed
        // page above this point never fires. `send_replace` is last-value-wins:
        // a replay republishes the same value (a harmless no-op for consumers).
        if let Some(notifier) = &self.index_advance {
            notifier.send_replace(Some(IndexAdvance {
                indexer: name,
                last_block,
            }));
        }

        Ok(())
    }
}

/// Whether a transport error is a provider range/limit rejection worth shrinking
/// the page for.
///
/// Providers signal "too many results" or "block range too wide" as a JSON-RPC
/// error response; the exact code and text vary, so this matches the common
/// phrasings rather than a single code. A non-`ErrorResp` transport failure (a
/// dropped connection) is not a range error and propagates.
fn is_range_error(err: &alloy_provider::transport::TransportError) -> bool {
    if !err.is_error_resp() {
        return false;
    }
    let text = err.to_string().to_ascii_lowercase();
    const RANGE_HINTS: &[&str] = &[
        "range",
        "too many",
        "limit",
        "exceed",
        "more than",
        "query returned more than",
        "block range",
        "response size",
    ];
    RANGE_HINTS.iter().any(|hint| text.contains(hint))
}
