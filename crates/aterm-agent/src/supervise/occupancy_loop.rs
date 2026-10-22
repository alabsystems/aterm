// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD'S CLOCK IN THE LOOP (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.1 and §6; the
//! clock itself is [`crate::supervise::occupancy::hold`]): what the loop feeds it, and what it
//! says of each change. It TYPES NOTHING, asks the server nothing and changes no decision.
//!
//! WHERE IT RUNS (the red-team's F2 and F4): on a HOSTED loop (a window's host, `stall_host`),
//! run UNATTENDED, with a policy whose `occupancy` is not `off`. Anywhere else nothing is fed,
//! said or booked.
//!
//! WHAT IT IS FED, from reads the loop makes anyway — no new read of the server, the host or the
//! clock, save the host's [`IdleHost::launch`] where the ledger holds a hold to carry:
//!
//! * a break of the agent's own background work, offered or withheld
//!   (`Session::host_steps_in_background`), with the break's start and how long it has stood,
//!   which the loop read for its settle;
//! * a line the host typed at a break;
//! * every turn read running, before it is marked taken (`Session::took`, its every caller): the
//!   edge into it is ours where the policy's act or the host's line is still untaken
//!   ([`TurnEndState::typed_untaken`]);
//! * a decided point (`turn_end_execute`), and the host's step that acted at an idle point.
//!
//! WHAT IT SAYS: one journal line a change, `OCCUPANCY seq=<n> hold open|declared|closed …`, and
//! `fact-hold@v1` ledger rows (`waiting`, so no act, mark or count follows them) for a hold that
//! has stood [`HOLD_ROW_MS`]: its open row then, a declaration and its close after, and its open
//! row again as the loop ends with it open ([`Session::hold_left`]). A shorter hold books
//! nothing, so the ledger's newest-rows ring is not spent on every short break.
//!
//! A SUCCESSOR CARRIES THE HOLD (a seamless update, a loop restarted on the same agent) only from
//! a loop that watched it until just before this one began (the red-team's F3): the last
//! `fact-hold@v1` row of this tab's own ledger leaves a hold open, it is the last row of the
//! session's under any rule that is no fact's, it is at most [`CARRY_GAP_MS`] old, Off was not
//! lifted since ([`IdleHost::lifted_off`]), and the agent's process was not launched after it
//! (P3, [`IdleHost::launch`]). Then it is taken up at the first thing the clock is fed. Anything
//! else is a hold that may have ended where no loop watched — a loop stopped for a day, one that
//! ran with `occupancy = "off"`, attended or with no host, a person who drove the session under
//! Off — and the stale row is closed (`why=stale`) so no later loop carries it either; the clock
//! opens afresh at its own break.

use std::time::{Duration, Instant};

use super::super::config::SupervisorConfig;
use super::super::facts::FactKind;
use super::super::occupancy::hold::{self, Close, HoldChange, HoldClock, HoldEvent, RowSaid};
use super::approvals::Outcome;
use super::*;

/// How long a hold stands before it is booked in the ledger.
pub(super) const HOLD_ROW_MS: i64 = 10 * 60_000;

/// The longest a hold's last row may stand before a successor that carries it begins: the gap
/// no loop watched. A seamless update's or a restart's is seconds (a stopped loop ends within one
/// 20 s wait, and books its open hold as it ends, [`Session::hold_left`]); five minutes leaves a
/// loaded machine room and bounds what may have happened unseen.
pub(super) const CARRY_GAP_MS: i64 = 5 * 60_000;

/// The loop's hold: the clock, the origin of its milliseconds, what the ledger left to carry, and
/// whether the open hold is booked.
#[derive(Debug, Default)]
pub(super) struct HoldWatch {
    clock: HoldClock,
    /// The instant the clock's milliseconds count from: the first this loop gave it.
    origin: Option<Instant>,
    /// What this tab's ledger left of the hold, taken up at the first thing fed.
    seed: Option<Seed>,
    /// The open hold's open row is in the ledger.
    booked: bool,
}

/// What a loop before this one left of the hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seed {
    /// Its open hold, on the clock's milliseconds, to carry.
    Carry {
        since_ms: i64,
        declared: bool,
        seen_ms: i64,
    },
    /// Its open hold, which an agent launched since never saw: to close.
    Stale { held_ms: i64 },
}

/// `at` in milliseconds from `origin`, before it negative.
fn signed_ms(origin: Instant, at: Instant) -> i64 {
    let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    match at.checked_duration_since(origin) {
        Some(after) => ms(after),
        None => -ms(origin.duration_since(at)),
    }
}

impl<C: Ctl> Session<'_, C> {
    /// Whether this loop measures the hold: a hosted loop, run unattended, under a policy whose
    /// `occupancy` is not `off` (F2, F4).
    fn occupancy_observes(&self, cfg: Option<&SupervisorConfig>, review: &dyn Review) -> bool {
        self.stall_host.is_some()
            && review.unattended()
            && cfg.is_some_and(|cfg| cfg.occupancy.observes())
    }

    /// `at` on the clock's milliseconds.
    fn hold_ms(&mut self, at: Instant) -> i64 {
        let origin = *self.hold.origin.get_or_insert(at);
        signed_ms(origin, at)
    }

    /// THE HOLD THIS TAB'S LEDGER LEFT OPEN ([`hold::seed_row_in`] of its own text `body`, read
    /// once at the loop's start), to carry or to close at the first thing fed: carried only where
    /// its row is the session's last word on it, at most [`CARRY_GAP_MS`] old, Off was not lifted
    /// since (`lifted_off`, the host's answer the loop's start already read) and the agent's
    /// process is not newer ([module docs](self)). `start` is the loop's first monotonic read
    /// and `now_ms` its wall read beside the ledger's: the pair that puts the row's wall time on
    /// the clock.
    pub(super) fn hold_seed_from(
        &mut self,
        body: &str,
        start: Instant,
        now_ms: i64,
        lifted_off: bool,
        cfg: &SupervisorConfig,
        review: &dyn Review,
    ) {
        if !self.occupancy_observes(Some(cfg), review) {
            return;
        }
        let Some(row) = hold::seed_row_in(body, self.setup.sid.as_deref()) else {
            return;
        };
        // The host's launch is asked last, and only of a row nothing else made stale.
        let stale = lifted_off
            || row.followed
            || now_ms.saturating_sub(row.row_ms) > CARRY_GAP_MS
            || self
                .stall_host
                .as_ref()
                .and_then(|host| host.launch())
                .is_some_and(|launched| launched > row.row_ms);
        if stale {
            self.hold.seed = Some(Seed::Stale {
                held_ms: row.held_ms,
            });
            return;
        }
        let start_ms = self.hold_ms(start);
        self.hold.seed = Some(Seed::Carry {
            since_ms: start_ms - now_ms.saturating_sub(row.since_wall_ms()),
            declared: row.declared,
            seen_ms: start_ms,
        });
    }

    /// The seed, taken up once: a carried hold opens (booked already: its row stands), a stale
    /// one is closed in the ledger.
    fn hold_take_seed(&mut self, seq: u64, review: &mut dyn Review) {
        match self.hold.seed.take() {
            None => {}
            Some(Seed::Stale { held_ms }) => {
                self.hold_said(
                    seq,
                    HoldChange::Closed {
                        why: Close::Stale,
                        held_ms,
                    },
                    review,
                );
            }
            Some(Seed::Carry {
                since_ms,
                declared,
                seen_ms,
            }) => {
                if let Some(change) = self.hold.clock.seed(since_ms, declared, seen_ms) {
                    self.hold.booked = true;
                    self.hold_said(seq, change, review);
                }
            }
        }
    }

    /// One event, with the seed taken up first and the change said.
    fn hold_step(&mut self, seq: u64, event: HoldEvent, review: &mut dyn Review) {
        self.hold_take_seed(seq, review);
        if let Some(change) = self.hold.clock.step(event) {
            self.hold_said(seq, change, review);
        }
    }

    /// A READ OF THE AGENT'S OWN BACKGROUND WORK at a break begun at `at`, standing `waited`
    /// (the loop's own read for its settle), `settled` once past it. At a settled read after a
    /// line the harness typed, the agent's last words are read for `WAITING:` (and kept for
    /// nothing else); a hold that has stood [`HOLD_ROW_MS`] is booked.
    pub(super) fn hold_background(
        &mut self,
        screen: &Screen,
        at: Instant,
        waited: Duration,
        settled: bool,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
        if !self.occupancy_observes(cfg, review) {
            return;
        }
        let since_ms = self.hold_ms(at);
        let now_ms = since_ms.saturating_add(i64::try_from(waited.as_millis()).unwrap_or(i64::MAX));
        let seq = screen.seq;
        self.hold_step(
            seq,
            HoldEvent::Background {
                since_ms,
                now_ms,
                settled,
            },
            review,
        );
        if settled && self.hold.clock.wants_tail() {
            let waiting_line = self
                .reader(&screen.rows)
                .said_tail(&screen.rows)
                .is_some_and(|said| hold::waiting_declared(&said));
            self.hold_step(seq, HoldEvent::AnswerTail { waiting_line }, review);
        }
        if !self.hold.booked
            && let Some(held_ms) = self.hold.clock.held_ms(now_ms)
            && held_ms >= HOLD_ROW_MS
        {
            self.hold.booked = true;
            let declared = self.hold.clock.declared();
            self.hold_row(seq, RowSaid::Open { held_ms, declared });
        }
    }

    /// The host typed a line at a break: its turn and the point that ends it are the harness's.
    pub(super) fn hold_line(
        &mut self,
        seq: u64,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
        if self.occupancy_observes(cfg, review) {
            self.hold_step(seq, HoldEvent::HarnessLine, review);
        }
    }

    /// A read of a live turn, before the loop marks its act taken (`Session::took`, which every
    /// such read goes through, calls it first): ours where the policy's act or the host's line
    /// is still untaken. The clock classifies only the edge into it.
    pub(super) fn hold_live(
        &mut self,
        seq: u64,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
        if self.occupancy_observes(cfg, review) {
            let ours = self.turn_end.typed_untaken();
            self.hold_step(seq, HoldEvent::LiveTurn { ours }, review);
        }
    }

    /// A decided point on `screen`: `acts` where its action types, escalates or restarts. While
    /// a hold is open the point is read for the agent's background work as the loop reads a
    /// break: the screen reader's `background_wait`, or the session's transcript's verdict kept
    /// for this very screen (`transcript_wait_seq`, BG-1; the loop reads such a screen busy, so
    /// no point is decided on it today, and none ever closes the hold over the transcript's
    /// wait). Nothing new is read. A shell counted on a screen where a message was just
    /// submitted under the status row is no background wait (the turn that message began is
    /// someone's), so it keeps no hold.
    pub(super) fn hold_point(
        &mut self,
        screen: &Screen,
        acts: bool,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) {
        if !self.occupancy_observes(cfg, review) {
            return;
        }
        let rows = &screen.rows;
        let background = self.hold.clock.since_ms().is_some()
            && (self.reader(rows).background_wait(rows).is_some()
                || self.transcript_wait_seq == Some(screen.seq));
        self.hold_step(
            screen.seq,
            HoldEvent::DecidedPoint { acts, background },
            review,
        );
    }

    /// THE LOOP ENDS WITH THE HOLD OPEN (`Session::drive`, any end but the session's own): a
    /// booked hold's open row is written again, held to the last read that saw it, so a
    /// successor that begins within [`CARRY_GAP_MS`] carries it ([`Self::hold_seed_from`]). An
    /// open hold not booked yet, a seed never taken up (the row it would carry stands as it
    /// was), or no hold: nothing. Nothing is read but the row's own stamp.
    pub(super) fn hold_left(&mut self, cfg: Option<&SupervisorConfig>, review: &dyn Review) {
        if !self.hold.booked || !self.occupancy_observes(cfg, review) {
            return;
        }
        if let Some(held_ms) = self.hold.clock.seen_held_ms() {
            let declared = self.hold.clock.declared();
            self.hold_row(0, RowSaid::Open { held_ms, declared });
        }
    }

    /// One change, said: its journal line, and its row where the hold is booked (a stale seed's
    /// close always: its open row stands).
    fn hold_said(&mut self, seq: u64, change: HoldChange, review: &mut dyn Review) {
        let s = |ms: i64| ms / 1000;
        let words = match change {
            HoldChange::Opened {
                seeded: false,
                declared,
                ..
            } => format!("hold open{}", if declared { " declared" } else { "" }),
            HoldChange::Opened {
                seeded: true,
                declared,
                ..
            } => format!(
                "hold open why=seeded held_s={}{}",
                s(self.hold.clock.seen_held_ms().unwrap_or(0)),
                if declared { " declared" } else { "" }
            ),
            HoldChange::Declared { held_ms } => format!("hold declared held_s={}", s(held_ms)),
            HoldChange::Closed { why, held_ms } => {
                format!("hold closed why={} held_s={}", why.word(), s(held_ms))
            }
        };
        review.note(&format!("OCCUPANCY seq={seq} {words}"));
        let row = match change {
            HoldChange::Opened { .. } => None,
            HoldChange::Declared { held_ms } => {
                self.hold.booked.then_some(RowSaid::Declared { held_ms })
            }
            HoldChange::Closed { why, held_ms } => (std::mem::take(&mut self.hold.booked)
                || why == Close::Stale)
                .then_some(RowSaid::Closed { why, held_ms }),
        };
        if let Some(said) = row {
            self.hold_row(seq, said);
        }
    }

    /// A `fact-hold@v1` row: `waiting`, so nothing follows it but the row.
    fn hold_row(&mut self, seq: u64, said: RowSaid) {
        self.ledger_row(
            FactKind::Hold.rule(),
            Outcome::Waiting,
            "",
            &hold::row_reason(&said),
            seq,
        );
    }
}
