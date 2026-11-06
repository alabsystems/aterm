// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN UNVERIFIABLE RESERVATION HOLDS ITS OWN SENDER, NOT THE LINK (critic item F21).
//!
//! A post or an owed receipt the bridge publishes again at its pinned sequence can meet an
//! answer its read back cannot vouch for (`Bridge::publish_reserved`, `Unverifiable`): its
//! words no longer match the attempt its pin names, or astream's dedup names an offset where
//! the read finds no record of the pin's faces, a record the pin, from a build before
//! publication identities, cannot tell from another's, a record retention gave up since the
//! answer, or a refusal below the face's oldest that a broker which does not tell fetches
//! leaves undecided. Such an answer is never taken for a delivery, and the post is never
//! retired dead while its first copy may have landed: it stays queued at its pin. Its sender's
//! later posts wait behind it, so they land in its order.
//!
//! Every build before F21 dropped the broker link at such an answer (`reason=read`), so every
//! drain ended at that reservation and EVERY sender's posts on the node waited with it, for as
//! long as retention kept the record the answer named. A link the read itself could not use
//! is still dropped (`PublicationReadFailure`); a verdict is not a transport fault.
//!
//! [`Unverified`] holds only that sender's lane of the outbox ([`Domain`]: its posts, or its
//! owed receipts) and lets every other sender's drain go on. The hold ends on a back-off, from
//! [`FIRST`] doubling at each answer the same pin meets again up to [`CEILING`]; the loop parks
//! no later than its end and the drain then tries the reservation again: the retry at the pin
//! appends anew once retention cut the record and its dedup key, or its read back verifies it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The first hold of a reservation an answer could not verify.
pub(crate) const FIRST: Duration = Duration::from_secs(1);

/// The longest hold: retention runs on minute ticks, so a sender waits at most about a tick
/// past the cut that releases it, and a reservation no cut releases (its words no longer
/// match its pin) costs its lane a retry a minute that reads nothing back and journals
/// nothing before the local check refuses it (`Bridge::landed_at_its_pin`,
/// `Bridge::publish_journaled_entry`).
pub(crate) const CEILING: Duration = Duration::from_secs(60);

/// The part of a session's outbox whose order one reservation holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Domain {
    /// Its posts, which land in its order.
    Posts,
    /// Its owed receipts, which a drain sends in aterm's order.
    Receipts,
}

/// A lane of one session's outbox, by its session and [`Domain`].
pub(crate) type OutboxLane = (String, Domain);

/// One held lane.
#[derive(Debug, Clone, Copy)]
struct Hold {
    /// The pinned sequence whose answer could not be verified.
    seq: u64,
    /// The answers in a row at `seq` that could not be verified.
    tries: u32,
    since: Instant,
    wait: Duration,
    /// Its end was told ([`Unverified::expire`]).
    over: bool,
}

impl Hold {
    fn end(self) -> Option<Instant> {
        self.since.checked_add(self.wait)
    }

    fn holds(self, now: Instant) -> bool {
        self.end().is_none_or(|end| now < end)
    }
}

/// The hold that follows the `tries`-th unverifiable answer at one pin in a row: [`FIRST`]
/// doubled at each, at most [`CEILING`].
#[must_use]
pub(crate) fn back_off(tries: u32) -> Duration {
    let doublings = tries.saturating_sub(1).min(31);
    FIRST.saturating_mul(1u32 << doublings).min(CEILING)
}

/// THE LANES HELD BEHIND A RESERVATION AN ANSWER COULD NOT VERIFY, by session and [`Domain`].
#[derive(Debug, Default)]
pub(crate) struct Unverified {
    held: BTreeMap<(String, Domain), Hold>,
}

impl Unverified {
    /// `sid`'s reservation at `seq` in `domain` met an answer it could not verify at `now`: its
    /// lane is held for the next step of its back-off. Answers how many such answers in a row
    /// that pin has met and the wait.
    pub(crate) fn hold(
        &mut self,
        sid: &str,
        domain: Domain,
        seq: u64,
        now: Instant,
    ) -> (u32, Duration) {
        let tries = self
            .held
            .get(&(sid.to_string(), domain))
            .filter(|had| had.seq == seq)
            .map_or(1, |had| had.tries.saturating_add(1));
        let wait = back_off(tries);
        self.held.insert(
            (sid.to_string(), domain),
            Hold {
                seq,
                tries,
                since: now,
                wait,
                over: false,
            },
        );
        (tries, wait)
    }

    /// Whether `sid`'s lane in `domain` is held at `now`.
    pub(crate) fn holds(&self, sid: &str, domain: Domain, now: Instant) -> bool {
        self.held
            .get(&(sid.to_string(), domain))
            .is_some_and(|hold| hold.holds(now))
    }

    /// The earliest end of a hold still running at `now`; a hold whose end cannot be
    /// represented is revisited at the ceiling.
    pub(crate) fn next_end(&self, now: Instant) -> Option<Instant> {
        self.held
            .values()
            .filter(|hold| hold.holds(now))
            .filter_map(|hold| hold.end().or_else(|| now.checked_add(CEILING)))
            .min()
    }

    /// Tell the ends of the holds over by `now`: `true` if one ended since the last call, so
    /// the drain tries its reservation again. An ended hold keeps its count for the back-off
    /// until its pin is answered otherwise or its lane leaves the outbox
    /// ([`Unverified::retain_listed`]).
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let mut ended = false;
        for hold in self.held.values_mut() {
            if !hold.over && !hold.holds(now) {
                hold.over = true;
                ended = true;
            }
        }
        ended
    }

    /// Forget every lane `listed` says a whole outbox listing no longer names: nothing of it is
    /// queued, so nothing waits on its back-off.
    pub(crate) fn retain_listed(&mut self, listed: impl Fn(&str, Domain) -> bool) {
        self.held.retain(|(sid, domain), _| listed(sid, *domain));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_back_off_doubles_from_its_first_hold_to_its_ceiling() {
        assert_eq!(back_off(0), FIRST);
        assert_eq!(back_off(1), FIRST);
        assert_eq!(back_off(2), FIRST * 2);
        assert_eq!(back_off(6), FIRST * 32);
        assert_eq!(back_off(7), CEILING);
        assert_eq!(back_off(u32::MAX), CEILING);
    }

    #[test]
    fn a_hold_holds_only_its_own_lane_until_its_end() {
        let t0 = Instant::now();
        let mut held = Unverified::default();
        assert_eq!(held.hold("s-a", Domain::Posts, 7, t0), (1, FIRST));
        assert!(held.holds("s-a", Domain::Posts, t0));
        assert!(!held.holds("s-b", Domain::Posts, t0), "another sender");
        assert!(
            !held.holds("s-a", Domain::Receipts, t0),
            "the same session's receipts"
        );
        assert_eq!(held.next_end(t0), Some(t0 + FIRST));
        assert!(!held.expire(t0), "not over yet");
        assert!(held.expire(t0 + FIRST), "its end is told once");
        assert!(!held.expire(t0 + FIRST));
        assert!(!held.holds("s-a", Domain::Posts, t0 + FIRST));
        assert_eq!(held.next_end(t0 + FIRST), None);
    }

    #[test]
    fn the_same_pin_answered_again_waits_longer_and_another_pin_starts_over() {
        let t0 = Instant::now();
        let mut held = Unverified::default();
        held.hold("s-a", Domain::Posts, 7, t0);
        let t1 = t0 + FIRST;
        assert!(held.expire(t1));
        assert_eq!(held.hold("s-a", Domain::Posts, 7, t1), (2, FIRST * 2));
        assert_eq!(held.next_end(t1), Some(t1 + FIRST * 2));
        let mut at = t1;
        for _ in 0..8 {
            at += CEILING;
            held.hold("s-a", Domain::Posts, 7, at);
        }
        assert_eq!(held.next_end(at), Some(at + CEILING), "at most the ceiling");
        assert_eq!(held.hold("s-a", Domain::Posts, 9, at), (1, FIRST));
    }

    #[test]
    fn a_whole_listing_forgets_the_lanes_it_no_longer_names() {
        let t0 = Instant::now();
        let mut held = Unverified::default();
        held.hold("s-a", Domain::Posts, 7, t0);
        held.hold("s-b", Domain::Receipts, 8, t0);
        held.retain_listed(|sid, domain| sid == "s-b" && domain == Domain::Receipts);
        assert!(!held.holds("s-a", Domain::Posts, t0));
        assert!(held.holds("s-b", Domain::Receipts, t0));
    }
}
