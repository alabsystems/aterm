// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervision-loop seam of Cruise's carry at a break of the agent's own background work
//! ([`super::super::policy::cruise_carry`]), and of the needs such a break's report names.

use std::time::{Duration, Instant};

use super::super::config::{Autopilot, SupervisorConfig};
use super::super::policy::cruise_carry::{BackgroundCarries, RULE_BACKGROUND, carry_words};
use super::super::policy::turn_end::TurnEndTiming;
use super::approvals::Outcome;
use super::turn_end_loop::Written;
use super::*;
use crate::harness::upgrade::HARNESS_MARK;

/// Cruise's carries at breaks: their pacing books and what the journal last said of them.
#[derive(Debug, Default)]
pub(super) struct CarryWatch {
    /// When the last carry went and the streak it set ([`BackgroundCarries`]).
    books: BackgroundCarries,
    /// The turn read live since the last settled break or point, which the next settled break
    /// judges ([`Self::at_break`]).
    opened: Option<Opened>,
    /// When the last carry was typed, until a settled break judges a turn: the turn it opens is
    /// its answer, its work counted from the typing.
    carried: Option<Instant>,
    /// The person's count (`human_seq`, the `if-human=` fence) the last carry was typed under.
    count: Option<u64>,
    /// The session's submissions (`submits`) where the last turn was judged or the last carry
    /// typed, whichever came later: a submission past them but the harness's own line's is
    /// someone else's message.
    submits: Option<u64>,
    /// What the journal last said of a carry not typed: once a change.
    said: Option<CarrySaid>,
    /// When the last carry of a loop before this one was typed ([`Self::seed`]), until this loop's
    /// first busy read: the counts that carry was typed under were that loop's.
    owed: Option<Instant>,
}

/// A turn read live since the last settled break or point.
#[derive(Debug, Clone, Copy)]
struct Opened {
    /// The harness's own line opened it (`TurnEndState::typed_untaken`).
    ours: bool,
    /// A read of it drew someone else's prompt row newest ([`newest_prompt_ours`]): a message no
    /// key in this window typed (Claude Code's Remote Control, another driver) or a person's.
    theirs: bool,
    /// Its live stretches' busy time so far, each from its live edge (the carry's typing, for the
    /// carry's answer) to the start of the break it reached.
    busy: Duration,
    /// When the stretch running now began: `None` while the turn stands at a break short of its
    /// settle, which is no work.
    since: Option<Instant>,
    /// A break read as begun before the stretch then running (the loop stepped no break while it
    /// ran: under Off): where that stretch ended went unread, so its work is unknown.
    unread: bool,
}

/// Why a due carry was not typed, as the journal says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarrySaid {
    /// This guard holds it.
    Held(Guard),
    /// The streak's back-off runs to this time.
    Waits(Instant),
    /// The hour's typing budget (`continue_per_hour`) is spent.
    Budget,
}

impl CarryWatch {
    /// A read of a live turn (`Session::took`): the first since the last settled break or point
    /// opens a turn (`ours` when the harness's own line opened it), the first after a pause
    /// inside it starts its next stretch, each at `at` (read only then), and the settled break
    /// that ends it judges it ([`Self::at_break`]).
    pub(super) fn live(&mut self, ours: bool, at: impl FnOnce() -> Instant) {
        match self.opened.as_mut() {
            None => {
                self.opened = Some(Opened {
                    ours,
                    theirs: false,
                    busy: Duration::ZERO,
                    since: Some(self.carried.unwrap_or_else(at)),
                    unread: false,
                });
            }
            Some(opened) if opened.since.is_none() => opened.since = Some(at()),
            Some(_) => {}
        }
    }

    /// A LOOP BEFORE THIS ONE'S LADDER: its last carry typed `ago` before `now`, the `streak`-th
    /// in a row. The counts it was typed under were that loop's, so this loop's first busy read
    /// stands for them ([`Self::read`]): the next turn is judged by its busy time unless someone
    /// else's message lands in it after that read, never as a person's for counts this loop never
    /// saw.
    pub(super) fn seed(&mut self, ago: Duration, streak: u32, now: Instant) {
        let Some(last) = now.checked_sub(ago) else {
            return;
        };
        self.books = BackgroundCarries::carried(last, streak);
        self.owed = Some(last);
    }

    /// A busy read on `screen`, at `at` (read only for the first after a seed): an open turn whose
    /// newest prompt row is not the harness's has someone else's message in it. The first after a
    /// seed stands for the counts the last carry was typed under, but not where the person's last
    /// gesture the host reports came after that carry: the seeded books start again even if
    /// their turn already reached its break before the restart, so no live edge was read. The
    /// counts stay unknown and a live turn not the harness's is a person's ([`Self::at_break`]).
    pub(super) fn read(
        &mut self,
        reader: &dyn ScreenReader,
        screen: &Screen,
        at: impl FnOnce() -> Instant,
    ) {
        if let Some(last) = self.owed.take() {
            let touched = match screen.human {
                HumanInput::Ago(ms) => {
                    Duration::from_millis(ms) < at().saturating_duration_since(last)
                }
                HumanInput::Unknown | HumanInput::Never | HumanInput::NeverFor(_) => false,
            };
            if touched {
                self.books.reset();
            } else {
                self.count = screen.human_seq;
                self.submits = screen.submits;
            }
        }
        if let Some(opened) = self.opened.as_mut()
            && !opened.theirs
        {
            opened.theirs = newest_prompt_ours(reader, &screen.rows) == Some(false);
        }
    }

    /// A TURN ENDS ONLY AT A SETTLED BREAK: a read on `screen` of a break of the agent's own
    /// background work that began at `began` ends the open turn's stretch there. Unsettled, it is
    /// a pause inside the turn (a reply's text drawn between tool calls under a status line), and
    /// the pause is no work. Settled ([`background_settle`]), it ends the turn, which is judged:
    /// - SOMEONE ELSE'S MESSAGE starts the books again, whoever opened the turn: a submission
    ///   since the last judged turn or carry that is not the harness's own line (a person's
    ///   Enter, another driver's `send`), or a prompt row not the harness's ([`Opened::theirs`]:
    ///   Remote Control's). A scroll, a click or a key that submits nothing is no message. Where
    ///   a submission count is unknown, the person's count (any gesture) moved since the last
    ///   carry, or unknown on a turn not the harness's, is a person's turn.
    /// - A turn whose stretch's end went unread starts them again: unsure.
    /// - THE PROGRAM'S OWN WAKE keeps them, as the harness's own line does: REAL WORK IS BUSY
    ///   TIME, the turn's stretches summed, and a turn of real work starts them again while a
    ///   short one leaves them ([`BackgroundCarries::ended`]).
    pub(super) fn at_break(
        &mut self,
        timing: &TurnEndTiming,
        screen: &Screen,
        began: Instant,
        settled: bool,
    ) {
        let Some(mut turn) = self.opened.take() else {
            return;
        };
        if let Some(since) = turn.since.take() {
            turn.unread |= began < since;
            turn.busy = turn
                .busy
                .saturating_add(began.saturating_duration_since(since));
        }
        if !settled {
            self.opened = Some(turn);
            return;
        }
        let carried = self.carried.take().is_some();
        let own = u64::from(turn.ours || carried);
        let someone = turn.theirs
            || match (screen.submits, self.submits, screen.human_seq, self.count) {
                (Some(now), Some(then), ..) => now > then.saturating_add(own),
                (.., Some(now), Some(then)) => now != then,
                _ => !turn.ours,
            };
        self.submits = screen.submits;
        if someone || turn.unread {
            self.books.reset();
            return;
        }
        self.books.ended(timing, turn.busy);
    }

    /// A judged point: the policy carries the session from here, and the next break is carried
    /// at its settle again.
    pub(super) fn point(&mut self) {
        self.opened = None;
        self.carried = None;
        self.books.reset();
        self.said = None;
    }

    /// The carry was typed at `now` at a break read on `screen`.
    fn typed(&mut self, now: Instant, screen: &Screen) {
        self.books.typed(now);
        self.carried = Some(now);
        self.count = screen.human_seq;
        self.submits = screen.submits;
        self.said = None;
    }
}

/// Whether the newest prompt row of Claude Code's transcript on `rows` (`❯ ` at its left edge,
/// above the live zone) is a line of the harness's: `None` for another program, or with none in
/// view. A wake draws no prompt row of its own (`⏺ Background command "…" completed`), so the
/// newest one stays the carry's.
fn newest_prompt_ours(reader: &dyn ScreenReader, rows: &[String]) -> Option<bool> {
    match reader.program() {
        Program::Claude => {
            let end = reader.transcript_end(rows).min(rows.len());
            rows[..end].iter().rev().find_map(|row| {
                let said = row.strip_prefix('❯')?;
                said.starts_with(char::is_whitespace)
                    .then(|| said.trim_start().starts_with(HARNESS_MARK))
            })
        }
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => None,
    }
}

impl<C: Ctl> Session<'_, C> {
    /// CRUISE NEVER PARKS AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK (owner ruling
    /// 2026-10-26): where nothing else went at the break that began at `at` (`line_gone`: a
    /// wake's, the host's, the stuck fact's or the door's line), it has settled
    /// ([`background_settle`]) and the carry is due ([`BackgroundCarries::due`]), types
    /// [`BACKGROUND_CARRY`] under Cruise, after every guard a line at a break passes but the
    /// person's own exchange and a manager's wait, which hold no Cruise session: a brake (a stall,
    /// a limit episode, an act or line untaken, the wake's settle, a credits hold), a person
    /// typing or another driver's hand, a halt, a draft or unread input, a session nobody asked
    /// anything, the memory wall, the API's route down, and the host's withhold. Off and Land type
    /// nothing here, by the mode the loop last read and again by the one read just before the
    /// write ([`Self::autopilot_at_busy`]). Returns whether it typed.
    pub(super) fn cruise_carry(
        &mut self,
        screen: &Screen,
        (at, waited): (Instant, Duration),
        line_gone: bool,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(cfg) = cfg else {
            return Ok(false);
        };
        if cfg.autopilot_for(self.autopilot.word()) != Autopilot::Cruise
            || !cfg.continue_policy
            || line_gone
            || waited < self.setup.background_settle
            || !review.unattended()
        {
            return Ok(false);
        }
        let Some(host) = self.stall_host.clone() else {
            return Ok(false);
        };
        let seq = screen.seq;
        let now = at + waited;
        let due = self.cruise_carry.books.due(&self.turn_end.timing, at);
        if now < due {
            self.carry_said(seq, CarrySaid::Waits(due), review);
            return Ok(false);
        }
        let cap = usize::try_from(cfg.continue_per_hour).unwrap_or(usize::MAX);
        if cap != 0 && self.turn_end.acts_in_window(now) >= cap {
            self.carry_said(seq, CarrySaid::Budget, review);
            return Ok(false);
        }
        let guards = match self.write_guards(screen, now, cfg, host.as_ref())? {
            Ok(guards) => guards,
            Err(guard) => {
                self.carry_said(seq, CarrySaid::Held(guard), review);
                return Ok(false);
            }
        };
        let held = guards
            .brake
            .or_else(|| {
                [
                    (Guard::Person, guards.person),
                    (Guard::Halted, guards.halted),
                    (Guard::Draft, guards.draft),
                    (Guard::Taskless, guards.taskless),
                    (Guard::Memory, guards.memory),
                    (Guard::Reach, guards.reach_down),
                ]
                .into_iter()
                .find_map(|(guard, holds)| holds.then_some(guard))
            })
            .or_else(|| {
                Self::write_at(host.as_ref())
                    .withheld
                    .then_some(Guard::HostWithheld)
            });
        if let Some(guard) = held {
            self.carry_said(seq, CarrySaid::Held(guard), review);
            return Ok(false);
        }
        // The mode the session keeps, read afresh for the write: a switch from the chip since
        // the loop last read it (to Land or Off) types nothing.
        if self.autopilot_at_busy(cfg)? != Some(Autopilot::Cruise) {
            return Ok(false);
        }
        let rules = cfg
            .rules_file
            .as_ref()
            .and_then(|path| self.fx.read_text(path))
            .and_then(|text| super::turn_end_loop::standing_rules_in(&text));
        let text = carry_words(rules.as_deref());
        let point = self.turn_of(screen.clone());
        let written = self.type_text_written(&point, &text, RULE_BACKGROUND, review)?;
        if written != Written::Submitted {
            return Ok(false);
        }
        let typed_at = self.fx.mono();
        self.cruise_carry.typed(typed_at, screen);
        self.turn_end.background_carried(typed_at);
        let typed_ms = self.hold_ms(typed_at);
        self.break_lined(typed_ms);
        self.hold_line(seq, Some(cfg), review);
        host.break_taken();
        host.inform(&text);
        review.say(&format!(
            "CONTINUED seq={seq} rule={RULE_BACKGROUND} {}",
            clip(&text)
        ))?;
        self.ledger_row(
            RULE_BACKGROUND,
            Outcome::Typed,
            &text,
            "the turn ended while the agent's own background work runs (autopilot cruise)",
            seq,
        );
        Ok(true)
    }

    /// THE NEEDS A BREAK'S REPORT NAMES: a turn that ends with the agent's own background work
    /// running reaches no point, where needs are read, so its `NEEDS YOU:` lines are read here
    /// at the settled break and put on the menu bar and the border as a point puts them
    /// ([`Self::sync_needs_under`]), whatever else the break types, and the work goes on.
    pub(super) fn break_needs(
        &mut self,
        screen: &Screen,
        waited: Duration,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(cfg) = cfg else {
            return Ok(());
        };
        if waited < self.setup.background_settle
            || cfg.autopilot_for(self.autopilot.word()) == Autopilot::Off
        {
            return Ok(());
        }
        let tail = self.reader(&screen.rows).said_tail(&screen.rows);
        let before = self.turn_end.needs_key();
        self.turn_end.fold_break_needs(tail.as_deref());
        if self.turn_end.needs_key() == before {
            return Ok(());
        }
        self.sync_needs_under(screen, cfg, review)
    }

    /// Journal why a due carry was not typed, once a change.
    fn carry_said(&mut self, seq: u64, said: CarrySaid, review: &mut dyn Review) {
        if self.cruise_carry.said == Some(said) {
            return;
        }
        self.cruise_carry.said = Some(said);
        let why = match said {
            CarrySaid::Held(guard) => format!("held:{}", guard.word()),
            CarrySaid::Waits(until) => format!(
                "waits until {} ({} short answer(s) in a row)",
                self.stamp_in(until),
                self.cruise_carry.books.streak()
            ),
            CarrySaid::Budget => "held:continue_per_hour".to_string(),
        };
        review.note(&format!("DEFERRED seq={seq} {RULE_BACKGROUND} {why}"));
    }
}
