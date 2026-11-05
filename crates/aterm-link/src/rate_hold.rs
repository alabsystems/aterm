// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER'S RATE LIMIT, WAITED OUT (critic item E06).
//!
//! `aterm link broker` opens its broker under a per-producer token bucket
//! ([`crate::broker_rules::rate_limit`]). A publish past it is refused `ERR_RATE_LIMITED`, naming
//! the wait the bucket needs (astream's `BrokerRefusal::retry_after_ms`): nothing was appended,
//! and the same request lands once the bucket has refilled, so the refusal is neither for good
//! nor another writer's. Retried at once, it is refused again, and again, for as long as the
//! bucket stays empty: a hot loop of refusals, each one more request on the broker's connection
//! thread.
//!
//! The bucket is the PRODUCER's, and the bridge publishes every session of its node under one
//! producer, so a refusal says every publish of the bridge's would be refused until the bucket
//! refills: [`RateHold`] holds them all ([`RateHold::refused`]) for the wait, capped at
//! [`PRODUCER_HOLD_MAX`], and the refused post's sender for the whole of it
//! ([`RateHold::hold_sender`]). A one-shot client (`aterm fabric post`, a ghost row's exit), and
//! a quit's withdrawal, the bridge's last rows ([`WITHDRAW_WAIT_MAX`]), wait out their own
//! refusal and send the same request again at the same sequence ([`waiting_out`]), so astream's
//! dedup answers a resend that did land.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

/// The most a refusal holds every publish of the producer. A record-rate refusal waits at most
/// a second at every rate astream takes (one token at `records_per_sec >= 1`); only a byte
/// bucket names more, for a record larger than the bucket can take soon. That record's sender
/// waits the whole of it, and the producer's other publishes go on after at most this.
pub(crate) const PRODUCER_HOLD_MAX: Duration = Duration::from_secs(1);

/// The wait a rate-limited refusal that names none is taken to name.
const UNNAMED_WAIT: Duration = Duration::from_secs(1);

/// The wait a rate-limited refusal names (`ERR_RATE_LIMITED`, at least a millisecond), or
/// `None` for every other answer.
pub(crate) fn wait_of(e: &io::Error) -> Option<Duration> {
    let refusal = astream_broker::broker_refusal(e).filter(|r| r.is_rate_limited())?;
    Some(
        refusal
            .retry_after_ms()
            .map_or(UNNAMED_WAIT, |ms| Duration::from_millis(ms.max(1))),
    )
}

/// A publish the bridge held back unsent, the producer's bucket held empty for `0` more
/// ([`RateHold`]): never on the wire, so nothing of it can be on the bus.
#[derive(Debug)]
pub(crate) struct RateHeld(Duration);

impl std::fmt::Display for RateHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "held unsent for {} ms more: the broker's rate limit refused this producer",
            self.0.as_millis()
        )
    }
}

impl std::error::Error for RateHeld {}

/// The error of a publish [`RateHold`] keeps from the wire for `left` more.
pub(crate) fn held(left: Duration) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, RateHeld(left))
}

/// Whether `e` is the broker's rate-limited refusal or the bridge's hold behind one: either way
/// nothing was appended, and the same publish lands once the wait is over.
pub(crate) fn rate_limited(e: &io::Error) -> bool {
    wait_of(e).is_some() || is_held(e)
}

/// Whether `e` is the bridge's own hold ([`held`]): a publish never sent.
pub(crate) fn is_held(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<RateHeld>())
}

/// How long `e` says to wait before the same publish is sent again: the broker's refusal for
/// the rate ([`wait_of`]) or the bridge's hold behind one ([`held`]); `None` for every other
/// answer.
pub(crate) fn wait_for(e: &io::Error) -> Option<Duration> {
    wait_of(e).or_else(|| {
        e.get_ref()
            .and_then(|inner| inner.downcast_ref::<RateHeld>())
            .map(|held| held.0.max(Duration::from_millis(1)))
    })
}

/// The most an attach's replay of the in-flight journal waits out the limit for one entry
/// ([`waiting_out`]) before it gives the attach up, the entry kept, to try again later. Astream
/// answers a refusal for the rate ahead of its dedup, so the refusal of a replay says nothing of
/// whether an earlier attempt landed: only the replay's own answer does.
pub(crate) const REPLAY_WAIT_MAX: Duration = Duration::from_secs(3);

/// The most a quit's withdrawal waits out the limit, over all its sessions' `state=exited` rows
/// ([`waiting_out`]): aterm has gone and the bridge winds down after them, so nothing else of the
/// bridge's waits behind them, and a record-rate refusal names at most a second. A row still
/// refused past it is left `live` behind the node's will, which readers see `away`, as at a quit
/// the broker does not answer.
pub(crate) const WITHDRAW_WAIT_MAX: Duration = Duration::from_secs(3);

/// THE HOLDS a rate-limited refusal set: every publish of the producer's until one instant, and
/// each refused post's sender for its full wait.
#[derive(Debug, Default)]
pub(crate) struct RateHold {
    producer: Option<Instant>,
    senders: BTreeMap<String, SenderHold>,
}

/// Keep the original duration even when its end cannot be represented as an instant.
#[derive(Debug, Clone, Copy)]
struct SenderHold {
    since: Instant,
    wait: Duration,
}

impl SenderHold {
    fn remaining(self, now: Instant) -> Duration {
        self.wait
            .saturating_sub(now.saturating_duration_since(self.since))
    }
}

impl RateHold {
    /// A refusal at `now` named `wait`: every publish of the producer waits it out, capped at
    /// [`PRODUCER_HOLD_MAX`]. A later end already set stands.
    pub(crate) fn refused(&mut self, now: Instant, wait: Duration) {
        let until = now + wait.min(PRODUCER_HOLD_MAX);
        self.producer = Some(self.producer.map_or(until, |had| had.max(until)));
    }

    /// `sid`'s post was refused at `now`, naming `wait`: its sender waits the whole of it, so its
    /// posts go next in its order once the bucket can take the refused one.
    pub(crate) fn hold_sender(&mut self, sid: &str, now: Instant, wait: Duration) {
        let next = SenderHold { since: now, wait };
        self.senders
            .entry(sid.to_string())
            .and_modify(|had| {
                if wait > had.remaining(now) {
                    *had = next;
                }
            })
            .or_insert(next);
    }

    /// How much longer every publish of the producer is held at `now`, if it is.
    pub(crate) fn producer_left(&self, now: Instant) -> Option<Duration> {
        self.producer
            .filter(|until| *until > now)
            .map(|until| until - now)
    }

    /// Whether a post of `sid`'s waits at `now`: the producer is held, or `sid` is.
    pub(crate) fn holds_sender(&self, sid: &str, now: Instant) -> bool {
        self.producer_left(now).is_some()
            || self
                .senders
                .get(sid)
                .is_some_and(|hold| !hold.remaining(now).is_zero())
    }

    /// The loop's next wake: the earliest hold end, or a bounded revisit when an end
    /// cannot be represented as an instant.
    pub(crate) fn next_end(&self, now: Instant) -> Option<Instant> {
        self.producer
            .into_iter()
            .chain(self.senders.values().filter_map(|hold| {
                if hold.remaining(now).is_zero() {
                    return None;
                }
                // An unrepresentable end still holds its sender. Revisit it at a
                // bounded interval rather than overflow, forget it, or spin.
                hold.since
                    .checked_add(hold.wait)
                    .or_else(|| now.checked_add(PRODUCER_HOLD_MAX))
            }))
            .filter(|until| *until > now)
            .min()
    }

    /// Forget every hold over by `now`: `true` if one was, so what it held is sent again.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let mut ended = self.producer.take_if(|until| *until <= now).is_some();
        let before = self.senders.len();
        self.senders
            .retain(|_, hold| !hold.remaining(now).is_zero());
        ended |= self.senders.len() != before;
        ended
    }
}

/// THE WAIT OF A PUBLISH THAT MUST LAND BEFORE ANYTHING ELSE GOES ON (a one-shot client's, an
/// attach's replay): `attempt` (one publish at one sequence) again after each refusal for the
/// rate or hold behind one, `sleep`ing the wait it names ([`wait_for`]), while the waits add up
/// to at most `budget`; past it, the refusal itself. Every other answer is `attempt`'s own. A
/// resend at the same sequence is astream's dedup's to answer, so a refusal that crossed a
/// landing appends nothing twice.
///
/// # Errors
/// `attempt`'s last error.
pub(crate) fn waiting_out<T>(
    mut attempt: impl FnMut() -> io::Result<T>,
    budget: Duration,
    mut sleep: impl FnMut(Duration),
) -> io::Result<T> {
    let mut waited = Duration::ZERO;
    loop {
        match attempt() {
            Err(e) => match wait_for(&e) {
                Some(wait) if waited.saturating_add(wait) <= budget => {
                    sleep(wait);
                    waited = waited.saturating_add(wait);
                }
                _ => return Err(e),
            },
            done => return done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(code: u8, msg: &str) -> io::Error {
        astream_broker::BrokerRefusal {
            code,
            msg: msg.to_string(),
        }
        .into()
    }

    fn limited(ms: u64) -> io::Error {
        refusal(
            astream_broker::proto::ERR_RATE_LIMITED,
            &format!(
                "{}{ms} (record rate exceeded)",
                astream_broker::proto::RATE_LIMITED_PREFIX
            ),
        )
    }

    #[test]
    fn only_a_rate_limited_refusal_names_a_wait() {
        assert_eq!(wait_of(&limited(7)), Some(Duration::from_millis(7)));
        assert_eq!(wait_of(&limited(0)), Some(Duration::from_millis(1)));
        assert_eq!(
            wait_of(&refusal(
                astream_broker::proto::ERR_RATE_LIMITED,
                "no number"
            )),
            Some(UNNAMED_WAIT)
        );
        assert_eq!(
            wait_of(&refusal(
                astream_broker::proto::ERR_REFUSED,
                "batch aborted"
            )),
            None
        );
        assert!(rate_limited(&held(Duration::from_millis(3))));
        assert!(!rate_limited(&io::Error::other("lost")));
    }

    #[test]
    fn the_producer_waits_at_most_its_cap_and_the_sender_the_whole_wait() {
        let t0 = Instant::now();
        let mut hold = RateHold::default();
        let long = Duration::from_secs(30);
        hold.refused(t0, long);
        hold.hold_sender("s-a", t0, long);
        assert_eq!(hold.producer_left(t0), Some(PRODUCER_HOLD_MAX));
        assert!(
            hold.holds_sender("s-b", t0),
            "every sender while the producer waits"
        );
        let after_cap = t0 + PRODUCER_HOLD_MAX;
        assert!(hold.expire(after_cap), "the producer's hold ended");
        assert_eq!(hold.producer_left(after_cap), None);
        assert!(!hold.holds_sender("s-b", after_cap));
        assert!(
            hold.holds_sender("s-a", after_cap),
            "s-a waits its whole wait"
        );
        assert_eq!(hold.next_end(after_cap), Some(t0 + long));
        assert!(hold.expire(t0 + long));
        assert!(!hold.holds_sender("s-a", t0 + long));
        assert_eq!(hold.next_end(t0 + long), None);
        assert!(!hold.expire(t0 + long), "nothing left to end");
    }

    #[test]
    fn a_later_end_stands() {
        let t0 = Instant::now();
        let mut hold = RateHold::default();
        hold.refused(t0, Duration::from_millis(500));
        hold.refused(t0, Duration::from_millis(100));
        assert_eq!(hold.producer_left(t0), Some(Duration::from_millis(500)));
        hold.hold_sender("s-a", t0, Duration::from_millis(500));
        hold.hold_sender("s-a", t0, Duration::from_millis(100));
        assert_eq!(hold.next_end(t0), Some(t0 + Duration::from_millis(500)));
    }

    #[test]
    fn an_unrepresentable_wait_keeps_the_sender_held_without_panicking() {
        let t0 = Instant::now();
        for wait in [wait_of(&limited(u64::MAX)).unwrap(), Duration::MAX] {
            let mut hold = RateHold::default();
            hold.hold_sender("s-a", t0, wait);
            let later = t0 + Duration::from_secs(2);
            hold.hold_sender("s-a", later, Duration::from_millis(1));
            assert!(hold.holds_sender("s-a", later));
            assert!(!hold.holds_sender("s-b", later));
            assert!(!hold.expire(later));
            let wake = hold.next_end(later).expect("the held sender has a wake");
            assert!(wake > later, "a long wait never spins");
            if t0.checked_add(wait).is_none() {
                assert!(
                    wake <= later + PRODUCER_HOLD_MAX,
                    "revisit without shortening the hold"
                );
            }
            assert!(hold.holds_sender("s-a", later + Duration::from_secs(1)));
        }
    }

    #[test]
    fn a_one_shot_client_waits_out_its_refusals_within_its_budget() {
        let mut answers = vec![limited(40), limited(30)].into_iter();
        let mut slept = Vec::new();
        let landed = waiting_out(
            || answers.next().map_or(Ok(9u64), Err),
            Duration::from_millis(100),
            |d| slept.push(d),
        );
        assert_eq!(landed.unwrap(), 9);
        assert_eq!(
            slept,
            [Duration::from_millis(40), Duration::from_millis(30)]
        );

        let mut tries = 0;
        let refused = waiting_out(
            || {
                tries += 1;
                Err::<u64, _>(limited(60))
            },
            Duration::from_millis(100),
            |_| {},
        );
        assert!(wait_of(&refused.unwrap_err()).is_some());
        assert_eq!(tries, 2, "the second wait would pass the budget");

        let mut tries = 0;
        let other = waiting_out(
            || {
                tries += 1;
                Err::<u64, _>(refusal(astream_broker::proto::ERR_REFUSED, "rolled back"))
            },
            Duration::from_secs(10),
            |_| {},
        );
        assert!(other.is_err());
        assert_eq!(tries, 1, "only a rate-limited refusal is waited out");
    }
}
