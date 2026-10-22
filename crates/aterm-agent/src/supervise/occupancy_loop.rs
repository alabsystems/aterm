// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD'S CLOCK AND THE OCCUPANCY DOOR IN THE LOOP (`docs/DESIGN-harness-occupancy-2026-10-20.md`
//! §3.1, §3.2 and §6; the clock itself is [`crate::supervise::occupancy::hold`], the decider
//! [`crate::supervise::occupancy::rules`]): what the loop feeds the clock, what it says of each
//! change, and what the built-in rules would do at a break. It TYPES NOTHING, asks the server
//! nothing and changes no decision.
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
//! THE DOOR AT A BREAK ([`Session::occupancy_break`]): at a settled break the screen
//! drew (never a carried one, which skips the screen's own break test: the red-team's F10), with
//! a hold open, the decider ([`rules::decide`]) judges `wait-nudge@v1` and `hold-room@v1` on:
//!
//! * the hold, from the clock;
//! * the board's reading ([`IdleHost::occupancy`], the one read the door adds), asked only where
//!   `hold-room` could be due by all but the machine ([`rules::wants_reading`]) and at most once
//!   a board look ([`ASK_EVERY_MS`], on the clock's milliseconds; the red-team's F9);
//! * guards judged ONLY from what the loop holds already (the red-team's F1): the last `status`'s
//!   person, driver and halt, the draft every read clocks, the break's own screen (a launch card
//!   with nothing asked under it: no task yet), the memory verdict and the API's reach the last
//!   points measured, the policy's own state, the brakes the loop keeps — and no `status`,
//!   `cell`, memory, taskless or reach read is made for them. The host's record of whose turns
//!   the conversation holds ([`IdleHost::taskless`]) is such a read: a session the screen draws
//!   with a message under its card is not judged taskless here. Where a brake's own test reads the clock or the
//!   host (a limit episode's spent reset, the wake's settle falling, the credits' end), the door
//!   takes the brake as standing while the loop keeps it: under `observe` that can only hold a
//!   would-be line, never type one.
//!
//! What it says: one `OCCUPANCY seq=<n> rule=<id> rev=builtin episode=<hex> door=break …` line a
//! rule a change (`would`, `held:<guard>`, `paced next_s=<n>`, `spent`), and for each would-be
//! line a `fact-would@v1` row (`waiting`), which counts toward the rule's caps and the session's
//! pace as a typed line would. Currently types nothing under any key: `prompt` reads as `observe`.
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
//! opens afresh at its own break. The would-be lines a loop before this one booked
//! ([`rules::would_rows_in`]) seed the door's books whatever became of the hold: those of the last
//! day count toward the nudges' cap and the pace, and those since a carried hold began toward its
//! own caps.

use std::time::{Duration, Instant};

use super::super::config::SupervisorConfig;
use super::super::facts::FactKind;
use super::super::memory;
use super::super::occupancy::OccupancyReading;
use super::super::occupancy::hold::{self, Close, HoldChange, HoldClock, HoldEvent, RowSaid};
use super::super::occupancy::rules::{self, Books, Guards, HoldSeen, RuleId, Verdict};
use super::super::policy::turn_end::Reach;
use super::approvals::Outcome;
use super::*;

/// How long a hold stands before it is booked in the ledger.
pub(super) const HOLD_ROW_MS: i64 = 10 * 60_000;

/// The longest a hold's last row may stand before a successor that carries it begins: the gap
/// no loop watched. A seamless update's or a restart's is seconds (a stopped loop ends within one
/// 20 s wait, and books its open hold as it ends, [`Session::hold_left`]); five minutes leaves a
/// loaded machine room and bounds what may have happened unseen.
pub(super) const CARRY_GAP_MS: i64 = 5 * 60_000;

/// How often the door asks the board at most: once a board look (the facts board is taken on the
/// window's 60 s disk look), on the clock's milliseconds.
pub(super) const ASK_EVERY_MS: i64 = 60_000;

/// The loop's hold: the clock, the origin of its milliseconds, what the ledger left to carry,
/// whether the open hold is booked, and the door's own state.
#[derive(Debug, Default)]
pub(super) struct HoldWatch {
    clock: HoldClock,
    /// The instant the clock's milliseconds count from: the first this loop gave it.
    origin: Option<Instant>,
    /// What this tab's ledger left of the hold, taken up at the first thing fed.
    seed: Option<Seed>,
    /// The open hold's open row is in the ledger.
    booked: bool,
    /// The occupancy door's ([`Session::occupancy_break`]).
    door: DoorWatch,
}

/// The occupancy door's state: its books, the board's reading last asked, what each rule last
/// said, and where the wall clock stood on the clock's milliseconds.
#[derive(Debug, Default)]
struct DoorWatch {
    /// What the session was told, or would have been ([`Books`]).
    books: Books,
    /// The board's reading last asked, with when, on the clock's milliseconds.
    asked: Option<(i64, OccupancyReading)>,
    /// What each rule last said ([`RuleId::index`]): a line once a change.
    said: [Option<Said>; 2],
    /// The clock's milliseconds and the wall's (unix ms) at the loop's start
    /// ([`Session::hold_seed_from`]): a hold's start in unix ms, for its episodes' ids.
    wall: Option<(i64, i64)>,
}

/// What a rule's last `OCCUPANCY` line said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    /// A would-be line of this episode.
    Would(u64),
    /// A guard held it.
    Held(Guard),
    /// The pace held it.
    Paced,
    /// Its cap was spent.
    Spent,
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
        let start_ms = self.hold_ms(start);
        self.hold.door.wall = Some((start_ms, now_ms));
        let would = rules::would_rows_in(body, self.setup.sid.as_deref());
        let Some(row) = hold::seed_row_in(body, self.setup.sid.as_deref()) else {
            self.door_seed(&would, start_ms, now_ms, None);
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
            self.door_seed(&would, start_ms, now_ms, None);
            return;
        }
        let since_ms = start_ms - now_ms.saturating_sub(row.since_wall_ms());
        self.hold.seed = Some(Seed::Carry {
            since_ms,
            declared: row.declared,
            seen_ms: start_ms,
        });
        self.door_seed(
            &would,
            start_ms,
            now_ms,
            Some((since_ms, row.since_wall_ms())),
        );
    }

    /// The door's books from the would-be lines a loop before this one booked (`would`, oldest
    /// first), placed on the clock by the loop's start (`start_ms`, beside the wall's `now_ms`):
    /// each of the last day counts toward the nudges' cap and the pace, and where a hold is
    /// carried (`carried`: its start on the clock and in unix ms) each booked since it began
    /// counts toward that hold's own caps, however long ago: a hold carried past a day keeps its
    /// one nudge and its `hold-room` lines (the day's window is the nudges' cap's, which
    /// [`Books`] judges by age itself).
    fn door_seed(
        &mut self,
        would: &[rules::WouldRow],
        start_ms: i64,
        now_ms: i64,
        carried: Option<(i64, i64)>,
    ) {
        let books = &mut self.hold.door.books;
        if let Some((since_ms, _)) = carried {
            books.hold(since_ms);
        }
        for row in would {
            let ago = now_ms.saturating_sub(row.row_ms);
            let this_hold = carried.is_some_and(|(_, since_wall)| row.row_ms >= since_wall);
            if ago < 0 || (!this_hold && ago >= rules::DAY_MS) {
                continue;
            }
            books.fired(row.rule, start_ms.saturating_sub(ago), this_hold);
        }
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
        // The door, at a settled break the screen drew (a carried one skips the screen's own
        // break test, F10), from this same read.
        let drawn = self.transcript_wait_seq != Some(seq);
        if settled
            && drawn
            && let Some(cfg) = cfg
        {
            self.occupancy_break(screen, at + waited, now_ms, cfg, review);
        }
    }

    /// THE OCCUPANCY DOOR AT A SETTLED, DRAWN BREAK ([module docs](self)): the built-in rules
    /// judged on the open hold at `now_ms` (the clock's; `now` the same read's instant), each
    /// change said, and each would-be line booked. Nothing is typed. The clock was fed this
    /// settled break first, which opens a hold where none stood (`HoldClock::step`), so a hold
    /// is open here; another hold's verdicts are forgotten (each hold's are its own).
    fn occupancy_break(
        &mut self,
        screen: &Screen,
        now: Instant,
        now_ms: i64,
        cfg: &SupervisorConfig,
        review: &mut dyn Review,
    ) {
        // A pre-door recording build had none of this rule observer, even
        // the nudge that needs no machine reading. Keep its 1a hold clock,
        // but add no new rule notes, row stamps or ledger outputs on replay.
        // A live empty board still permits the machine-independent nudge.
        if self.fx.replay_lacks_host_door("host.occupancy") {
            return;
        }
        let Some(since_ms) = self.hold.clock.since_ms() else {
            return;
        };
        let hold = HoldSeen {
            since_ms,
            declared: self.hold.clock.declared(),
        };
        // Another hold's verdicts are not this one's: each is said afresh.
        if self.hold.door.books.hold_since_ms() != Some(since_ms) {
            self.hold.door.said = [None; 2];
        }
        self.hold.door.books.hold(since_ms);
        let reading = if rules::wants_reading(Some(hold), &self.hold.door.books, now_ms) {
            self.occupancy_reading(now_ms)
        } else {
            OccupancyReading::default()
        };
        let guards = self.occupancy_guards(now, cfg, &screen.rows);
        let verdicts = rules::decide(&reading, Some(hold), &self.hold.door.books, &guards, now_ms);
        for (rule, verdict) in verdicts {
            self.occupancy_said(screen.seq, rule, verdict, hold, now_ms, review);
        }
    }

    /// The board's reading for this break: the one asked within the last [`ASK_EVERY_MS`], else
    /// the host asked once more (the empty reading from a loop with no host).
    fn occupancy_reading(&mut self, now_ms: i64) -> OccupancyReading {
        if let Some((asked_ms, reading)) = &self.hold.door.asked
            && (0..ASK_EVERY_MS).contains(&now_ms.saturating_sub(*asked_ms))
        {
            return reading.clone();
        }
        let reading = self
            .stall_host
            .as_ref()
            .map(|host| host.occupancy())
            .unwrap_or_default();
        self.hold.door.asked = Some((now_ms, reading.clone()));
        reading
    }

    /// THE GUARDS, FROM WHAT THE LOOP HOLDS (F1): nothing is read for them. `now` is the break
    /// read's own instant, `rows` its screen (for a credits notice drawn on it, read as the
    /// break's own credits hold reads it, its end not judged: the door takes the hold as
    /// standing while the loop keeps it; and for the launch card, the screen's own word that no
    /// task was asked yet).
    fn occupancy_guards(&self, now: Instant, cfg: &SupervisorConfig, rows: &[String]) -> Guards {
        let reader = self.reader(rows);
        let credits =
            !cfg.usage_credits && (self.credits_kept.is_some() || reader.credits(rows).is_some());
        let brake = [
            (self.stalled.is_some(), Guard::Stall),
            (self.limit.is_some(), Guard::LimitEpisode),
            (self.turn_end.act_untaken(), Guard::ActUntaken),
            (self.wake_settle.is_some(), Guard::WakeSettle),
            (credits, Guard::Credits),
        ]
        .into_iter()
        .find_map(|(holds, guard)| holds.then_some(guard));
        let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
        let person = self.driver
            || self
                .person_at()
                .and_then(|typed| typed.checked_add(grace))
                .is_some_and(|until| until > now);
        Guards {
            brake,
            person,
            halted: self.halted,
            draft: self.draft_seen.is_some(),
            taskless: reader.fresh(rows),
            exchange: self.turn_end.left_to_person(),
            memory: self.memory_seen.is_some()
                || matches!(
                    self.memory_told,
                    Some(memory::Verdict::Restart | memory::Verdict::Person)
                ),
            reach_down: matches!(self.reach_seen, Some(Reach::Down { .. })),
            mode: cfg.autopilot_for(self.autopilot.word()),
        }
    }

    /// One rule's verdict at a break: its line where it changed, and a would-be line's row.
    fn occupancy_said(
        &mut self,
        seq: u64,
        rule: RuleId,
        verdict: Verdict,
        hold: HoldSeen,
        now_ms: i64,
        review: &mut dyn Review,
    ) {
        let episode = rules::episode(
            rule,
            self.hold_wall_ms(hold.since_ms),
            self.hold.door.books.this_hold(rule),
        );
        let (said, words) = match &verdict {
            Verdict::Quiet => (None, String::new()),
            Verdict::Spent => (Some(Said::Spent), "spent".to_string()),
            Verdict::Held(guard) => (Some(Said::Held(*guard)), format!("held:{}", guard.word())),
            Verdict::Paced { next_ms } => (
                Some(Said::Paced),
                format!(
                    "paced next_s={}",
                    next_ms.saturating_sub(now_ms).max(0) / 1000
                ),
            ),
            Verdict::Due(_) => (Some(Said::Would(episode)), "would".to_string()),
        };
        let last = &mut self.hold.door.said[rule.index()];
        if *last == said {
            return;
        }
        *last = said;
        if said.is_none() {
            return;
        }
        review.note(&format!(
            "OCCUPANCY seq={seq} rule={} rev=builtin episode={episode:016x} door=break {words}",
            rule.name()
        ));
        if let Verdict::Due(_) = verdict {
            let held_ms = now_ms.saturating_sub(hold.since_ms);
            self.ledger_row(
                rules::WOULD_RULE,
                Outcome::Waiting,
                "",
                &rules::would_reason(rule, episode, held_ms, hold.declared),
                seq,
            );
            self.hold.door.books.fired(rule, now_ms, true);
        }
    }

    /// The hold begun at `since_ms` (the clock's) in unix ms, where the loop placed the wall on
    /// the clock at its start; else the clock's own milliseconds.
    fn hold_wall_ms(&self, since_ms: i64) -> i64 {
        self.hold.door.wall.map_or(since_ms, |(clock_ms, wall_ms)| {
            wall_ms.saturating_add(since_ms.saturating_sub(clock_ms))
        })
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
