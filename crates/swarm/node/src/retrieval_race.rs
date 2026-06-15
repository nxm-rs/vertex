//! Staggered candidate race for outbound chunk retrieval.
//!
//! Retrieval has several candidate storers in proximity order, any of which can
//! answer. Walking them strictly in series lets one slow or withholding head
//! candidate stall the whole request for a full per-attempt deadline, and on a
//! streamed download that head-of-line stall blocks every later chunk behind
//! it.
//!
//! [`race_candidates`] instead queries the best candidate immediately and adds
//! each further candidate after a [`RETRIEVAL_STAGGER`] tick (or as soon as an
//! earlier attempt fails), resolving on the first success. Staggering bounds the
//! fan-out cost: every raced attempt the remote answers is paid for in
//! accounting units, so further candidates only start while no response has
//! arrived. A withholding head candidate is therefore overtaken by the next
//! candidate within the stagger instead of within the per-attempt deadline.
//!
//! The losing attempts are dropped the moment the race resolves: returning from
//! the race drops every future still in the [`FuturesUnordered`], so any
//! accounting reservation or forwarder slot a losing attempt holds is released
//! on drop. The per-candidate retrieval closure carries its own pacing (the
//! outbound self-throttle and affordability check run inside each attempt before
//! it dispatches), so staggered starts preserve the per-peer pacing that a
//! single all-at-once fan-out would skip.

use std::time::Duration;

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use futures_timer::Delay;

/// Delay before each additional retrieval candidate joins the race.
///
/// Staggering bounds the cost of the fan-out: every raced attempt the remote
/// answers is paid for in accounting units, so further candidates only start
/// while no response has arrived. A failed attempt starts the next candidate
/// immediately instead of waiting out the stagger.
pub const RETRIEVAL_STAGGER: Duration = Duration::from_millis(500);

/// Outcome of a candidate race that produced no success.
#[derive(Debug)]
pub enum RaceFailure<E> {
    /// No candidate was supplied, so nothing was attempted.
    NoCandidates,
    /// Every candidate was attempted and failed; carries the last failure.
    AllFailed(E),
}

/// Race `candidates` for the first successful retrieval, dispatching each
/// through `attempt` with a staggered start.
///
/// The best candidate is queried immediately; each [`RETRIEVAL_STAGGER`] tick
/// (or earlier failure) adds the next candidate, and the race resolves on the
/// first `Ok`. When the candidates are exhausted with no success, the last
/// failure is returned as [`RaceFailure::AllFailed`]; an empty candidate list
/// yields [`RaceFailure::NoCandidates`]. Losing attempts are dropped as soon as
/// the race resolves, releasing any resource they hold.
pub async fn race_candidates<C, T, E, F, Fut>(
    candidates: impl IntoIterator<Item = C>,
    mut attempt: F,
) -> Result<T, RaceFailure<E>>
where
    F: FnMut(C) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut candidates = candidates.into_iter();

    let mut in_flight = FuturesUnordered::new();
    match candidates.next() {
        Some(candidate) => in_flight.push(attempt(candidate)),
        None => return Err(RaceFailure::NoCandidates),
    }

    let mut stagger = Delay::new(RETRIEVAL_STAGGER).fuse();

    loop {
        futures::select! {
            result = in_flight.select_next_some() => match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    // A failed attempt frees its slot: start the next candidate
                    // immediately. Once no candidates and no attempts remain,
                    // the race ends with the last attempt's error.
                    if let Some(candidate) = candidates.next() {
                        in_flight.push(attempt(candidate));
                    } else if in_flight.is_empty() {
                        return Err(RaceFailure::AllFailed(error));
                    }
                }
            },
            _ = stagger => {
                if let Some(candidate) = candidates.next() {
                    in_flight.push(attempt(candidate));
                    stagger = Delay::new(RETRIEVAL_STAGGER).fuse();
                }
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

    /// A retrieval leg that records whether it was polled to completion or
    /// dropped mid-flight, so a test can assert losing attempts are dropped (the
    /// reservation-release signal).
    struct TrackedLeg {
        delay: Delay,
        result: Option<Result<u32, &'static str>>,
        completed: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        done: bool,
    }

    impl TrackedLeg {
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

    impl std::future::Future for TrackedLeg {
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

    impl Drop for TrackedLeg {
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
        let outcome = race_candidates::<u32, u32, &str, _, _>(Vec::new(), |_| async {
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

        let legs = vec![
            // Head: would succeed, but only after ~5s.
            (Duration::from_secs(5), Ok(1u32)),
            // Second: succeeds shortly after it is staggered in.
            (Duration::from_millis(50), Ok(2u32)),
        ];
        let mut legs = legs.into_iter();

        let start = Instant::now();
        let outcome = race_candidates(0..2, |_| {
            let (after, result) = legs.next().expect("a leg per candidate");
            TrackedLeg::new(after, result, completed.clone(), dropped.clone())
        })
        .await;
        let elapsed = start.elapsed();

        assert_eq!(outcome.ok(), Some(2), "the staggered second wins");
        // Resolved well under the head's 5s withhold: stagger (~500ms) plus the
        // second leg (~50ms).
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

        let legs = vec![
            (Duration::from_millis(10), Err("head failed")),
            (Duration::from_millis(10), Ok(2u32)),
        ];
        let mut legs = legs.into_iter();

        let start = Instant::now();
        let outcome = race_candidates(0..2, |_| {
            let (after, result) = legs.next().expect("a leg per candidate");
            TrackedLeg::new(after, result, completed.clone(), dropped.clone())
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

        let legs = vec![
            (Duration::from_millis(5), Err("first")),
            (Duration::from_millis(5), Err("second")),
            (Duration::from_millis(5), Err("last")),
        ];
        let mut legs = legs.into_iter();

        let outcome = race_candidates(0..3, |_| {
            let (after, result) = legs.next().expect("a leg per candidate");
            TrackedLeg::new(after, result, completed.clone(), dropped.clone())
        })
        .await;

        match outcome {
            Err(RaceFailure::AllFailed(last)) => assert_eq!(last, "last"),
            _ => panic!("expected AllFailed with the last error"),
        }
    }
}
