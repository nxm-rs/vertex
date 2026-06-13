# Security analysis: unified chain indexer (`vertex-chain-index-contracts`)

This is the implementor's honest defence of the seven security surfaces from the unified-indexer design, each pointed at the concrete `file:function` that defends it, with the partial or deferred cases called out rather than hidden. The assessor reading the pushed branch should attack these claims directly.

All paths are under `crates/chain/index-contracts/src/`.

## 1. Unknown / malformed event decode

Risk: a malformed event body that the per-branch indexers would hit at `apply` (decode -> `IndexError::Apply` -> stops the run loop -> re-pages the same range forever) now affects a shared indexer; a panic or hard error in `apply` would stall ALL contracts at once.

Defence: `indexer.rs::ContractIndexer::apply` never decodes the event body for the verbatim store path. It computes `stored_event_from_log` (`store.rs::stored_event_from_log`), which copies `topics` and `data` byte-for-byte with no ABI parsing, and writes them. A malformed body therefore cannot panic or error the hot path. The only `apply`-time error is a missing `log_index` (`IndexError::MalformedLog`), which a canonical finalized log always carries. Decoding happens only in `views/*`, where each `decode_log_data` returns a `Result` that the view folds skip on `Err` (for example `views/postage.rs::chain_state` does `if let Ok(decoded) = abi::PriceUpdate::decode_log_data(...)`), so a decode failure is scoped to one read and never wedges the cursor.

Partial / honest note: there is ONE place `apply` does decode, in `indexer.rs::decode_batch_update`, to feed the typed postage batch projection that backs the value-sorted index. That decode is non-fatal: it returns `None` on any decode miss, the verbatim row is still written first (`put_event` runs before `apply_batch_update`), and the index is a self-healing hint the reserve recomputes at dequeue. So even this decode cannot wedge the cursor or corrupt the source of truth; the worst case is a stale ordering hint, which the reserve already tolerates by design.

## 2. Log ordering / `(block, log_index)` monotonicity

Risk: views fold position-ordered rows and take last-write-wins; a provider returning out-of-order or duplicated logs could corrupt a derived value.

Defence: the store key IS the position. `store.rs::EventKey` encodes `[contract_tag u8][block u64 BE][log_index u64 BE]` (`EventKey::encode`), so redb's btree returns rows in canonical `(contract, block, log_index)` order regardless of provider response order, and a duplicate log overwrites its own slot in place (`store.rs::put_event` is a plain `put`, idempotent by key). `store.rs::events_of` additionally `sort_by`s the filtered rows defensively so a view never depends on backend iteration order. The engine also sorts each page by `(block_number, log_index)` before `apply` (engine, unchanged). Monotonicity is structural, not a hand-rolled supersede guard, which is the single biggest simplification over the branches.

## 3. Untrusted field values stored then folded

Risk: the store holds attacker-influenceable event fields (balances, depths, overlays) verbatim; a view folds them into a decision (batch valid, overlay staked).

Defence: the store is a faithful mirror of finalized on-chain logs from the canonical contract addresses and asserts nothing about meaning (`store.rs::StoredEvent` doc: "this struct asserts nothing about meaning"). Every safety-critical interpretation stays at the consumer. Stamp validity is recomputed at the decision point from the rising line an attacker cannot forge without paying the contract: `views/postage.rs::is_batch_valid_now` computes `normalised_balance > chain_state.current_total_out_payment(block)`, where `current_total_out_payment` (`views/postage.rs::ChainState::current_total_out_payment`) reconstructs the contract's own accumulator from the `PriceUpdate` cadence. Critically, stamp signature and owner recovery are NOT in this crate at all; they stay in nectar primitives (`signing.rs` upstream). The indexer records facts; it grants no trust.

Honest note: the typed `BatchState` row (`store.rs::BatchState`) does carry attacker-influenceable fields (balance, depth) that feed the value-sorted index ordering. That is acceptable because the index is only an ordering hint: `views/postage.rs::eviction_candidates` returns candidates, and the reserve must recompute true validity with `is_batch_valid_now` at dequeue (the chain-reactions contract). An attacker who games the index order only changes the order in which the reserve checks batches, not which batches are actually evicted.

## 4. Address / topic confusion across contracts sharing the table

Risk: one combined filter matches any watched topic0 at any watched address; two contracts could share a topic0 (in this registry postage's `PriceUpdate(uint256)`, swap's `PriceUpdate(uint256)`, and the storage price oracle's `PriceUpdate(uint256)` all share one `topic0`), and a row could be misfiled under the wrong `ContractId`, poisoning that contract's view.

Defence: address is the authority, not topic0. `indexer.rs::ContractIndexer::apply` first calls `resolve(log.address())` (`indexer.rs::ContractIndexer::resolve`, a `HashMap<Address, WatchedContract>` lookup); a log from an unwatched address is skipped (`unwatched_address_is_skipped` test). It then verifies the topic0 is declared for the resolved contract via `registry.rs::WatchedContract::declares`; a topic0 not in that contract's descriptor set is skipped (`topic_not_declared_for_resolved_contract_is_skipped` test). The `EventKey` is namespaced by `ContractId` (`store.rs::EventKey.contract`), so even a hypothetical misresolution cannot cross-contaminate two contracts' key ranges. The stored row also carries the emitting `address` (`store.rs::StoredEvent.address`) as a redundant cross-check a view can assert.

Honest note: the three real `PriceUpdate(uint256)` collisions in the registry share a topic0 but live at three different addresses, so the address resolution files each correctly and the topic0 check passes for each (each contract genuinely declares that topic0). The defence is exercised, not bypassed, by this case: the test `topic_not_declared_for_resolved_contract_is_skipped` uses a genuinely-foreign topic0 (chequebook `SimpleSwapDeployed` at the postage address) to prove the reject path.

## 5. Decode-time DoS (huge data fields)

Risk: a contract could emit an event with a very large non-indexed `data` blob; storing it verbatim consumes disk, and a view decode allocates it.

Defence: `topics` is bounded by the EVM (<= 4), inherited from the log. `data` size is capped at `apply`: `store.rs::put_event` returns `Ok(false)` (skip, not error) when `event.data.len() > MAX_EVENT_DATA` (`store.rs::MAX_EVENT_DATA = 8 KiB`); `indexer.rs::ContractIndexer::apply` logs the skip with `warn!` and returns `Ok(())`, so an oversized log neither wedges the cursor nor lands on disk. The `oversized_data_is_capped` test asserts both (no error, not stored). Every watched event has a small fixed-size payload, so a real event never trips the cap.

Honest note: the cap is a fixed 8 KiB for all events rather than a per-event exact size. This is deliberate (it tolerates ABI evolution) but means a 7 KiB junk blob at a watched address with a declared topic0 would still be stored. The disk cost is bounded (8 KiB/log) and the view decode of such a row simply fails and is skipped, so the residual risk is bounded disk growth under a sustained malicious-but-finalized log stream, which a finalized-only indexer already rate-limits to real on-chain activity the attacker pays gas for.

## 6. Reorg-revert correctness on the shared table

Risk: a buggy revert that under-deletes leaves stale rows that poison every view; over-deletes lose canonical data. The branches dodge this by never reverting (finalized-only).

Defence: `indexer.rs::ContractIndexer::revert` calls `store.rs::revert_contract` per `ContractId`, which range-deletes every `EventTable` row with `k.contract == contract && k.block >= from_block`, and for postage also drops batch-projection rows whose `start_block >= from_block` (with their index entries via `delete_indexed`). Because every view derives purely from raw rows (`views/*` all fold `events_of`), deleting the reorged-out range is necessary and sufficient: no view holds independent state revert could miss. The `revert_range_deletes_per_contract` test proves the block-100 row survives while block-200 is dropped. The MVP engine indexes finalized-only and never calls `revert`, so this is correct-by-construction today and correct-by-design when head-tracking arrives.

Honest deviations from the design text:
- The design specified `revert` as a `DbCursorRW` `seek`+`delete_current` walk. The actual `vertex-storage` trait surface (`crates/storage/src/traits.rs`) exposes `DbCursorRO`/`DbCursorRW` as traits but the redb backend provides no cursor-factory method on a transaction, so `revert_contract` instead reads `entries::<EventTable>()` (ordered) and issues keyed `delete`s. Same effect, same range, O(n) over the table rather than O(reverted) over the range; when a cursor factory lands this becomes the bounded walk. This is a performance, not a correctness, difference.
- Postage revert by `start_block` is coarse: a batch created before `from_block` but topped-up inside the reverted range keeps its pre-revert `BatchState` row (the topup's balance bump is NOT rolled back in the typed projection). This is safe because the verbatim `EventTable` rows for that batch in the reverted range ARE deleted, so the authoritative `views/postage.rs::batch` fold recomputes the correct post-revert balance on read, and the next forward `apply` of the re-finalized range re-bumps the index. The index hint can thus be transiently stale across a reorg, which the reserve's dequeue-time recompute already absorbs. The verbatim store (the source of truth) is always exactly correct after revert.

## 7. Where validation must still live

Risk: centralizing decode could tempt moving trust decisions into the indexer.

Defence: it does not. The crate has no dependency on any storer, accounting, postage-issuer, or redistribution-agent trait (`Cargo.toml` deps are only `vertex-chain-index`, `vertex-storage`, alloy, nectar-contracts, serde, strum, thiserror, tracing). `apply` writes rows and returns; it calls no domain trait. The views answer queries and return values; they decide nothing. Stamp signature / owner recovery is absent here (nectar primitives own it). Cheque validity reads `views/chequebook.rs::is_factory_deployed`; stake-frozen / round-commit gating reads `views/staking.rs` and `views/redistribution.rs`; batch eviction reads `views/postage.rs::eviction_candidates`; in every case the consumer decides. The engine/domain boundary from `CHAIN_REACTIONS_DESIGN.md` holds: the chain crate announces "advanced" (the engine's `watch` notifier, landing with the reserve); it never calls a domain trait.

## Cursor single-point-of-failure (concentration note)

One cursor for all contracts means a poison page from any contract stalls indexing for all. This is an availability, not integrity, concern and is acceptable: a page that fails to commit (storage error) is retried by the engine; a page that "cannot decode" does not exist as a failure mode here, because the verbatim store never decodes at `apply`. The only `apply` error path is `log_index`-missing (`indexer.rs::apply`), which a canonical finalized log never hits. A storage-layer failure stalls the node's indexing wholesale, which is the correct fail-stop: a node with a broken store should not make stamp/stake decisions on stale data.
