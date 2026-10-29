// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervision-loop facts seam: turn-end riders and stuck-episode break lines.

use std::time::{Duration, Instant};

use super::super::config::{BreakGrant, StuckBreakGrant, SupervisorConfig};
use super::super::facts::{EpisodeKey, Fact, FactKind, FactsState};
use super::super::occupancy::door::{self, AtWrite, FactDoor, LeftRow, StuckVerdict, Told};
use super::super::occupancy::fault::{Fault, armed};
use super::super::occupancy::rules::Books;
use super::super::occupancy::templates;
use super::super::policy::turn_end::{RideFact, TurnEndAction, carries_fact, fact_may_ride};
use super::approvals::Outcome;
use super::occupancy_loop::ASK_EVERY_MS;
use super::turn_end_loop::Written;
use super::*;

/// Session facts state, pending turn-end rider, and stuck-line pacing.
#[derive(Debug, Default)]
pub(super) struct FactsWatch {
    /// Which episodes the session was told.
    pub(super) state: FactsState,
    /// The fact the act decided last carries ([`Session::fact_for`]), told once it is typed.
    offered: Option<Fact>,
    /// The declared wake's fact the act decided last carries (`Session::wake_ride`), claimed
    /// just before the act is typed and told once it is typed with it.
    pub(super) offered_wake: Option<RideFact>,
    /// Under `observe`, the declared wake (its ordinal and episode) the act decided last would
    /// have carried under `prompt` (`Session::wake_ride`): booked once the act is typed.
    pub(super) offered_would: Option<(u16, u64)>,
    /// The stuck episode whose write made its reads and wrote nothing, with when (the hold
    /// clock's milliseconds) its reads may be made again.
    again: Option<(EpisodeKey, i64)>,
    /// What the stuck line's last `FACT` line said, of which episode: once a change.
    said: Option<(EpisodeKey, StuckSaid)>,
    /// When (the hold clock's milliseconds) a line of aterm's own last went at a break, this
    /// loop's or a predecessor's ([`Session::stuck_lines_seed`]): the stuck fact's, or the
    /// door's under `prompt`, and never a line `observe` only would have typed. The pace the
    /// stuck line keeps ([`Session::stuck_books`], [`Session::break_lined`]).
    pub(super) lined_ms: Option<i64>,
}

/// What the stuck line came to at a break, as its `FACT` line says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StuckSaid {
    Held(Guard),
    Paced,
    Typed,
    Skipped(&'static str),
}

impl<C: Ctl> Session<'_, C> {
    /// Returns the pending untold fact that `action` may carry, if any.
    pub(super) fn fact_for(
        &mut self,
        action: &TurnEndAction,
        opts: &SuperviseOpts,
    ) -> Option<Fact> {
        if !opts.policy.facts || !fact_may_ride(&self.turn_end, action) {
            return None;
        }
        let host = opts.idle_host.clone()?;
        if self.fx.replay_lacks_host_door("host.facts") {
            return None;
        }
        let reading = host.facts();
        let now_ms = self.now_unix_ms();
        self.facts.state.observe(reading.as_ref(), now_ms);
        self.facts.state.pending(reading.as_ref(), now_ms)
    }

    /// The decision is `action`: the fact it carries (`fact`, or a declared wake's `wake`, where
    /// it rode) waits for the act.
    pub(super) fn fact_offer(
        &mut self,
        fact: Option<Fact>,
        wake: Option<RideFact>,
        action: &TurnEndAction,
    ) {
        self.facts.offered = fact.filter(|f| carries_fact(action, f));
        self.facts.offered_wake = wake.filter(|w| carries_fact(action, w.fact()));
        if !fact_may_ride(&self.turn_end, action) {
            self.facts.offered_would = None;
        }
    }

    /// Books the intent row for the fact `action` carries before the write.
    pub(super) fn fact_intends(&mut self, action: &TurnEndAction, seq: u64) -> Option<Fact> {
        let fact = self
            .facts
            .offered
            .take()
            .filter(|f| carries_fact(action, f))?;
        self.ledger_row(
            door::INTENT_RULE,
            Outcome::Waiting,
            "",
            &door::fact_intent_reason(fact.key, FactDoor::TurnEnd),
            seq,
        );
        Some(fact)
    }

    /// Books the outcome row for the fact `action` carried and marks it told when typed.
    pub(super) fn fact_rode(
        &mut self,
        fact: &Fact,
        skipped: Option<&'static str>,
        seq: u64,
        review: &mut dyn Review,
    ) {
        let typed = skipped.is_none();
        let told = skipped.map_or(Told::Typed, Told::Skipped);
        if typed {
            let at_ms = self.now_unix_ms();
            self.facts.state.told(fact.key, at_ms);
            self.turn_end.fact_rode(fact);
            if let Some(host) = self.stall_host.clone() {
                host.inform(&templates::stuck_line(&fact.text));
            }
        }
        review.note(&format!(
            "FACT seq={seq} kind={} episode={:016x} door={} {}",
            fact.key.kind.word(),
            fact.key.id,
            FactDoor::TurnEnd.word(),
            skipped.map_or_else(|| "typed".to_string(), |why| format!("skipped why={why}"))
        ));
        self.ledger_row(
            fact.key.kind.rule(),
            if typed {
                Outcome::Typed
            } else {
                Outcome::Skipped
            },
            &fact.text,
            &door::fact_outcome_reason(fact.key, FactDoor::TurnEnd, told),
            seq,
        );
    }

    /// The act the fact rode was written and not submitted (`Written::Left`): its line stands
    /// left in the composer ([`Self::left_write`]), told or released at the next look; under the
    /// planted `ReleaseOnLeft`, skipped as before.
    pub(super) fn fact_left(&mut self, fact: &Fact, seq: u64, review: &mut dyn Review) {
        let row = LeftRow::Fact {
            door: FactDoor::TurnEnd,
        };
        if !self.left_write(fact.key, row, &fact.text, seq, review) {
            self.fact_rode(fact, Some("not-submitted"), seq, review);
        }
    }

    /// Types the session's untold no-progress fact at a settled background break when due.
    pub(super) fn stuck_break(
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
        let Some(grant) = cfg.stuck_break_grant() else {
            return Ok(false);
        };
        let Some(host) = self.stall_host.clone() else {
            return Ok(false);
        };
        if waited < self.setup.background_settle
            || !review.unattended()
            || self.transcript_wait_seq == Some(screen.seq)
            || self.fx.replay_lacks_host_door("host.facts")
        {
            return Ok(false);
        }
        let reading = host.facts();
        let now_ms = self.now_unix_ms();
        self.facts.state.observe(reading.as_ref(), now_ms);
        let Some(fact) = self.facts.state.pending(reading.as_ref(), now_ms) else {
            return Ok(false);
        };
        let now = at + waited;
        let clock_ms = self.hold_ms(now);
        let gone = AtWrite {
            host_line: line_gone,
            ..AtWrite::default()
        };
        let decided = self.stuck_verdict(&grant, now, cfg, gone, clock_ms, &screen.rows);
        if decided != StuckVerdict::Due {
            self.stuck_said(screen.seq, fact.key, decided, review);
            return Ok(false);
        }
        if self
            .facts
            .again
            .is_some_and(|(key, until)| key == fact.key && clock_ms < until)
        {
            return Ok(false);
        }
        let written = match self.write_guards(screen, now, cfg, host.as_ref())? {
            Err(guard) => StuckVerdict::Held(guard),
            Ok(guards) => match guards.held_for(door::BreakLine::Stuck) {
                Some(guard) => StuckVerdict::Held(guard),
                None => door::stuck_at_break(
                    Some(&grant),
                    &guards,
                    Self::write_at(host.as_ref()),
                    &self.stuck_books(),
                    clock_ms,
                ),
            },
        };
        if written != StuckVerdict::Due {
            self.facts.again = Some((fact.key, clock_ms.saturating_add(ASK_EVERY_MS)));
            self.stuck_said(screen.seq, fact.key, written, review);
            return Ok(false);
        }
        self.stuck_types(screen, &grant, &fact, clock_ms, cfg, host.as_ref(), review)
    }

    fn stuck_verdict(
        &self,
        grant: &StuckBreakGrant,
        now: Instant,
        cfg: &SupervisorConfig,
        gone: AtWrite,
        clock_ms: i64,
        rows: &[String],
    ) -> StuckVerdict {
        let guards = self.occupancy_guards(now, cfg, rows);
        door::stuck_at_break(Some(grant), &guards, gone, &self.stuck_books(), clock_ms)
    }

    fn stuck_books(&self) -> Books {
        let mut own = Books::default();
        if let Some(at_ms) = self.facts.lined_ms {
            own.lined(at_ms);
        }
        own
    }

    /// Records that a line of aterm's own went at a break at `at_ms`.
    pub(super) fn break_lined(&mut self, at_ms: i64) {
        self.facts.lined_ms = Some(self.facts.lined_ms.map_or(at_ms, |l| l.max(at_ms)));
    }

    /// Seeds break-line pacing from `body`, the ledger read at loop start.
    pub(super) fn stuck_lines_seed(&mut self, body: &str, start_ms: i64, now_ms: i64) {
        use super::super::occupancy::rules::SESSION_PACE_MS;
        for (row_ms, kind) in door::break_lines_in(body, self.setup.sid.as_deref()) {
            let ago = now_ms.saturating_sub(row_ms);
            if !(1 - SESSION_PACE_MS..SESSION_PACE_MS).contains(&ago) {
                continue;
            }
            let at_ms = start_ms.saturating_sub(ago.max(0));
            if kind == FactKind::NoProgress {
                self.door_lined(at_ms);
            }
            self.break_lined(at_ms);
        }
    }

    /// The write itself, every guard passed: the intent row, the fenced write, and after it the
    /// line's bookkeeping and its outcome row.
    #[allow(clippy::too_many_arguments)]
    fn stuck_types(
        &mut self,
        screen: &Screen,
        grant: &StuckBreakGrant,
        fact: &Fact,
        clock_ms: i64,
        cfg: &SupervisorConfig,
        host: &dyn IdleHost,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let seq = screen.seq;
        let rule = fact.key.kind.rule();
        self.ledger_row(
            door::INTENT_RULE,
            Outcome::Waiting,
            "",
            &door::fact_intent_reason(fact.key, FactDoor::Break),
            seq,
        );
        let line = templates::stuck_line(&fact.text);
        let point = self.turn_of(screen.clone());
        let written = self.type_fact(BreakGrant::StuckBreak(grant), &point, &line, rule, review)?;
        let why = match written {
            Some(Written::Submitted) => None,
            None => Some("unfenced"),
            Some(Written::Not) => Some("not-written"),
            Some(Written::Left) => Some("not-submitted"),
        };
        let row = LeftRow::Fact {
            door: FactDoor::Break,
        };
        if written == Some(Written::Left) && self.left_write(fact.key, row, &line, seq, review) {
            // Tried again, should a look release it, only past the session's pace; it is the
            // break's one line, as a typed one is.
            self.facts.again = Some((
                fact.key,
                clock_ms.saturating_add(super::super::occupancy::rules::SESSION_PACE_MS),
            ));
            self.facts.said = Some((fact.key, StuckSaid::Skipped("not-submitted")));
            return Ok(!armed(Fault::StuckLeftNotTheLine));
        }
        let told = why.map_or(Told::Typed, Told::Skipped);
        if let Some(why) = why {
            self.facts.again = Some((
                fact.key,
                clock_ms.saturating_add(super::super::occupancy::rules::SESSION_PACE_MS),
            ));
            self.facts.said = Some((fact.key, StuckSaid::Skipped(why)));
        } else {
            self.turn_end.fact_typed();
            let at_ms = self.now_unix_ms();
            self.facts.state.told(fact.key, at_ms);
            self.facts.again = None;
            self.door_lined(clock_ms);
            self.break_lined(clock_ms);
            self.hold_line(seq, Some(cfg), review);
            host.break_taken();
            host.inform(&line);
            self.facts.said = Some((fact.key, StuckSaid::Typed));
        }
        review.note(&format!(
            "FACT seq={seq} kind={} episode={:016x} door={} {}",
            fact.key.kind.word(),
            fact.key.id,
            FactDoor::Break.word(),
            why.map_or_else(|| "typed".to_string(), |why| format!("skipped why={why}"))
        ));
        self.ledger_row(
            rule,
            if why.is_none() {
                Outcome::Typed
            } else {
                Outcome::Skipped
            },
            &line,
            &door::fact_outcome_reason(fact.key, FactDoor::Break, told),
            seq,
        );
        Ok(why.is_none())
    }

    fn stuck_said(
        &mut self,
        seq: u64,
        key: EpisodeKey,
        verdict: StuckVerdict,
        review: &mut dyn Review,
    ) {
        let said = match verdict {
            StuckVerdict::Held(guard) => StuckSaid::Held(guard),
            StuckVerdict::Paced { .. } => StuckSaid::Paced,
            StuckVerdict::Ungranted | StuckVerdict::Due => return,
        };
        if self.facts.said == Some((key, said)) {
            return;
        }
        self.facts.said = Some((key, said));
        let words = match said {
            StuckSaid::Held(guard) => format!("held:{}", guard.word()),
            StuckSaid::Paced => "paced".to_string(),
            StuckSaid::Typed => "typed".to_string(),
            StuckSaid::Skipped(why) => format!("skipped why={why}"),
        };
        review.note(&format!(
            "FACT seq={seq} kind={} episode={:016x} door={} {words}",
            key.kind.word(),
            key.id,
            FactDoor::Break.word(),
        ));
    }
}
