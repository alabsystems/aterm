// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD'S CLOCK AND THE OCCUPANCY DOOR IN THE LOOP (`docs/DESIGN-harness-occupancy-2026-10-20.md`
//! §3.1, §3.2 and §6; the clock itself is [`crate::supervise::occupancy::hold`], the decider
//! [`crate::supervise::occupancy::rules`]): what the loop feeds the clock, what it says of each
//! change, and what the built-in rules would do at a break. Under `off` and `observe` it TYPES
//! NOTHING, asks the server nothing and changes no decision; under the owner's `prompt` alone the
//! door types its line at a drawn break ([`Session::occupancy_write`]).
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
//!   points measured, the policy's own state (a line of the policy's, the door's or the host's
//!   the agent has not taken among it), the brakes the loop keeps — and no `status`,
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
//! pace as a typed line would. A held verdict is booked as a `fact-held@v1` row (`waiting`; design
//! §6) on its transition only, and once an episode a guard ([`Session::held_row`]): a guard that
//! flaps, or one that holds a rule for a whole day, spends a row or two of the ledger's
//! newest-rows ring, never one a read.
//!
//! UNDER `prompt` ([`Session::occupancy_write`]; the red-team's F1, F10, F11 and F12) the due line
//! is no would-be row: the break's step, once the host's own step is judged, takes it to the
//! write, where — and only there, with the owner's grant in hand — the door makes its reads: one
//! `status`, parsed apart from the loop's fields; the guards again with that read, a draft in the
//! composer (an unknown one is a draft) and the host's record of a taskless session; a manager
//! waiting on the session holds it; then the host's withhold and wind-down, the upgrade's notice
//! due (a nudge waits for it), and the host's pace (`IdleHost::occupancy_pace`). `hold-room`'s
//! line types the board's figures, so its pace's answer carries the board the host re-read for
//! the write (design §3.1; [`PaceGrant::slot_answer`]): the rule's terms are judged again on it,
//! its figures must have been read no more than `FACT_FRESH_MS` before the break, and the line
//! typed is made from them; terms no longer met or figures stale hold it (`paced why=unmet` or
//! `why=stale`) and it stays due, tried again at the next look. Then a `fact-intent@v1` row, the
//! fenced write (`Session::type_fact`: never the guarded submit), and after it the line's own
//! turn state (`TurnEndState::fact_typed`), the facts state, the books, the host's
//! `break_taken` and `inform`, and the outcome's `fact-hold@v1` row, `typed` or
//! `skipped why=<word>` (`unfenced`, `not-written`, or `not-submitted` for a line written and left
//! in the composer). Its `OCCUPANCY` line says `typed`, `skipped why=…`, `held:<guard>`,
//! `told`, `paced why=…` or `refused why=…`, once a change. A typed line is booked as a would-be
//! line is, and a successor seeds its facts from the door's rows ([`door::seed_rows_in`]): an
//! intent with no outcome after it is untold, so a crash between them retells it once.
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

use super::super::config::{Occupancy, SupervisorConfig};
use super::super::facts::{EpisodeKey, FactKind, FactsState};
use super::super::memory;
use super::super::occupancy::door::{self, AtWrite, PaceGrant, StatusSeen, Told};
use super::super::occupancy::fault::{Fault, armed};
use super::super::occupancy::hold::{self, Close, HoldChange, HoldClock, HoldEvent, RowSaid};
use super::super::occupancy::rules::{self, Books, Guards, HoldSeen, RuleId, Verdict};
use super::super::occupancy::{
    FactLine, OccupancyReading, OccupancySaid, SaidHold, SaidRule, SaidVerdict,
};
use super::super::policy::turn_end::{HostTurn, Reach};
use super::approvals::Outcome;
use super::turn_end_loop::Written;
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
pub(super) const ASK_EVERY_MS: i64 = super::super::occupancy::reading::LOOK_EVERY_MS;

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
    /// Under `prompt`, this break's due line, for the write at the same break
    /// ([`Session::occupancy_write`]).
    due: Option<DueLine>,
    /// Which episodes the session was told, seeded from the ledger's fact rows.
    facts: FactsState,
    /// The episode whose write last made its reads and wrote nothing, with when it may be tried
    /// again (the clock's): a skipped write once the session's pace has passed, so a host that
    /// cannot fence books no rows every break; a held one at the next board look
    /// ([`ASK_EVERY_MS`]), so a person's keys or a machine pace cost one `status` read, one
    /// composer read and one pace a look, not one a settled read (the safety review's F-D).
    again: Option<(u64, i64)>,
    /// When each rule's word last changed ([`Self::said`]), unix ms.
    said_at: [i64; 2],
    /// What the last break judged beside the verdicts: the ready proxy, the guards, and the
    /// reading the decider judged ([`OccupancySaid`]).
    seen: Option<(bool, Guards, OccupancyReading)>,
    /// What the host was last told ([`Session::occupancy_tell`]): once a change.
    told: Option<OccupancySaid>,
    /// The open hold's held verdicts booked as `fact-held@v1` rows ([`Session::held_row`]):
    /// each episode by its rule and its ordinal in the hold, with the guard that held it. Of the
    /// open hold alone, so keyed by nothing that names the hold: cleared when the hold changes,
    /// and seeded from the carried hold's own rows ([`Session::door_seed`]), however far the
    /// successor's wall start for it sits from its predecessor's (`rules::episode`).
    held: Vec<HeldKey>,
}

/// A held verdict of the open hold, as [`DoorWatch::held`] keeps it: the rule, the episode's
/// ordinal in the hold ([`rules::Books::this_hold`]), and the guard that held it.
type HeldKey = (RuleId, usize, Guard);

/// A due line under `prompt`, between the decider and the write at the same break.
#[derive(Debug, Clone)]
struct DueLine {
    rule: RuleId,
    line: FactLine,
    episode: u64,
    /// The episode's ordinal in the hold ([`rules::Books::this_hold`]).
    ordinal: usize,
    hold: HoldSeen,
    /// The break's read, on the clock's milliseconds and as an instant.
    now_ms: i64,
    now: Instant,
    seq: u64,
}

/// What the door's write came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrote {
    /// A guard held it at the write.
    Held(Guard),
    /// The episode was told already (a predecessor's row).
    Told,
    /// The host's pace held or refused it.
    Paced(PaceGrant),
    /// Typed and submitted.
    Typed,
    /// Its intent was booked, and nothing was written: why.
    Skipped(&'static str),
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
    /// Under `prompt`: this episode typed.
    Typed(u64),
    /// Under `prompt`: this episode's write skipped, and why.
    Skipped(u64, &'static str),
    /// Under `prompt`: this episode told already.
    Told(u64),
    /// Under `prompt`: the host's pace held it.
    HostPaced(PaceGrant),
}

impl Said {
    /// The word the host is told ([`SaidVerdict`]).
    const fn verdict(self) -> SaidVerdict {
        match self {
            Said::Would(_) => SaidVerdict::Would,
            Said::Held(guard) => SaidVerdict::Held(guard),
            Said::Paced => SaidVerdict::Paced,
            Said::Spent => SaidVerdict::Spent,
            Said::Typed(_) => SaidVerdict::Typed,
            Said::Skipped(_, why) => SaidVerdict::Skipped(why),
            Said::Told(_) => SaidVerdict::Told,
            Said::HostPaced(pace) => SaidVerdict::HostPaced(pace),
        }
    }
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
        let facts = door::seed_rows_in(body, self.setup.sid.as_deref());
        self.hold.door.facts.seed(&facts, now_ms);
        let would = rules::would_rows_in(body, self.setup.sid.as_deref());
        let held = rules::held_rows_in(body, self.setup.sid.as_deref());
        let booked = (&would[..], &held[..]);
        let Some(row) = hold::seed_row_in(body, self.setup.sid.as_deref()) else {
            self.door_seed(booked, start_ms, now_ms, None);
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
            self.door_seed(booked, start_ms, now_ms, None);
            return;
        }
        let since_ms = start_ms - now_ms.saturating_sub(row.since_wall_ms());
        self.hold.seed = Some(Seed::Carry {
            since_ms,
            declared: row.declared,
            seen_ms: start_ms,
        });
        self.door_seed(
            booked,
            start_ms,
            now_ms,
            Some((since_ms, row.since_wall_ms())),
        );
    }

    /// The door's books from the would-be lines and the held verdicts a loop before this one
    /// booked (`would` and `held`, oldest first), placed on the clock by the loop's start
    /// (`start_ms`, beside the wall's `now_ms`): each line of the last day counts toward the
    /// nudges' cap and the pace, and where a hold is carried (`carried`: its start on the clock
    /// and in unix ms) each booked since it began counts toward that hold's own caps, however
    /// long ago: a hold carried past a day keeps its one nudge and its `hold-room` lines (the
    /// day's window is the nudges' cap's, which [`Books`] judges by age itself). The held
    /// verdicts booked since the carried hold began are its own ([`DoorWatch::held`]); any other
    /// is another hold's, and none is kept where no hold is carried.
    fn door_seed(
        &mut self,
        (would, held): (&[rules::WouldRow], &[rules::HeldRow]),
        start_ms: i64,
        now_ms: i64,
        carried: Option<(i64, i64)>,
    ) {
        let this_hold = |row_ms: i64| carried.is_some_and(|(_, since_wall)| row_ms >= since_wall);
        self.hold.door.held = held
            .iter()
            .filter(|row| this_hold(row.row_ms))
            .map(|row| (row.rule, row.ordinal, row.guard))
            .collect();
        let books = &mut self.hold.door.books;
        if let Some((since_ms, _)) = carried {
            books.hold(since_ms);
        }
        for row in would {
            let ago = now_ms.saturating_sub(row.row_ms);
            let this_hold = this_hold(row.row_ms);
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
        self.hold.door.due = None;
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
        // Another hold's verdicts are not this one's: each is said, and booked, afresh.
        if self.hold.door.books.hold_since_ms() != Some(since_ms) {
            self.hold.door.said = [None; 2];
            self.hold.door.said_at = [0; 2];
            if !armed(Fault::HeldAcrossHolds) {
                self.hold.door.held.clear();
            }
        }
        self.hold.door.books.hold(since_ms);
        let reading = if rules::wants_reading(Some(hold), &self.hold.door.books, now_ms) {
            self.occupancy_reading(now, now_ms)
        } else {
            OccupancyReading::default()
        };
        let guards = self.occupancy_guards(now, cfg, &screen.rows);
        let nudge_after_ms =
            i64::try_from(self.setup.occupancy_nudge_after.as_millis()).unwrap_or(i64::MAX);
        let verdicts = rules::decide(
            &reading,
            Some(hold),
            &self.hold.door.books,
            &guards,
            nudge_after_ms,
            now_ms,
        );
        // A tape from a build with no write (no `host.occupancy_pace` door: 1b's, which read
        // `prompt` as `observe`) replays as it was recorded: its due lines are would-be rows.
        let prompt = cfg.occupancy.grant().is_some()
            && !self.fx.replay_lacks_host_door("host.occupancy_pace");
        let ready = rules::ready(&reading, Some(hold), now_ms);
        for (rule, verdict) in verdicts {
            if prompt && let Verdict::Due(line) = &verdict {
                self.occupancy_due(screen.seq, rule, line, hold, (now_ms, now));
                continue;
            }
            self.occupancy_said(screen.seq, rule, verdict, hold, now_ms, review);
        }
        self.hold.door.seen = Some((ready, guards, reading));
        // The mode the break ran under: a tape from a build with no write ran `prompt` as
        // `observe`, and says so.
        let mode = if cfg.occupancy == Occupancy::Prompt && !prompt {
            Occupancy::Observe
        } else {
            cfg.occupancy
        };
        self.occupancy_tell(mode, self.hold_wall_ms(now_ms));
    }

    /// WHAT THE DOOR SAYS NOW, TOLD THE HOST once a change (`IdleHost::occupancy_said`,
    /// design §6): the mode the break ran under, the open hold, the last break's ready proxy,
    /// first guard and reading, and each rule's last word, composed from what the loop holds.
    /// Nothing is read for it and nothing typed. A tape from a build with no such door replays
    /// as it was recorded: nothing is told.
    fn occupancy_tell(&mut self, mode: Occupancy, at_ms: i64) {
        if self.fx.replay_lacks_host_door("host.occupancy_said") {
            return;
        }
        let Some(host) = self.stall_host.clone() else {
            return;
        };
        let hold = self.hold.clock.since_ms().map(|since_ms| SaidHold {
            since_ms: self.hold_wall_ms(since_ms),
            declared: self.hold.clock.declared(),
        });
        let door = &self.hold.door;
        let rules = RuleId::ALL.map(|rule| {
            let said = door.said[rule.index()].filter(|_| hold.is_some());
            SaidRule {
                rule,
                verdict: said.map_or(SaidVerdict::Quiet, Said::verdict),
                at_ms: if said.is_some() {
                    door.said_at[rule.index()]
                } else {
                    0
                },
            }
        });
        let (ready, guards, reading) = door.seen.map_or(
            (false, None, OccupancyReading::default()),
            |(ready, guards, reading)| (ready, Some(guards), reading),
        );
        let said = OccupancySaid {
            mode,
            hold,
            ready: ready && hold.is_some(),
            guards,
            rules,
            at_ms,
            reading,
        };
        if door
            .told
            .as_ref()
            .is_some_and(|told| told.same_change(&said))
        {
            return;
        }
        host.occupancy_said(&said);
        self.hold.door.told = Some(said);
    }

    /// UNDER `prompt`, A DUE LINE kept for the write at this same break
    /// ([`Self::occupancy_write`]), which the break's step makes once the host's own step is
    /// judged (§2.1: the host's line, the upgrade's notice, goes first). Nothing is said yet. An
    /// episode whose write made its reads and wrote nothing waits until it may be tried again
    /// (`DoorWatch::again`).
    fn occupancy_due(
        &mut self,
        seq: u64,
        rule: RuleId,
        line: &FactLine,
        hold: HoldSeen,
        (now_ms, now): (i64, Instant),
    ) {
        let ordinal = self.hold.door.books.this_hold(rule);
        let episode = rules::episode(rule, self.hold_wall_ms(hold.since_ms), ordinal);
        if self
            .hold
            .door
            .again
            .is_some_and(|(waits, at_ms)| waits == episode && now_ms < at_ms)
        {
            return;
        }
        self.hold.door.due = Some(DueLine {
            rule,
            line: line.clone(),
            episode,
            ordinal,
            hold,
            now_ms,
            now,
            seq,
        });
    }

    /// THE DOOR'S WRITE UNDER `prompt`, at the break whose decider left a line due
    /// (`docs/DESIGN-harness-occupancy-2026-10-20.md` §2.1; the red-team's F1, F10, F11): the
    /// break's step calls it last, with `before` the guard that already held the break (a brake,
    /// or [`Guard::HostLine`] where the host typed its line, which [`door::write_held`] judges
    /// first). Only under the owner's grant, and in this order: the break's one line not gone
    /// already; the episode not told already; one `status` read, parsed apart from the
    /// loop's own fields; the full guard set again (that read's person, driver, halt and unread
    /// input, a draft in the composer, the host's record of a taskless session); the host's
    /// withhold and its wind-down, then the upgrade's notice due (a nudge waits for it); the
    /// host's pace, with a slot rule's figures re-read in it ([`PaceGrant::slot_answer`]: the
    /// line typed is made from them); the `fact-intent@v1` row; the fenced write
    /// ([`Self::type_fact`]); then the bookkeeping and the outcome's `fact-hold@v1` row, and one
    /// `OCCUPANCY` line once a change.
    /// Whether the line was typed.
    pub(super) fn occupancy_write(
        &mut self,
        screen: &Screen,
        before: Option<Guard>,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(mut due) = self.hold.door.due.take() else {
            return Ok(false);
        };
        let (Some(cfg), Some(host)) = (cfg, self.stall_host.clone()) else {
            return Ok(false);
        };
        let Some(grant) = cfg.occupancy.grant() else {
            return Ok(false);
        };
        if due.seq != screen.seq {
            return Ok(false);
        }
        let key = EpisodeKey {
            kind: FactKind::Hold,
            id: due.episode,
        };
        let line_gone = before == Some(Guard::HostLine);
        let gone = AtWrite {
            host_line: line_gone,
            ..AtWrite::default()
        };
        let wrote = if let Some(guard) = before.filter(|_| !line_gone) {
            Wrote::Held(guard)
        } else if let Some(guard) = door::write_held(due.rule, gone) {
            Wrote::Held(guard)
        } else if self.hold.door.facts.is_told(key) {
            Wrote::Told
        } else {
            let wrote = if let Some(guard) = self.door_holds(screen, &due, cfg, host.as_ref())? {
                Wrote::Held(guard)
            } else {
                let mut pace = host.occupancy_pace(due.rule, self.hold_wall_ms(due.hold.since_ms));
                if due.rule.slot() {
                    // The figures typed are those the host re-read for this write (design §3.1).
                    let at_ms = self.wall_at(due.now);
                    (pace, due.line) = pace.slot_answer(&due.line, due.hold, due.now_ms, at_ms);
                }
                if pace.lets() {
                    self.door_types(&grant, screen, &due, key, cfg, host.as_ref(), review)?
                } else {
                    Wrote::Paced(pace)
                }
            };
            // Its reads made and nothing written: tried again at the next look (a skip has
            // set its own wait).
            if matches!(wrote, Wrote::Held(_) | Wrote::Paced(_)) && !armed(Fault::DoorRereads) {
                self.hold.door.again = Some((due.episode, due.now_ms.saturating_add(ASK_EVERY_MS)));
            }
            wrote
        };
        self.door_said(&due, wrote, review);
        self.occupancy_tell(cfg.occupancy, self.hold_wall_ms(due.now_ms));
        Ok(wrote == Wrote::Typed)
    }

    /// What holds the due line at the write, past the decider: the `status` read and the full
    /// guard set again, then the host's withhold, its wind-down and the upgrade's notice
    /// ([`door::write_held`]). `None`: nothing.
    fn door_holds(
        &mut self,
        screen: &Screen,
        due: &DueLine,
        cfg: &SupervisorConfig,
        host: &dyn IdleHost,
    ) -> Result<Option<Guard>, Fail> {
        let seen = if armed(Fault::DoorSkipsStatus) {
            StatusSeen {
                human: super::super::screen::HumanInput::Never,
                driver: false,
                halted: false,
                input_unread: false,
            }
        } else {
            let r = self.call(&["status"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
            }
            let Some(seen) = r.ok().then(|| StatusSeen::of(&r.stdout)).flatten() else {
                return Ok(Some(Guard::Person));
            };
            seen
        };
        let mut guards = self.occupancy_guards(due.now, cfg, &screen.rows);
        guards.person |= seen.person(cfg.human_grace_s);
        guards.halted |= seen.halted;
        guards.draft |= seen.input_unread || self.door_draft(screen) != Some(false);
        guards.taskless |= host.taskless();
        if armed(Fault::DoorIgnoresPerson) {
            guards.person = false;
            guards.halted = false;
        }
        if armed(Fault::DoorIgnoresDraft) {
            guards.draft = false;
        }
        let brakes = !armed(Fault::DoorIgnoresBrakes);
        if !brakes {
            guards.brake = None;
            guards.memory = false;
            guards.reach_down = false;
        }
        if let Some(guard) = guards.held(due.rule) {
            return Ok(Some(guard));
        }
        let withheld = brakes && (host.withholds_turn_end() || host.owns_turn_end());
        // The break's line gone was judged before anything was read.
        let at = AtWrite {
            host_line: false,
            withheld,
            upgrade_due: !withheld && host.upgrade_due(),
        };
        Ok(door::write_held(due.rule, at))
    }

    /// Whether the composer the break's screen draws holds a draft: `Some(true)` a draft,
    /// `None` unknown — which the door reads as a draft (F10).
    fn door_draft(&mut self, screen: &Screen) -> Option<bool> {
        let reader = self.reader(&screen.rows);
        let reading = reader.read(&screen.rows, Some(screen.cursor_col));
        let (placeholder, hint_fg) = (
            reading.program.reader().placeholder(),
            reading.program.reader().placeholder_fg(),
        );
        self.composer_draft(reading.composer.as_ref(), placeholder, hint_fg, screen)
    }

    /// The write itself, every guard passed: the intent row, the fenced write, and after it the
    /// line's bookkeeping and its outcome row.
    #[allow(clippy::too_many_arguments)]
    fn door_types(
        &mut self,
        grant: &super::super::config::PromptGrant,
        screen: &Screen,
        due: &DueLine,
        key: EpisodeKey,
        cfg: &SupervisorConfig,
        host: &dyn IdleHost,
        review: &mut dyn Review,
    ) -> Result<Wrote, Fail> {
        let held_ms = due.now_ms.saturating_sub(due.hold.since_ms);
        let (rule, episode, declared) = (due.rule, due.episode, due.hold.declared);
        self.ledger_row(
            door::INTENT_RULE,
            Outcome::Waiting,
            "",
            &door::intent_reason(rule, episode, held_ms, declared),
            due.seq,
        );
        let text = due.line.text();
        let outcome_row = |s: &mut Self, told: Told| {
            let outcome = match told {
                Told::Typed => Outcome::Typed,
                Told::Skipped(_) => Outcome::Skipped,
            };
            s.ledger_row(
                FactKind::Hold.rule(),
                outcome,
                text,
                &door::outcome_reason(rule, episode, held_ms, declared, told),
                due.seq,
            );
        };
        let early = armed(Fault::DoorOutcomeFirst);
        if early {
            outcome_row(self, Told::Typed);
        }
        let point = self.turn_of(screen.clone());
        let why = match self.type_fact(grant, &point, text, rule.name(), review)? {
            Some(Written::Submitted) => None,
            None => Some("unfenced"),
            Some(Written::Not) => Some("not-written"),
            Some(Written::Left) => Some("not-submitted"),
        };
        if let Some(why) = why {
            self.door_skipped(due, why, &outcome_row);
            return Ok(Wrote::Skipped(why));
        }
        if armed(Fault::DoorHostTyped) {
            self.turn_end.host_typed(self.fx.mono(), HostTurn::Notice);
        } else {
            self.turn_end.fact_typed();
        }
        self.hold
            .door
            .facts
            .told(key, self.hold_wall_ms(due.now_ms));
        self.hold.door.books.fired(rule, due.now_ms, true);
        self.hold_line(due.seq, Some(cfg), review);
        host.break_taken();
        host.inform(text);
        if !early {
            outcome_row(self, Told::Typed);
        }
        Ok(Wrote::Typed)
    }

    /// A WRITE THAT WROTE NOTHING after its intent row (`OccupancyDoor`'s `Skip`): its outcome
    /// row `skipped why=<why>`, the episode untold, and tried again only past the session's pace.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "Skip",
            project = "conformance_occupancy::project"
        )
    )]
    fn door_skipped(&mut self, due: &DueLine, why: &'static str, row: &dyn Fn(&mut Self, Told)) {
        self.hold.door.again = Some((
            due.episode,
            due.now_ms.saturating_add(rules::SESSION_PACE_MS),
        ));
        row(self, Told::Skipped(why));
    }

    /// The write's `OCCUPANCY` line, once a change: `typed`, `skipped why=<word>`,
    /// `held:<guard>`, `told`, `paced why=<word>` or `refused why=<word>`.
    fn door_said(&mut self, due: &DueLine, wrote: Wrote, review: &mut dyn Review) {
        let (said, words) = match wrote {
            Wrote::Held(guard) => (Said::Held(guard), format!("held:{}", guard.word())),
            Wrote::Told => (Said::Told(due.episode), "told".to_string()),
            Wrote::Paced(PaceGrant::Refused(why)) => (
                Said::HostPaced(PaceGrant::Refused(why)),
                format!("refused why={}", why.word()),
            ),
            Wrote::Paced(pace) => (Said::HostPaced(pace), format!("paced why={}", pace.word())),
            Wrote::Typed => (Said::Typed(due.episode), "typed".to_string()),
            Wrote::Skipped(why) => (
                Said::Skipped(due.episode, why),
                format!("skipped why={why}"),
            ),
        };
        let last = &mut self.hold.door.said[due.rule.index()];
        if *last == Some(said) {
            return;
        }
        *last = Some(said);
        self.hold.door.said_at[due.rule.index()] = self.hold_wall_ms(due.now_ms);
        review.note(&format!(
            "OCCUPANCY seq={} rule={} rev=builtin episode={:016x} door=break {words}",
            due.seq,
            due.rule.name(),
            due.episode
        ));
        if let Wrote::Held(guard) = wrote {
            self.held_row(
                due.seq,
                (due.rule, due.ordinal, guard),
                due.episode,
                due.hold,
                due.now_ms,
            );
        }
    }

    /// The board's reading for this break: the one asked within the last [`ASK_EVERY_MS`], else
    /// the host asked once more (the empty reading from a loop with no host).
    /// Re-age it at this break's existing clock pair, so retaining a fresh
    /// producer verdict cannot extend its two-look lifetime. No clock is read here.
    fn occupancy_reading(&mut self, now: Instant, now_ms: i64) -> OccupancyReading {
        let reading = if let Some((asked_ms, reading)) = &self.hold.door.asked
            && (0..ASK_EVERY_MS).contains(&now_ms.saturating_sub(*asked_ms))
        {
            *reading
        } else {
            let reading = self
                .stall_host
                .as_ref()
                .map(|host| host.occupancy())
                .unwrap_or_default();
            self.hold.door.asked = Some((now_ms, reading));
            reading
        };
        reading.aged_at(self.wall_at(now))
    }

    /// The wall clock (unix ms) at `now`, a read this loop made, by the pair its last read
    /// marked (`clock_marks`); `None` before one did. No clock is read here.
    fn wall_at(&self, now: Instant) -> Option<i64> {
        self.clock_marks.map(|(marked_at, wall_ms)| {
            let elapsed = now.checked_duration_since(marked_at).unwrap_or_default();
            let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
            wall_ms.saturating_add(elapsed_ms)
        })
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
        // An act untaken is any line the harness typed that the agent has not taken: the
        // policy's, the door's own, and the host's (the upgrade's notice, a carry-on), which
        // `act_untaken` leaves to the host's own step and which stands until the agent's turn
        // is read, past the break it went at (the safety review's F-A).
        let untaken = self.turn_end.act_untaken()
            || (self.turn_end.typed_untaken() && !armed(Fault::OverUntakenHostLine));
        let brake = [
            (self.stalled.is_some(), Guard::Stall),
            (self.limit.is_some(), Guard::LimitEpisode),
            (untaken, Guard::ActUntaken),
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
            managed: self.turn_end.managed(),
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
        let ordinal = self.hold.door.books.this_hold(rule);
        let episode = rules::episode(rule, self.hold_wall_ms(hold.since_ms), ordinal);
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
        self.hold.door.said_at[rule.index()] = self.hold_wall_ms(now_ms);
        if said.is_none() {
            return;
        }
        review.note(&format!(
            "OCCUPANCY seq={seq} rule={} rev=builtin episode={episode:016x} door=break {words}",
            rule.name()
        ));
        if let Verdict::Held(guard) = verdict {
            self.held_row(seq, (rule, ordinal, guard), episode, hold, now_ms);
        }
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

    /// A HELD VERDICT'S `fact-held@v1` ROW (design §6; `waiting`, so nothing follows it), at the
    /// change of a rule's word to `held:<guard>` that the caller said: booked the first time the
    /// guard holds the episode alone (`key`: its rule, its ordinal in the open hold and the
    /// guard; `episode` its id), at the decider or at the write, so a guard that flaps with
    /// another or with the machine's terms books it once, and a hold's rows are bounded by its
    /// episodes and the guards that held each. A successor that carries the hold books none its
    /// predecessor booked ([`DoorWatch::held`], seeded from the ledger).
    fn held_row(&mut self, seq: u64, key: HeldKey, episode: u64, hold: HoldSeen, now_ms: i64) {
        if self.hold.door.held.contains(&key) {
            if !armed(Fault::HeldEachChange) {
                return;
            }
        } else {
            self.hold.door.held.push(key);
        }
        let (rule, ordinal, guard) = key;
        let held_ms = now_ms.saturating_sub(hold.since_ms);
        self.ledger_row(
            rules::HELD_RULE,
            Outcome::Waiting,
            "",
            &rules::held_reason(rule, episode, ordinal, held_ms, hold.declared, guard),
            seq,
        );
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

    /// THE LOOP ENDS (`Session::drive`, any end): where its host was last told an open hold, it
    /// is told none, once — no loop measures that hold now, so the window's board counts it
    /// neither in `occupancy_hold=` nor in `ready=`. A policy whose occupancy moved hands the tab
    /// to a successor loop (the host's `cfg_gen`), which under `off` tells nothing; under
    /// `observe` or `prompt` its first break tells the hold again. The word is the last one told
    /// with its hold, its ready proxy, its guards and its rules' words emptied, stamped as that
    /// one was: nothing is read for it and nothing typed. A tape from a build with no such door
    /// told nothing, so it tells nothing here either. A loop that unwinds is told so by
    /// [`UntoldOnUnwind`].
    pub(super) fn occupancy_untold(&mut self) {
        let Some(said) = self.untold_word() else {
            return;
        };
        let Some(host) = self.stall_host.clone() else {
            return;
        };
        host.occupancy_said(&said);
        self.hold.door.told = Some(said);
    }

    /// The word the loop's end tells its host ([`Self::occupancy_untold`]): the last one told,
    /// its hold, ready proxy, guards and rules' words emptied; `None` where no open hold was told.
    fn untold_word(&self) -> Option<OccupancySaid> {
        let told = self.hold.door.told.as_ref().filter(|t| t.hold.is_some())?;
        Some(OccupancySaid {
            hold: None,
            ready: false,
            guards: None,
            rules: RuleId::ALL.map(|rule| SaidRule {
                rule,
                verdict: SaidVerdict::Quiet,
                at_ms: 0,
            }),
            ..told.clone()
        })
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
        if let HoldChange::Closed { .. } = change {
            // The hold's episodes closed with it: none is open, and a seed holds its kind no more.
            self.hold.door.facts.observe_closed(&[]);
        }
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
        // A close is told where the host was told the hold: its status no longer shows it.
        if let HoldChange::Closed { held_ms, .. } = change
            && let Some(told) = &self.hold.door.told
        {
            let at_ms = told
                .hold
                .map_or(told.at_ms, |hold| hold.since_ms.saturating_add(held_ms));
            self.occupancy_tell(told.mode, at_ms);
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

/// [`Session::drive`]'S EVERY END TOLD, AN UNWIND'S TOO: a loop that panics with an open hold told
/// its host would leave that hold on the window's board, where `ready=` counts it, until the
/// tab's next loop told it again. The guard holds the session for `drive`'s body; dropped on an
/// unwind (and on that alone: a return tells it itself, [`Session::occupancy_untold`]), it
/// tells `live` — the host the loop's caller handed it, never the tape's door: an unwind crosses
/// no door, and a replay, whose host is the tape's, has none — the untold word, once. Nothing
/// is read for it and nothing typed. A host that panics in turn is let be: a second panic out of
/// a destructor that runs on an unwind aborts the process, and every window with it.
pub(super) struct UntoldOnUnwind<'g, 's, C: Ctl> {
    session: &'g mut Session<'s, C>,
    live: Option<Arc<dyn IdleHost>>,
}

impl<'g, 's, C: Ctl> UntoldOnUnwind<'g, 's, C> {
    /// The guard over `session`, telling `live` should it unwind.
    pub(super) fn over(session: &'g mut Session<'s, C>, live: Option<Arc<dyn IdleHost>>) -> Self {
        Self { session, live }
    }

    /// The session it guards.
    pub(super) fn session(&mut self) -> &mut Session<'s, C> {
        self.session
    }
}

impl<C: Ctl> Drop for UntoldOnUnwind<'_, '_, C> {
    fn drop(&mut self) {
        if !std::thread::panicking() || armed(Fault::UntoldOnReturnOnly) {
            return;
        }
        let (Some(live), Some(said)) = (self.live.as_ref(), self.session.untold_word()) else {
            return;
        };
        let told =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| live.occupancy_said(&said)));
        if told.is_ok() {
            self.session.hold.door.told = Some(said);
        }
    }
}
