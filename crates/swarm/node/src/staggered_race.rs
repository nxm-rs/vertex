//! Staggered candidate race: a reusable any-candidate race that resolves on the
//! first success.
//!
//! Some requests have several interchangeable candidates, any of which can
//! satisfy the request. Trying them strictly in series lets one slow or
//! withholding head candidate stall the whole request for a full per-attempt
//! deadline, and where the caller streams a sequence of such requests that
//! head-of-line stall blocks every later item behind it.
//!
//! [`race_candidates`] instead queries the best candidate immediately and adds
//! each further candidate after a caller-supplied `stagger` tick (or as soon as
//! an earlier attempt fails), resolving on the first success. Staggering bounds
//! the fan-out cost: where each raced attempt is itself costly (a metered
//! network request, say), further candidates only start while no response has
//! arrived, so a withholding head candidate is overtaken by the next candidate
//! within the stagger instead of within the per-attempt deadline.
//!
//! The losing attempts are dropped the moment the race resolves: returning from
//! the race drops every future still in the [`FuturesUnordered`], so any
//! resource a losing attempt holds (a reservation, a forwarder slot) is released
//! on drop. The per-candidate closure carries its own pacing, so staggered
//! starts preserve any per-candidate throttling that a single all-at-once
//! fan-out would skip.
//!
//! # Read-style races only
//!
//! This helper is for read-style, any-peer races where every candidate answers
//! the same immutable question and the first answer wins. Chunk retrieval is the
//! canonical caller, with [`RETRIEVAL_STAGGER`] as its stagger. It must NOT be
//! applied to directed-write paths such as pushsync: fanning out a write makes
//! several peers take redundant custody of the same chunk and multiplies the
//! outbound bandwidth cost, which is exactly what a write path is meant to
//! avoid. Directed writes use a sequential fallback (try one peer, then the
//! next), never a fanned-out race.

use std::time::Duration;

use futures::{
    FutureExt, StreamExt,
    future::{self, Either},
    stream::FuturesUnordered,
};
use futures_timer::Delay;

/// Default stagger between retrieval candidates joining a [`race_candidates`]
/// race.
///
/// This is the retrieval path's chosen value, passed in by the retrieval
/// callers; the helper itself takes the stagger as a parameter and is not bound
/// to it. Staggering bounds the cost of the fan-out: every raced retrieval the
/// remote answers is paid for in accounting units, so further candidates only
/// start while no response has arrived. A failed attempt starts the next
/// candidate immediately instead of waiting out the stagger.
///
/// The value sits comfortably above a typical live-network retrieval round
/// trip, which spans several forwarding hops and runs in the hundreds of
/// milliseconds. A stagger below that round trip dispatches the second
/// candidate before the first attempt has had a chance to answer, so both entry
/// points forward and deliver the same chunk and both attempts are metered: a
/// near twofold over-fetch on the bulk path. Pacing the second attempt past the
/// round trip keeps the race single-attempt whenever the head is merely in
/// flight, and reserves the fan-out for a head that is genuinely slow or
/// withholding. The failover cost is that a withholding head is now overtaken
/// after this stagger rather than within a few hundred milliseconds; that is
/// acceptable because the stagger stays far below the per-request
/// `retrieval_timeout`, the failed-attempt path still starts the next candidate
/// immediately on an explicit error, and the difficult-chunk retrieval's
/// wall-clock deadline still admits its full attempt budget at this pace.
pub const RETRIEVAL_STAGGER: Duration = Duration::from_millis(1200);

/// Outcome of a candidate race that produced no success.
#[derive(Debug)]
pub enum RaceFailure<E> {
    /// No candidate was supplied, so nothing was attempted.
    NoCandidates,
    /// Every candidate was attempted and failed; carries the last failure.
    AllFailed(E),
    /// The race's wall-clock deadline elapsed before any candidate succeeded.
    /// Distinct from [`AllFailed`](Self::AllFailed) because the race may still
    /// have had attempts in flight or untried candidates when the clock ran out.
    TimedOut,
}

/// Race `candidates` for the first success, dispatching each through `attempt`
/// with a `stagger`-delayed start.
///
/// The best candidate is queried immediately; each `stagger` tick (or earlier
/// failure) adds the next candidate, and the race resolves on the first `Ok`.
/// When the candidates are exhausted with no success, the last failure is
/// returned as [`RaceFailure::AllFailed`]; an empty candidate list yields
/// [`RaceFailure::NoCandidates`]. Losing attempts are dropped as soon as the
/// race resolves, releasing any resource they hold.
///
/// This is a read-style any-candidate race. See the module docs for why it must
/// not be applied to directed-write paths such as pushsync.
pub async fn race_candidates<C, T, E, F, Fut>(
    candidates: impl IntoIterator<Item = C>,
    stagger: Duration,
    mut attempt: F,
) -> Result<T, RaceFailure<E>>
where
    F: FnMut(C) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    // Unbounded: no attempt budget, no in-flight width cap, and no wall-clock
    // deadline, so the race never times out. The closure cannot decline a
    // candidate, so the race ends only on a success, an all-failed drain, or an
    // empty candidate list.
    race(
        candidates,
        usize::MAX,
        usize::MAX,
        None,
        stagger,
        move |candidate| Some(attempt(candidate)),
    )
    .await
}

/// Like [`race_candidates`], but bounded to at most `budget` dispatched attempts
/// and an overall wall-clock `deadline`.
///
/// This is the difficult-chunk retrieval race. Retrieval is forwarding-Kademlia
/// with no authoritative negative response: an attempt that errors or times out
/// means "this entry point could not serve it", never "the chunk is absent". So
/// the race keeps trying the next candidate on every failure and only gives up
/// on a real bound: `budget` attempts dispatched, the candidate source
/// exhausted, or the `deadline`. The caller filters back-pressured peers out of
/// `candidates` beforehand, so a busy peer is never dispatched and never spends a
/// budget unit; `budget` therefore counts only genuine coverage attempts. The
/// `attempt` closure may also decline a candidate at dispatch by returning
/// `None` (a peer that filled since the availability snapshot), which is skipped
/// for the next candidate without spending a budget unit, so the cap holds on the
/// live state rather than the stale snapshot.
///
/// Each attempt is staggered, at most `max_in_flight` run concurrently, and
/// losers are dropped on resolve, so true concurrency is bounded by
/// `max_in_flight` regardless of `budget`: this is a patient race, not a wide
/// simultaneous fan-out. A stagger tick adds an attempt only while fewer than
/// `max_in_flight` are live; a failed attempt still refills at once, which
/// replaces a freed slot rather than widening, so the race keeps its reach
/// without multiplying the metered over-fetch when attempts merely withhold.
pub async fn race_with_refill<C, T, E, I, F, Fut>(
    candidates: I,
    budget: usize,
    max_in_flight: usize,
    deadline: Duration,
    stagger: Duration,
    attempt: F,
) -> Result<T, RaceFailure<E>>
where
    I: IntoIterator<Item = C>,
    F: FnMut(C) -> Option<Fut>,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    race(
        candidates,
        budget,
        max_in_flight,
        Some(deadline),
        stagger,
        attempt,
    )
    .await
}

/// The staggered any-candidate race behind [`race_candidates`] and
/// [`race_with_refill`]: one `FuturesUnordered` driver with the attempt `budget`,
/// the in-flight `max_in_flight` width cap, and an optional wall-clock `deadline`
/// parameterized. Keeping the drop-on-resolve and refill-on-failure invariants in
/// one loop is the point; the two public entry points fix these parameters. An
/// unbounded budget, unbounded width, and absent deadline reproduce
/// [`race_candidates`]; a `Some` deadline surfaces [`RaceFailure::TimedOut`] when
/// the clock runs out with no failure to report.
async fn race<C, T, E, I, F, Fut>(
    candidates: I,
    budget: usize,
    max_in_flight: usize,
    deadline: Option<Duration>,
    stagger: Duration,
    mut attempt: F,
) -> Result<T, RaceFailure<E>>
where
    I: IntoIterator<Item = C>,
    F: FnMut(C) -> Option<Fut>,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut candidates = candidates.into_iter();
    let mut in_flight = FuturesUnordered::new();
    let mut dispatched = 0usize;
    let mut last_error: Option<E> = None;

    // Dispatch the next dispatchable candidate while the attempt budget allows. A
    // candidate the closure declines (returns `None`, e.g. a peer found busy at
    // dispatch) is skipped without spending a budget unit, so only a pushed
    // attempt counts. Returns whether an attempt was pushed (false once the
    // budget is spent or the source is drained, declines included).
    let mut dispatch_next = |in_flight: &mut FuturesUnordered<Fut>, dispatched: &mut usize| {
        if *dispatched >= budget {
            return false;
        }
        for candidate in candidates.by_ref() {
            if let Some(future) = attempt(candidate) {
                in_flight.push(future);
                *dispatched += 1;
                return true;
            }
        }
        false
    };

    if !dispatch_next(&mut in_flight, &mut dispatched) {
        return Err(RaceFailure::NoCandidates);
    }

    let mut stagger_tick = Delay::new(stagger).fuse();
    // No deadline: a never-ready arm the select never wakes on, so the race is
    // bounded only by the budget and the candidate source.
    let mut deadline_tick = match deadline {
        Some(deadline) => Either::Left(Delay::new(deadline)),
        None => Either::Right(future::pending::<()>()),
    }
    .fuse();

    loop {
        futures::select! {
            result = in_flight.select_next_some() => match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    // A failed attempt frees its slot: dispatch the next candidate
                    // at once. When the budget and the source are both spent and no
                    // attempt is in flight, this failure is the race's last word.
                    if !dispatch_next(&mut in_flight, &mut dispatched) && in_flight.is_empty() {
                        return Err(RaceFailure::AllFailed(error));
                    }
                    last_error = Some(error);
                }
            },
            _ = stagger_tick => {
                // Grow the in-flight set only up to the width: a stagger adds an
                // attempt, a withholding attempt never does. Failure-refill above
                // still replaces a freed slot, so reach is preserved without
                // widening. A failed dispatch is permanent (the budget and the
                // source are both monotone), so let the tick lapse rather than
                // re-arm a wakeup that can never dispatch again.
                if in_flight.len() >= max_in_flight
                    || dispatch_next(&mut in_flight, &mut dispatched)
                {
                    stagger_tick = Delay::new(stagger).fuse();
                }
            },
            _ = deadline_tick => {
                // Out of wall-clock time. Surface the last real failure if one
                // landed, else the attempts were merely slow: a distinct TimedOut.
                return match last_error.take() {
                    Some(error) => Err(RaceFailure::AllFailed(error)),
                    None => Err(RaceFailure::TimedOut),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    use futures_timer::Delay;

    use super::*;

    /// A retrieval attempt that records whether it was polled to completion or
    /// dropped mid-flight, so a test can assert losing attempts are dropped (the
    /// reservation-release signal).
    struct TrackedAttempt {
        delay: Delay,
        result: Option<Result<u32, &'static str>>,
        completed: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        done: bool,
    }

    impl TrackedAttempt {
        fn new(
            after: Duration,
            result: Result<u32, &'static str>,
            completed: Arc<AtomicUsize>,
            dropped: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                delay: Delay::new(after),
                result: Some(result),
                completed,
                dropped,
                done: false,
            }
        }
    }

    impl std::future::Future for TrackedAttempt {
        type Output = Result<u32, &'static str>;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match self.delay.poll_unpin(cx) {
                std::task::Poll::Ready(()) => {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                    self.done = true;
                    let result = self.result.take().expect("polled after completion");
                    std::task::Poll::Ready(result)
                }
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        }
    }

    impl Drop for TrackedAttempt {
        fn drop(&mut self) {
            // Only count a drop that pre-empted completion: that is the
            // race-lost path whose resource release we care about.
            if !self.done {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[tokio::test]
    async fn no_candidates_yields_no_candidates() {
        let outcome =
            race_candidates::<u32, u32, &str, _, _>(Vec::new(), RETRIEVAL_STAGGER, |_| async {
                unreachable!("no candidate is attempted")
            })
            .await;

        assert!(matches!(outcome, Err(RaceFailure::NoCandidates)));
    }

    #[tokio::test]
    async fn withholding_head_is_overtaken_by_the_staggered_second() {
        // The head candidate withholds for far longer than the stagger but well
        // inside any per-attempt deadline; the second candidate must overtake it
        // shortly after the stagger tick, not after the head's full delay.
        let completed = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));

        let outcomes = vec![
            // Head: would succeed, but only after ~5s.
            (Duration::from_secs(5), Ok(1u32)),
            // Second: succeeds shortly after it is staggered in.
            (Duration::from_millis(50), Ok(2u32)),
        ];
        let mut outcomes = outcomes.into_iter();

        let start = Instant::now();
        let outcome = race_candidates(0..2, RETRIEVAL_STAGGER, |_| {
            let (after, result) = outcomes.next().expect("an attempt per candidate");
            TrackedAttempt::new(after, result, completed.clone(), dropped.clone())
        })
        .await;
        let elapsed = start.elapsed();

        assert_eq!(outcome.ok(), Some(2), "the staggered second wins");
        // Resolved well under the head's 5s withhold: stagger (~500ms) plus the
        // second attempt (~50ms).
        assert!(
            elapsed < Duration::from_secs(2),
            "race resolved in {elapsed:?}, expected ~stagger not the head delay"
        );
        // The head was still in flight when the race resolved, so it was dropped
        // (its reservation released on drop) rather than run to completion.
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "the losing head is dropped"
        );
        assert_eq!(
            completed.load(Ordering::SeqCst),
            1,
            "only the winner completes"
        );
    }

    #[tokio::test]
    async fn failed_head_starts_the_next_immediately() {
        // A failing head must not wait out the stagger: the second candidate
        // starts the moment the head fails.
        let completed = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));

        let outcomes = vec![
            (Duration::from_millis(10), Err("head failed")),
            (Duration::from_millis(10), Ok(2u32)),
        ];
        let mut outcomes = outcomes.into_iter();

        let start = Instant::now();
        let outcome = race_candidates(0..2, RETRIEVAL_STAGGER, |_| {
            let (after, result) = outcomes.next().expect("an attempt per candidate");
            TrackedAttempt::new(after, result, completed.clone(), dropped.clone())
        })
        .await;
        let elapsed = start.elapsed();

        assert_eq!(outcome.ok(), Some(2), "the second candidate succeeds");
        assert!(
            elapsed < RETRIEVAL_STAGGER,
            "failed head started the next without waiting the stagger ({elapsed:?})"
        );
    }

    #[tokio::test]
    async fn all_failing_yields_the_last_error() {
        let completed = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));

        let outcomes = vec![
            (Duration::from_millis(5), Err("first")),
            (Duration::from_millis(5), Err("second")),
            (Duration::from_millis(5), Err("last")),
        ];
        let mut outcomes = outcomes.into_iter();

        let outcome = race_candidates(0..3, RETRIEVAL_STAGGER, |_| {
            let (after, result) = outcomes.next().expect("an attempt per candidate");
            TrackedAttempt::new(after, result, completed.clone(), dropped.clone())
        })
        .await;

        match outcome {
            Err(RaceFailure::AllFailed(last)) => assert_eq!(last, "last"),
            _ => panic!("expected AllFailed with the last error"),
        }
    }

    /// The race reaches a chunk whose closest few entry points miss: the attempts
    /// before the holder fail, and the race refills to the next-closest until the
    /// holder serves it, well within the budget.
    #[tokio::test]
    async fn race_reaches_a_holder_past_missing_close_peers() {
        // Candidates 0..10 in proximity order; the first four miss, the fifth
        // serves. A bound of 3 would have failed here.
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let outcome = race_with_refill(
            0..10u32,
            8,
            3,
            Duration::from_secs(30),
            RETRIEVAL_STAGGER,
            |i| {
                counted.fetch_add(1, Ordering::SeqCst);
                Some(async move { if i < 4 { Err("miss") } else { Ok(i) } })
            },
        )
        .await;

        assert_eq!(
            outcome.ok(),
            Some(4),
            "the fifth candidate serves the chunk"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            5,
            "exactly the five attempts up to and including the holder were dispatched"
        );
    }

    /// An all-miss race gives up after exactly `budget` attempts, never the whole
    /// pool: the bound caps paid coverage attempts even when far more candidates
    /// are available.
    #[tokio::test]
    async fn race_spends_exactly_the_budget_then_fails() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counted = attempts.clone();
        let outcome = race_with_refill(
            0..100u32,
            6,
            3,
            Duration::from_secs(30),
            RETRIEVAL_STAGGER,
            |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Some(async move { Err::<u32, _>("miss") })
            },
        )
        .await;

        assert!(matches!(outcome, Err(RaceFailure::AllFailed("miss"))));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            6,
            "the budget caps dispatched attempts at 6 of the 100 available"
        );
    }

    /// A candidate the closure declines (returns `None`, a peer busy at dispatch)
    /// is skipped for the next without spending a budget unit, so the budget is
    /// spent only on dispatched attempts.
    #[tokio::test]
    async fn race_skips_declined_candidates_without_spending_budget() {
        // Budget 2. The first three candidates decline; the budget must survive
        // them so the fourth (a miss) and fifth (a hit) are still reached.
        let dispatched = Arc::new(AtomicUsize::new(0));
        let counted = dispatched.clone();
        let outcome = race_with_refill(
            0..10u32,
            2,
            3,
            Duration::from_secs(30),
            RETRIEVAL_STAGGER,
            move |i| {
                if i < 3 {
                    return None; // busy at dispatch: skipped, no budget spent
                }
                counted.fetch_add(1, Ordering::SeqCst);
                Some(async move { if i == 3 { Err("miss") } else { Ok(i) } })
            },
        )
        .await;

        assert_eq!(outcome.ok(), Some(4), "the holder past the declines serves");
        assert_eq!(
            dispatched.load(Ordering::SeqCst),
            2,
            "budget spent only on the two dispatched attempts, not the three declines"
        );
    }

    /// Under a withhold-storm (attempts stall, never error) a stagger grows the
    /// in-flight set only up to the width, never the budget: the metered
    /// over-fetch is bounded by `max_in_flight` even when the budget and the
    /// deadline would admit more attempts.
    #[tokio::test]
    async fn race_caps_concurrent_attempts_at_the_width() {
        let constructed = Arc::new(AtomicUsize::new(0));
        let counted = constructed.clone();
        // Budget 8 over a 300ms deadline at a 20ms stagger would dispatch eight
        // attempts without a width cap; max_in_flight = 3 holds it to three.
        let outcome = race_with_refill(
            0..100u32,
            8,
            3,
            Duration::from_millis(300),
            Duration::from_millis(20),
            |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Some(async {
                    Delay::new(Duration::from_secs(30)).await;
                    Ok::<u32, &str>(0)
                })
            },
        )
        .await;

        assert!(matches!(outcome, Err(RaceFailure::TimedOut)));
        assert_eq!(
            constructed.load(Ordering::SeqCst),
            3,
            "the width caps concurrent attempts at 3, not the budget of 8"
        );
    }

    /// A race whose attempts all merely withhold (never an explicit failure) is
    /// bounded by the wall clock, surfacing TimedOut rather than hanging.
    #[tokio::test]
    async fn race_deadline_bounds_withholding_attempts() {
        let start = Instant::now();
        // Every attempt withholds for far longer than the deadline; none ever errors.
        let outcome = race_with_refill(
            0..8u32,
            8,
            3,
            Duration::from_millis(300),
            Duration::from_millis(50),
            |_| {
                Some(async {
                    Delay::new(Duration::from_secs(30)).await;
                    Ok::<u32, &str>(0)
                })
            },
        )
        .await;
        let elapsed = start.elapsed();

        assert!(
            matches!(outcome, Err(RaceFailure::TimedOut)),
            "withholding attempts surface TimedOut"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "the deadline bounded the race ({elapsed:?})"
        );
    }

    /// An empty pool and a zero budget both attempt nothing.
    #[tokio::test]
    async fn race_with_nothing_to_try_yields_no_candidates() {
        let empty = race_with_refill(
            Vec::<u32>::new(),
            8,
            3,
            Duration::from_secs(1),
            RETRIEVAL_STAGGER,
            |_| Some(async { Ok::<u32, &str>(0) }),
        )
        .await;
        assert!(matches!(empty, Err(RaceFailure::NoCandidates)));

        let zero_budget = race_with_refill(
            0..4u32,
            0,
            3,
            Duration::from_secs(1),
            RETRIEVAL_STAGGER,
            |_| Some(async { Ok::<u32, &str>(0) }),
        )
        .await;
        assert!(matches!(zero_budget, Err(RaceFailure::NoCandidates)));
    }
}
