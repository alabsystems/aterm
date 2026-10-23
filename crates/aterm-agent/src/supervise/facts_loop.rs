// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS SEAM IN THE SUPERVISION LOOP (`docs/DESIGN-hung-background-call-2026-10-20.md` R2 and
//! R3; system awareness §14.4): the session's told episodes, a no-progress fact riding a prompt
//! the turn-end policy types anyway, and the stuck fact's own line at a break of the agent's
//! background work (the occupancy design's §2.1 rank 4).
//!
//! ONCE PER EPISODE ACROSS BOTH DOORS: one [`FactsState`] for the session, seeded from the
//! ledger's fact rows at the loop's start (`Session::hold_seed_from`) under every `occupancy`
//! mode, told only where the act that carries the fact was taken, and forgotten only on a fresh
//! reading that no longer names the episode ([`FactsState::observe`]).
//!
//! THE ROWS: each door books an intent row (`fact-intent@v1`, `kind=no-progress episode=<hex>
//! door=<break|turn-end>`) before its write and an outcome row of the kind's rule
//! (`fact-no-progress@v1`, `typed` with the fact as its command, or `skipped why=<word>`) after
//! it, as the occupancy door books its lines (`occupancy::door`); a restart's seed reads them
//! back ([`door::seed_rows_in`]).

use std::time::{Duration, Instant};

use super::super::config::{BreakGrant, Occupancy, StuckBreakGrant, SupervisorConfig};
use super::super::facts::{EpisodeKey, Fact, FactsState};
use super::super::occupancy::door::{self, AtWrite, FactDoor, StuckVerdict, Told};
use super::super::occupancy::rules::Books;
use super::super::occupancy::templates;
use super::super::policy::turn_end::{TurnEndAction, carries_fact, fact_may_ride};
use super::approvals::Outcome;
use super::occupancy_loop::ASK_EVERY_MS;
use super::turn_end_loop::Written;
use super::*;

/// The session's facts: what it was told, the fact the decided act carries, and the stuck line's
/// own throttle and last word.
#[derive(Debug, Default)]
pub(super) struct FactsWatch {
    /// Which episodes the session was told.
    pub(super) state: FactsState,
    /// The fact the act decided last carries ([`Session::fact_for`]), told once it is typed.
    offered: Option<Fact>,
    /// The stuck episode whose write made its reads and wrote nothing, with when (the hold
    /// clock's milliseconds) its reads may be made again.
    again: Option<(EpisodeKey, i64)>,
    /// What the stuck line's last `FACT` line said, of which episode: once a change.
    said: Option<(EpisodeKey, StuckSaid)>,
    /// When (the hold clock's milliseconds) the stuck fact's own line last went, this loop's or
    /// a predecessor's ([`Session::stuck_lines_seed`]): the pace it keeps where the door types
    /// nothing ([`Session::stuck_books`]).
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
    /// THE FACT THE ACT DECIDED BEFORE THE FINAL PASS MAY CARRY: the session's pending untold one,
    /// from the host's reading ([`IdleHost::facts`]) folded into what the session was told.
    /// Asked of the host only where `action` is an act a fact rides
    /// ([`super::super::policy::turn_end::FACT_RIDES`]) and `[harness] facts` is on, and never on
    /// a tape from a build without the door.
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

    /// The decision is `action`: the fact it carries (`fact`, where it rode) waits for the act.
    pub(super) fn fact_offer(&mut self, fact: Option<Fact>, action: &TurnEndAction) {
        self.facts.offered = fact.filter(|f| carries_fact(action, f));
    }

    /// The decision that is about to be carried out: the fact `action` carries, its intent row
    /// booked before the write. `None` where it carries none.
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

    /// THE ACT THAT CARRIED `fact` WAS WRITTEN, or not (`skipped`: why, `not-written`, or
    /// `not-submitted` where the text stands in the composer unsent): its outcome row, and where
    /// it was typed the episode told, the person's record of it made as the break's line makes
    /// one ([`IdleHost::inform`], the same line), and the act's own text kept without it
    /// ([`super::super::policy::turn_end::TurnEndState::fact_rode`]).
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

    /// THE STUCK FACT AT A BREAK (§2.1's rank 4; `[harness] stuck_break_note`, which ships off):
    /// at a settled, drawn break of the agent's own background work begun at `at` and standing
    /// `waited`, where no turn end comes for a fact to ride, the session's untold no-progress fact
    /// is typed as a line of its own, `[aterm harness] aterm: <fact>.`, through the fenced write
    /// ([`Self::type_fact`]), once per episode. `line_gone`: this break's one line went already
    /// (the host's: the upgrade's notice, rank 3). Whether it typed.
    ///
    /// With the key off nothing here runs: no host call, no journal line, no row. On, every
    /// guard the occupancy door's line passes is judged, at the decider from what the loop holds
    /// and again at the write on its one `status` read and the host's answers
    /// ([`door::stuck_at_break`]); then the intent row, the write, and the outcome row. Typed, it
    /// is the door's kind of line ([`super::super::policy::turn_end::TurnEndState::fact_typed`]):
    /// the agent's taking it answers it, and the next line at a break waits for that.
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
                    &self.stuck_books(cfg),
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

    /// The stuck line judged at the decider: the guards from what the loop holds, `gone`, and
    /// the session's pace.
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
        door::stuck_at_break(Some(grant), &guards, gone, &self.stuck_books(cfg), clock_ms)
    }

    /// The books the stuck line keeps the session's pace by: the door's where the door really
    /// types (`occupancy = "prompt"`), else its own lines alone, since under `observe` the
    /// door's books hold lines that would have gone and never did.
    fn stuck_books(&self, cfg: &SupervisorConfig) -> Books {
        if cfg.occupancy == Occupancy::Prompt {
            return self.hold_books().clone();
        }
        let mut own = Books::default();
        if let Some(at_ms) = self.facts.lined_ms {
            own.lined(at_ms);
        }
        own
    }

    /// THE STUCK LINES A LOOP BEFORE THIS ONE TYPED AT A BREAK (`body`, the ledger read once at
    /// the loop's start at wall `now_ms`, its clock placed at `start_ms`): each still inside the
    /// session's pace counts toward it, as the door's own lines do through its seed.
    pub(super) fn stuck_lines_seed(&mut self, body: &str, start_ms: i64, now_ms: i64) {
        for row_ms in door::stuck_lines_in(body, self.setup.sid.as_deref()) {
            let ago = now_ms.saturating_sub(row_ms);
            if !(0..super::super::occupancy::rules::SESSION_PACE_MS).contains(&ago) {
                continue;
            }
            let at_ms = start_ms.saturating_sub(ago);
            self.door_lined(at_ms);
            self.facts.lined_ms = Some(self.facts.lined_ms.map_or(at_ms, |l| l.max(at_ms)));
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
        let why =
            match self.type_fact(BreakGrant::StuckBreak(grant), &point, &line, rule, review)? {
                Some(Written::Submitted) => None,
                None => Some("unfenced"),
                Some(Written::Not) => Some("not-written"),
                Some(Written::Left) => Some("not-submitted"),
            };
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
            self.facts.lined_ms = Some(clock_ms);
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

    /// The stuck line's `FACT` line for a verdict that typed nothing, once a change of what it
    /// says of its episode: `held:<guard>` or `paced`. A key that is off says nothing.
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
