// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hold clock and occupancy door in the supervision loop ([`crate::supervise::occupancy::hold`],
//! [`crate::supervise::occupancy::rules`]).

use std::time::{Duration, Instant};

use super::super::config::{Occupancy, SupervisorConfig};
use super::super::facts::{EpisodeKey, FactKind};
use super::super::memory;
use super::super::occupancy::door::{self, AtWrite, PaceGrant, StatusSeen, Told};
use super::super::occupancy::fault::{Fault, armed};
use super::super::occupancy::hold::{self, Close, HoldChange, HoldClock, HoldEvent, RowSaid};
use super::super::occupancy::rules::{self, Books, Guards, HoldSeen, RuleId, Verdict};
use super::super::occupancy::said;
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
/// no loop watched.
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
    said: [Option<Said>; RuleId::COUNT],
    /// The clock's milliseconds and the wall's (unix ms) at the loop's start
    /// ([`Session::hold_seed_from`]): a hold's start in unix ms, for its episodes' ids.
    wall: Option<(i64, i64)>,
    /// Under `prompt`, this break's due line, for the write at the same break
    /// ([`Session::occupancy_write`]).
    due: Option<DueLine>,
    /// The episode whose write last made its reads and wrote nothing, with when it may be tried
    /// again (the clock's).
    again: Option<(u64, i64)>,
    /// When each rule's word last changed ([`Self::said`]), unix ms.
    said_at: [i64; RuleId::COUNT],
    /// What the last break judged beside the verdicts: the ready proxy, the guards, and the
    /// reading the decider judged ([`OccupancySaid`]).
    seen: Option<(bool, Guards, OccupancyReading)>,
    /// What the host was last told ([`Session::occupancy_tell`]): once a change.
    told: Option<OccupancySaid>,
    /// The open hold's held verdicts booked as `fact-held@v1` rows ([`Session::held_row`]):
    /// each episode by its rule and its ordinal in the hold, with the guard that held it.
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
    /// `occupancy` is not `off`.
    fn occupancy_observes(&self, cfg: Option<&SupervisorConfig>, review: &dyn Review) -> bool {
        self.stall_host.is_some()
            && review.unattended()
            && cfg.is_some_and(|cfg| cfg.occupancy.observes())
    }

    /// `at` on the clock's milliseconds.
    pub(super) fn hold_ms(&mut self, at: Instant) -> i64 {
        let origin = *self.hold.origin.get_or_insert(at);
        signed_ms(origin, at)
    }

    /// The hold this tab's ledger left open ([`hold::seed_row_in`] of its own text `body`, read
    /// once at the loop's start), to carry or to close at the first thing fed.
    pub(super) fn hold_seed_from(
        &mut self,
        body: &str,
        start: Instant,
        now_ms: i64,
        lifted_off: bool,
        cfg: &SupervisorConfig,
        review: &dyn Review,
    ) {
        // The facts told, of every kind: the stuck fact and the turn-end rider tell under any
        // `occupancy`, and a successor must not tell them twice.
        let facts = door::seed_rows_in(body, self.setup.sid.as_deref());
        self.facts.state.seed(&facts, now_ms);
        // And the stuck lines' place in the session's pace, under any key: a line typed before
        // a restart still spaces the next one, the stuck fact's or the door's.
        let start_ms = self.hold_ms(start);
        self.stuck_lines_seed(body, start_ms, now_ms);
        if !self.occupancy_observes(Some(cfg), review) {
            return;
        }
        self.hold.door.wall = Some((start_ms, now_ms));
        let would = rules::would_rows_in(body, self.setup.sid.as_deref());
        let held = rules::held_rows_in(body, self.setup.sid.as_deref());
        let booked = (&would[..], &held[..]);
        let Some(row) = hold::seed_row_in(body, self.setup.sid.as_deref()) else {
            self.door_seed(booked, start_ms, now_ms, None);
            return;
        };
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

    /// Seed the door's books from the would-be lines and the held verdicts a loop before this one
    /// booked (`would` and `held`, oldest first), placed on the clock by the loop's start.
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

    /// A read of the agent's own background work at a break begun at `at`, standing `waited`,
    /// `settled` once past its settle.
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
        let drawn = self.transcript_wait_seq != Some(seq);
        if settled
            && drawn
            && let Some(cfg) = cfg
        {
            self.occupancy_break(screen, at + waited, now_ms, cfg, review);
        }
    }

    /// The occupancy door at a settled, drawn break: the built-in rules judged on the open hold
    /// at `now_ms`, each change said, and each would-be line booked.
    fn occupancy_break(
        &mut self,
        screen: &Screen,
        now: Instant,
        now_ms: i64,
        cfg: &SupervisorConfig,
        review: &mut dyn Review,
    ) {
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
        if self.hold.door.books.hold_since_ms() != Some(since_ms) {
            self.hold.door.said = [None; RuleId::COUNT];
            self.hold.door.said_at = [0; RuleId::COUNT];
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
        let mode = if cfg.occupancy == Occupancy::Prompt && !prompt {
            Occupancy::Observe
        } else {
            cfg.occupancy
        };
        self.occupancy_tell(mode, self.hold_wall_ms(now_ms));
    }

    /// What the door says now, told to the host once a change (`IdleHost::occupancy_said`).
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
        let rules = said::told_rules(self.fx.slice2_recorded())
            .into_iter()
            .map(|rule| {
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
            })
            .collect();
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

    /// Under `prompt`, a due line kept for the write at this same break
    /// ([`Self::occupancy_write`]).
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

    /// The door's write under `prompt`, at the break whose decider left a line due.
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
        } else if self.facts.state.is_told(key) {
            Wrote::Told
        } else {
            let wrote = if let Some(guard) = self.door_holds(screen, &due, cfg, host.as_ref())? {
                Wrote::Held(guard)
            } else {
                let mut pace = host.occupancy_pace(due.rule, self.hold_wall_ms(due.hold.since_ms));
                if due.rule.slot() {
                    let at_ms = self.wall_at(due.now);
                    (pace, due.line) = pace.slot_answer(&due.line, due.hold, due.now_ms, at_ms);
                }
                if pace.lets() {
                    self.door_types(&grant, screen, &due, key, cfg, host.as_ref(), review)?
                } else {
                    Wrote::Paced(pace)
                }
            };
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
    /// ([`door::write_held`]).
    fn door_holds(
        &mut self,
        screen: &Screen,
        due: &DueLine,
        cfg: &SupervisorConfig,
        host: &dyn IdleHost,
    ) -> Result<Option<Guard>, Fail> {
        let guards = match self.write_guards(screen, due.now, cfg, host)? {
            Ok(guards) => guards,
            Err(guard) => return Ok(Some(guard)),
        };
        if let Some(guard) = guards.held(due.rule) {
            return Ok(Some(guard));
        }
        Ok(door::write_held(due.rule, Self::write_at(host)))
    }

    /// THE WRITE'S GUARDS, judged again on the one `status` read just before it (whose hands
    /// are on the session, the halt, unread input), the composer's read (a composer the read
    /// cannot judge is a draft) and the host's `taskless`; `Err` with the guard that holds where
    /// the read itself says nothing (`Guard::Person`). Every line at a break goes through it: the
    /// door's ([`Self::door_holds`]) and the stuck fact's (`Session::stuck_break`, through `door::stuck_at_break`).
    pub(super) fn write_guards(
        &mut self,
        screen: &Screen,
        now: Instant,
        cfg: &SupervisorConfig,
        host: &dyn IdleHost,
    ) -> Result<Result<Guards, Guard>, Fail> {
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
                return Ok(Err(Guard::Person));
            };
            seen
        };
        let mut guards = self.occupancy_guards(now, cfg, &screen.rows);
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
        if armed(Fault::DoorIgnoresBrakes) {
            guards.brake = None;
            guards.memory = false;
            guards.reach_down = false;
        }
        Ok(Ok(guards))
    }

    /// What the host says at the write, its guards passed: its withhold or wind-down, and
    /// whether the upgrade's notice is due (asked only where nothing withholds).
    pub(super) fn write_at(host: &dyn IdleHost) -> AtWrite {
        let brakes = !armed(Fault::DoorIgnoresBrakes);
        let withheld = brakes && (host.withholds_turn_end() || host.owns_turn_end());
        AtWrite {
            host_line: false,
            withheld,
            upgrade_due: !withheld && host.upgrade_due(),
        }
    }

    /// Whether the composer the break's screen draws holds a draft: `Some(true)` a draft,
    /// `None` unknown — which the door reads as a draft.
    pub(super) fn door_draft(&mut self, screen: &Screen) -> Option<bool> {
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
        let grant = super::super::config::BreakGrant::Occupancy(grant);
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
        let told_ms = self.hold_wall_ms(due.now_ms);
        self.facts.state.told(key, told_ms);
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
    /// marked (`clock_marks`); `None` before one did.
    fn wall_at(&self, now: Instant) -> Option<i64> {
        self.clock_marks.map(|(marked_at, wall_ms)| {
            let elapsed = now.checked_duration_since(marked_at).unwrap_or_default();
            let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
            wall_ms.saturating_add(elapsed_ms)
        })
    }

    /// The guards, from what the loop holds: nothing is read for them.
    pub(super) fn occupancy_guards(
        &self,
        now: Instant,
        cfg: &SupervisorConfig,
        rows: &[String],
    ) -> Guards {
        let reader = self.reader(rows);
        let credits =
            !cfg.usage_credits && (self.credits_kept.is_some() || reader.credits(rows).is_some());
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

    /// A held verdict's `fact-held@v1` row at the change of a rule's word to `held:<guard>` that
    /// the caller said: booked the first time the guard holds the episode alone.
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

    /// The door's books, which the stuck fact's line obeys the session's pace by.
    pub(super) const fn hold_books(&self) -> &Books {
        &self.hold.door.books
    }

    /// A line of the stuck fact's went at `at_ms` (the clock's): it counts toward the session's
    /// pace as the door's own lines do.
    pub(super) fn door_lined(&mut self, at_ms: i64) {
        self.hold.door.books.lined(at_ms);
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
    /// is still untaken.
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

    /// A decided point on `screen`: `acts` where its action types, escalates or restarts.
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

    /// The loop ends with the hold open (`Session::drive`, any end but the session's own): a
    /// booked hold's open row is written again, held to the last read that saw it, so a
    /// successor that begins within [`CARRY_GAP_MS`] carries it ([`Self::hold_seed_from`]).
    pub(super) fn hold_left(&mut self, cfg: Option<&SupervisorConfig>, review: &dyn Review) {
        if !self.hold.booked || !self.occupancy_observes(cfg, review) {
            return;
        }
        if let Some(held_ms) = self.hold.clock.seen_held_ms() {
            let declared = self.hold.clock.declared();
            self.hold_row(0, RowSaid::Open { held_ms, declared });
        }
    }

    /// The loop ends (`Session::drive`, any end): where its host was last told an open hold, it
    /// is told none, once.
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
            rules: told
                .rules
                .iter()
                .map(|r| SaidRule {
                    rule: r.rule,
                    verdict: SaidVerdict::Quiet,
                    at_ms: 0,
                })
                .collect(),
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
            self.facts.state.observe_closed(FactKind::Hold, &[]);
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

/// Guard that tells the host the untold word if [`Session::drive`] unwinds with an open hold.
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
