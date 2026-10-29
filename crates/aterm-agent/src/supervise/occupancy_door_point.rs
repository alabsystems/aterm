// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE IDLE-POINT RIDER (the slice-2 plan's 2.9, D3): a declared wake's fact rides the act the
//! policy types at an idle point, in the one fact slot, under the owner's `prompt` alone. It never
//! makes an act of its own: the policy's act goes with it or without it, never lost to it. The
//! decision carries it ([`Session::wake_ride`]); the write claims it just before the act is typed
//! (`IdleHost::wake_claim(.., Point)`), and any answer but a grant types the policy's own text
//! ([`Session::point_claims`]); a typed one is told once ([`Session::point_told`]). Under
//! `observe` the point types the policy's own text and books what it would have carried
//! (`fact-would@v1 … door=point`, [`Session::point_would`]); under `off`, nothing.

use std::time::Instant;

use crate::supervise::facts::{EpisodeKey, FactKind};
use crate::supervise::occupancy::door::LeftRow;
use crate::supervise::occupancy::rules::{SESSION_PACE_MS, WOULD_RULE};
use crate::supervise::policy::turn_end::{
    RideFact, carries_fact, fact_may_ride, fact_would_ride, wake_fact, without_fact,
};

use super::*;

/// A wake's fact the act being typed carries, its claim granted for the point: what its rows and
/// its tell need once the write comes back ([`Session::point_told`]).
#[derive(Debug)]
pub(in crate::supervise::run) struct PointRide {
    ride: RideFact,
    /// How long the hold had stood at the claim (`0`: none stood), as its rows say it.
    held_ms: i64,
    /// The claim's time on the clock's milliseconds.
    now_ms: i64,
}

impl<C: Ctl> Session<'_, C> {
    /// THE FACT A POINT'S ACT MAY CARRY, decided with it: under the owner's `prompt` (a
    /// [`RideFact`] needs its grant), on a recording with slice 2's doors, with `[harness] facts`
    /// on and `action` an act a fact rides ([`fact_may_ride`]), the book's head where
    /// [`rules::at_point`] says it due on the guards the loop holds (`judged`'s rows; nothing is
    /// read for them) and the session's pace. The book is read for it ([`Self::book_read`]) only
    /// past the guards and the pace. Under `observe` nothing is read: the book as the loop last
    /// read it says what would have ridden (`FactsWatch::offered_would`), and `None` rides.
    pub(in crate::supervise::run) fn wake_ride(
        &mut self,
        action: &TurnEndAction,
        opts: &SuperviseOpts,
        now: Instant,
        judged: &Screen,
    ) -> Option<RideFact> {
        let cfg = &opts.policy;
        if !cfg.facts
            || !self.fx.slice2_recorded()
            || !cfg.occupancy.observes()
            || !fact_may_ride(&self.turn_end, action)
        {
            return None;
        }
        let host = self.stall_host.clone()?;
        let now_ms = self.hold_ms(now);
        let guards = self.occupancy_guards(now, cfg, &judged.rows);
        let Some(grant) = cfg.occupancy.grant() else {
            self.facts.offered_would = self.point_would_ride(action, &guards, now_ms);
            return None;
        };
        if guards.held(RuleId::Wake).is_some()
            || rules::pace_next(&self.hold.door.books, now_ms).is_some()
        {
            return None;
        }
        self.book_read(host.as_ref(), judged.seq);
        self.hold.door.books.keep_wakes(&self.hold.door.wakes);
        let wake = self.point_due(&guards, now_ms)?;
        let line = self.wake_line(&wake, WakeFacts::default());
        Some(RideFact::new(
            grant,
            &line,
            wake.ord,
            wake.episode(),
            self.hold_wall_ms(now_ms),
        ))
    }

    /// The book's head due at a point at `now_ms` (the clock's) on `guards`
    /// ([`rules::at_point`]), as the loop last read the book; `None` where it is not, or where its
    /// last write at a point or a break wrote nothing within the session's pace.
    fn point_due(&self, guards: &Guards, now_ms: i64) -> Option<WakeSeen> {
        let facts = &self.facts.state;
        let told = |id| {
            let key = EpisodeKey {
                kind: FactKind::Wake,
                id,
            };
            facts.is_spoken_for(key)
        };
        let sight = WakeSight {
            seen: &self.hold.door.wakes,
            wall_ms: self.hold_wall_ms(now_ms),
            told: &told,
            ranked: true,
        };
        let (verdict, wake) = rules::at_point(&self.hold.door.books, guards, now_ms, &sight);
        let wake = wake.filter(|_| matches!(verdict, Verdict::Due(_)))?;
        let again = self
            .hold
            .door
            .again
            .is_some_and(|(waits, at_ms)| waits == wake.episode() && now_ms < at_ms);
        (!again).then(|| wake.clone())
    }

    /// Under `observe`, the wake (its ordinal and episode) `action` would have carried under
    /// `prompt`: the head due ([`Self::point_due`]) whose fact fits ([`fact_would_ride`]), its
    /// clock's length kept without the local zone's probe (no new read is made under `observe`).
    fn point_would_ride(
        &self,
        action: &TurnEndAction,
        guards: &Guards,
        now_ms: i64,
    ) -> Option<(u16, u64)> {
        let wake = self.point_due(guards, now_ms)?;
        let since = wake.ready_since_ms.map(|_| (0, 0));
        let line = templates::wake(wake.ord, wake.class, &wake.kinds, since, None);
        let fact = wake_fact(&line, wake.episode(), self.hold_wall_ms(now_ms));
        fact_would_ride(&self.turn_end, action, &fact).then(|| (wake.ord, wake.episode()))
    }

    /// THE POINT'S CLAIM, just before `action` is typed (the slice-2 plan's W2): where the act
    /// carries the wake's fact its decision offered (`FactsWatch::offered_wake`), the owner's
    /// `prompt` is asked again, the intent row booked (`fact-intent@v1 … door=point`) and the host
    /// asked for the wake (`IdleHost::wake_claim(.., Point)`: its book, its own mode, slice 1's
    /// refusals, the need re-read and the wake lines' pace). Granted: the act goes as decided,
    /// its fact with it. Any other answer types the policy's own text ([`without_fact`]): one the
    /// agent's own `await wake` answered is booked told by it (`told door=await`), the rest a
    /// skip, the claim given back where the host may hold it ([`WakeClaim::releases`]). A wake
    /// told since the decision, or whose line stands left in the composer, rides nothing and
    /// books nothing.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "Type",
            project = "conformance_occupancy::project"
        )
    )]
    pub(in crate::supervise::run) fn point_claims(
        &mut self,
        action: TurnEndAction,
        cfg: &SupervisorConfig,
        seq: u64,
        review: &mut dyn Review,
    ) -> (TurnEndAction, Option<PointRide>) {
        let Some(ride) = self
            .facts
            .offered_wake
            .take()
            .filter(|w| carries_fact(&action, w.fact()))
        else {
            return (action, None);
        };
        let without = armed(Fault::RideWithoutClaim);
        let key = ride.fact().key;
        let host = self
            .stall_host
            .clone()
            .filter(|_| cfg.occupancy.grant().is_some());
        let untold = !self.facts.state.is_spoken_for(key);
        let Some(host) = host.filter(|_| without || untold) else {
            return (without_fact(action, ride.fact()), None);
        };
        let (ord, episode) = (ride.ord(), ride.episode());
        let now_ms = self.hold_ms(self.fx.mono());
        let held_ms = self
            .hold
            .clock
            .since_ms()
            .map_or(0, |since| now_ms.saturating_sub(since).max(0));
        self.ledger_row(
            door::INTENT_RULE,
            Outcome::Waiting,
            "",
            &door::wake_intent_reason(ord, episode, held_ms, WakeDoor::Point),
            seq,
        );
        let claim = host.wake_claim(ord, episode, WakeDoor::Point);
        if claim.lets() || without {
            let ride = PointRide {
                ride,
                held_ms,
                now_ms,
            };
            return (action, Some(ride));
        }
        if claim == WakeClaim::Answered {
            self.facts.state.told(key, self.hold_wall_ms(now_ms));
            self.ledger_row(
                FactKind::Wake.rule(),
                Outcome::Waiting,
                "",
                &door::wake_heard_reason(ord, episode),
                seq,
            );
            self.point_note(seq, ord, episode, "told door=await", review);
        } else {
            if claim.releases() {
                host.wake_release(ord, episode, WakeDoor::Point);
            }
            self.point_skipped(&ride, (held_ms, now_ms), claim.word(), seq, review);
        }
        (without_fact(action, ride.fact()), None)
    }

    /// The act `ride` rode came back from its write: typed (`skipped` `None`), its fact told once
    /// (the loop's facts, the session's pace, the policy's cut text, the host's note), its row
    /// booked (`fact-wake@v1 typed … door=point`) and the host told (`wake_told(.., Point)`); else
    /// the claim given back and the skip booked.
    pub(in crate::supervise::run) fn point_told(
        &mut self,
        ride: PointRide,
        skipped: Option<&'static str>,
        seq: u64,
        review: &mut dyn Review,
    ) {
        let PointRide {
            ride,
            held_ms,
            now_ms,
        } = ride;
        let (ord, episode) = (ride.ord(), ride.episode());
        let Some(host) = self.stall_host.clone() else {
            return;
        };
        if let Some(why) = skipped {
            host.wake_release(ord, episode, WakeDoor::Point);
            self.point_skipped(&ride, (held_ms, now_ms), why, seq, review);
            return;
        }
        let fact = ride.fact();
        self.facts.state.told(fact.key, self.hold_wall_ms(now_ms));
        self.hold.door.books.woke(episode, now_ms);
        self.turn_end.fact_rode(fact);
        host.inform(&templates::stuck_line(&fact.text));
        self.ledger_row(
            FactKind::Wake.rule(),
            Outcome::Typed,
            &fact.text,
            &door::wake_outcome_reason(ord, episode, held_ms, WakeDoor::Point, Told::Typed),
            seq,
        );
        host.wake_told(ord, episode, WakeDoor::Point);
        self.point_note(seq, ord, episode, "typed", review);
    }

    /// The act `ride` rode was written and not submitted (`Written::Left`): its fact stands left
    /// in the composer (`Session::left_write`), the claim kept the point's until a look settles
    /// it, and tried again, should the look release it, only past the session's pace. Under the
    /// planted `ReleaseOnLeft`, the claim is given back and the skip booked, as before.
    pub(in crate::supervise::run) fn point_left(
        &mut self,
        ride: PointRide,
        seq: u64,
        review: &mut dyn Review,
    ) {
        let PointRide {
            ride,
            held_ms,
            now_ms,
        } = ride;
        let (ord, episode) = (ride.ord(), ride.episode());
        let row = LeftRow::Wake {
            ord,
            held_ms,
            door: WakeDoor::Point,
        };
        let fact = ride.fact();
        if !self.left_write(fact.key, row, &fact.text, seq, review) {
            return self.point_told(
                PointRide {
                    ride,
                    held_ms,
                    now_ms,
                },
                Some("not-submitted"),
                seq,
                review,
            );
        }
        self.hold.door.again = Some((episode, now_ms.saturating_add(SESSION_PACE_MS)));
    }

    /// A point's write of `ride` that told nothing after its intent row: its outcome row
    /// `skipped why=<why>`, the wake untold, and tried again at a point or a break only past the
    /// session's pace.
    fn point_skipped(
        &mut self,
        ride: &RideFact,
        (held_ms, now_ms): (i64, i64),
        why: &'static str,
        seq: u64,
        review: &mut dyn Review,
    ) {
        let (ord, episode) = (ride.ord(), ride.episode());
        self.hold.door.again = Some((episode, now_ms.saturating_add(SESSION_PACE_MS)));
        self.ledger_row(
            FactKind::Wake.rule(),
            Outcome::Skipped,
            "",
            &door::wake_outcome_reason(ord, episode, held_ms, WakeDoor::Point, Told::Skipped(why)),
            seq,
        );
        self.point_note(seq, ord, episode, &format!("skipped why={why}"), review);
    }

    /// Under `observe`, the wake `action` would have carried under `prompt` as its decision found
    /// it (`FactsWatch::offered_would`), taken just before the act is typed: none where a hung
    /// call's fact rides it (`rode`, the one slot's), or the act is no longer one a fact rides.
    pub(in crate::supervise::run) fn point_would_owed(
        &mut self,
        action: &TurnEndAction,
        cfg: &SupervisorConfig,
        rode: bool,
        review: &dyn Review,
    ) -> Option<(u16, u64)> {
        self.facts
            .offered_would
            .take()
            .filter(|_| !rode && fact_may_ride(&self.turn_end, action))
            .filter(|_| {
                cfg.occupancy.grant().is_none() && self.occupancy_observes(Some(cfg), review)
            })
    }

    /// Under `observe`, the wake the act just typed would have carried under `prompt`
    /// ([`Self::wake_ride`]), `would` its ordinal and episode: its `fact-would@v1 … door=point`
    /// row, and the session's books as `prompt`'s tell would have left them (told once, its
    /// pace). Nothing is typed, claimed or told.
    pub(in crate::supervise::run) fn point_would(
        &mut self,
        (ord, episode): (u16, u64),
        seq: u64,
        review: &mut dyn Review,
    ) {
        let now_ms = self.hold_ms(self.fx.mono());
        let held_ms = self
            .hold
            .clock
            .since_ms()
            .map_or(0, |since| now_ms.saturating_sub(since).max(0));
        self.ledger_row(
            WOULD_RULE,
            Outcome::Waiting,
            "",
            &door::wake_line_words(ord, episode, held_ms, WakeDoor::Point),
            seq,
        );
        self.hold.door.books.woke(episode, now_ms);
        self.point_note(seq, ord, episode, "would", review);
    }

    fn point_note(&self, seq: u64, ord: u16, episode: u64, words: &str, review: &mut dyn Review) {
        review.note(&format!(
            "FACT seq={seq} kind={} episode={episode:016x} ord={ord} door={} {words}",
            FactKind::Wake.word(),
            WakeDoor::Point.word()
        ));
    }
}
