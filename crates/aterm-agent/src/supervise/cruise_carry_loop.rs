// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervision-loop seam of Cruise's carry at a break of the agent's own background work
//! ([`super::super::policy::cruise_carry`]), and of the needs such a break's report names.

use std::time::{Duration, Instant};

use super::super::config::{Autopilot, SupervisorConfig};
use super::super::policy::cruise_carry::{BackgroundCarries, RULE_BACKGROUND, carry_words};
use super::approvals::Outcome;
use super::turn_end_loop::Written;
use super::*;

/// Cruise's carries at breaks: their pacing books and what the journal last said of them.
#[derive(Debug, Default)]
pub(super) struct CarryWatch {
    /// When the last carry went and the streak it set ([`BackgroundCarries`]).
    books: BackgroundCarries,
    /// A live turn has been read since the last break or point: the next live read is no edge.
    live: bool,
    /// What the journal last said of a carry not typed: once a change.
    said: Option<CarrySaid>,
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
    /// A read of a live turn (`Session::took`): someone else's (`ours` false: a person's, the
    /// program's own wake) starts the books again, once a turn.
    pub(super) fn live(&mut self, ours: bool) {
        if std::mem::replace(&mut self.live, true) {
            return;
        }
        if !ours {
            self.books.reset();
        }
    }

    /// A read of a break of the agent's own background work: the next live read is an edge.
    pub(super) const fn at_break(&mut self) {
        self.live = false;
    }

    /// A judged point: the policy carries the session from here, and the next break is carried
    /// at its settle again.
    pub(super) fn point(&mut self) {
        self.live = false;
        self.books.reset();
        self.said = None;
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
        self.cruise_carry
            .books
            .typed(&self.turn_end.timing, at, typed_at);
        self.cruise_carry.said = None;
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
