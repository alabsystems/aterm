// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHADOW: `aterm drive replay FILE… --shadow [--set k=v…|--policy FILE]`.
//!
//! Each decision, decided again or not, goes in order to astream-effects' [`ShadowLedger`]
//! (`Astream.Effects.TlaShadowReplay`), which keeps the taint and the counts the exit code is
//! selected from. The ledger holds at most `MAX_SHADOW_ROWS` points: on a longer tape the first
//! decision it refuses ends the feed, and that one and every one after it are counted as not
//! decided again (`past the ledger:`), never decided.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use astream_effects::{ShadowDecisionOutcome, ShadowLedger, shadow_exit_code};

use super::acts::Candidate;
use super::doors;
use super::explain::Book;
use super::replay::replay_shadowed;
use super::report::{BOUND, Decided, Verdict};
use super::show::{self, shown};

/// One recorded decision and the candidate's on the same point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowRow {
    /// The mark's door: `decide.approval`, `decide.question`, `decide.turn_end`.
    pub door: String,
    /// The mark's ordinal (`#N` in `tape cat`).
    pub mark: u64,
    /// The judged screen's sequence.
    pub seq: u64,
    /// The recorded decision: its rule and outcome, as the mark says them.
    pub recorded_rule: Option<String>,
    pub recorded: String,
    /// The candidate's, where it was decided again.
    pub candidate_rule: Option<String>,
    pub candidate: Option<String>,
    /// Whether the candidate decided otherwise; `None` where it was not decided again.
    pub differs: Option<bool>,
    /// After the first row that differs: the ledger's point for this row says so.
    pub taint: bool,
    /// What the re-decision needed and the tape did not hold.
    pub needs: Vec<String>,
    /// Why there is no candidate decision.
    pub note: Option<String>,
}

/// The decisions the ledger refused: from the first one it refused to the tape's end, none
/// shadowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unshadowed {
    /// The first refused decision's mark ordinal.
    pub mark: u64,
    /// How many decisions, from it on.
    pub left: usize,
    /// The ledger's refusal.
    pub why: String,
}

/// A shadow of one tape.
#[derive(Debug, Clone)]
pub struct Shadow {
    pub tape: String,
    /// The build that recorded it (`None`: its header does not read here).
    pub recorded_by: Option<String>,
    /// This build.
    pub shadowed_by: String,
    /// The candidate, as said.
    pub candidate: Vec<String>,
    /// Every decision on the tape the ledger took, in order.
    pub rows: Vec<ShadowRow>,
    /// The rest, where the ledger refused one.
    pub unshadowed: Option<Unshadowed>,
    /// The rows, as astream's ledger holds them.
    ledger: ShadowLedger,
}

impl Shadow {
    /// How many points the candidate decides otherwise.
    #[must_use]
    pub fn differs(&self) -> usize {
        usize::try_from(self.ledger.differs_count()).unwrap_or(usize::MAX)
    }

    /// The exit code `aterm drive replay --shadow` ends with: 1 when the candidate decides a point
    /// otherwise; else 3 when a point was not decided again (a turn end on another build, one the
    /// strict replay never reached, a box no decision came of, a decision past the ledger): what
    /// the candidate would decide there the shadow cannot say; else 0 — every point decided
    /// again, and as recorded. With every decision in the ledger it is the ledger's own
    /// ([`ShadowLedger::exit_code`]); past it, the same selector ([`shadow_exit_code`]) over the
    /// ledger's counts with each decision it refused counted as not decided again, so a
    /// difference after the first refused decision is never seen.
    #[must_use]
    pub fn code(&self) -> u8 {
        let Some(refused) = &self.unshadowed else {
            return self.ledger.exit_code();
        };
        // The ledger's selector, over its counts and the decisions it refused, none decided
        // again: `shadow_exit_code` answers 0..=3 (`shadow_code_in_range`).
        let not_decided = self
            .ledger
            .unknown_count()
            .saturating_add(refused.left as u64);
        shadow_exit_code(self.ledger.differs_count(), not_decided) as u8
    }

    /// The shadow as text: a heading, a line per decision, what each needed, the decisions past
    /// the ledger (`past the ledger:`, never the batch's `not shadowed:`, which names a tape
    /// refused whole), and the BOUND line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let again = self.rows.iter().filter(|r| r.differs.is_some()).count();
        let left = self.unshadowed.as_ref().map_or(0, |u| u.left);
        let _ = writeln!(
            out,
            "SHADOW tape {}, recorded by {}, shadowed by {}: {} decision(s), {} decided again, {} \
             otherwise",
            shown(&self.tape),
            shown(self.recorded_by.as_deref().unwrap_or("-")),
            shown(&self.shadowed_by),
            self.rows.len().saturating_add(left),
            again,
            self.differs()
        );
        for said in &self.candidate {
            let _ = writeln!(out, "  candidate: {}", shown(said));
        }
        for row in &self.rows {
            let verdict = match (row.differs, row.taint) {
                (Some(true), false) => "OTHERWISE",
                (Some(true), true) => "OTHERWISE TAINT",
                (Some(false), false) => "same",
                (Some(false), true) => "same TAINT",
                (None, false) => "UNKNOWN",
                (None, true) => "UNKNOWN TAINT",
            };
            let _ = writeln!(
                out,
                "  #{} seq={} {} [{verdict}]",
                row.mark,
                row.seq,
                shown(&row.door)
            );
            let _ = writeln!(
                out,
                "    recorded  {}: {}",
                shown(row.recorded_rule.as_deref().unwrap_or("-")),
                shown(&row.recorded)
            );
            match &row.candidate {
                Some(candidate) => {
                    let _ = writeln!(
                        out,
                        "    candidate {}: {}",
                        shown(row.candidate_rule.as_deref().unwrap_or("-")),
                        shown(candidate)
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "    candidate: not decided again: {}",
                        shown(row.note.as_deref().unwrap_or("-"))
                    );
                }
            }
            for need in &row.needs {
                let _ = writeln!(out, "    needs: {}", shown(need));
            }
        }
        if let Some(refused) = &self.unshadowed {
            let _ = writeln!(
                out,
                "  past the ledger: {} decision(s) from #{} on, not decided again: {}",
                refused.left,
                refused.mark,
                shown(&refused.why)
            );
        }
        let _ = writeln!(out, "  {BOUND}");
        out
    }
}

/// The SHADOW of the tape `bytes` under `candidate`.
/// # Errors
/// A tape that does not read, one with no decision, a header this build cannot read (the candidate
/// is made from the policy it carries), a candidate the policy's reader refuses.
pub fn shadow(bytes: &[u8], candidate: &Candidate) -> Result<Shadow, String> {
    shadow_onto(bytes, candidate, ShadowLedger::new())
}

/// [`shadow`], its rows fed to `ledger` after the points it already holds.
pub(crate) fn shadow_onto(
    bytes: &[u8],
    candidate: &Candidate,
    mut ledger: ShadowLedger,
) -> Result<Shadow, String> {
    let tape = show::open(bytes, false)?;
    let book = Book::open(&tape);
    if book.decisions.is_empty() {
        return Err("no decisions on this tape".to_string());
    }
    let header = book.header.as_ref().map_err(Clone::clone)?;
    let policy = candidate.policy(&header.opts.policy)?;
    let shadowed_by = aterm_types::version::build_id();
    let (turn_ends, turn_note) = turn_ends(bytes, &book, &header.build, &shadowed_by, &policy);
    let made = book.decisions.iter().map(|d| {
        let mark = book.ordinal_of(d.item).unwrap_or(0);
        let mut row = ShadowRow {
            door: d.door.clone(),
            mark,
            seq: d.seq,
            recorded_rule: d.rule.clone(),
            recorded: d.outcome.clone(),
            candidate_rule: None,
            candidate: None,
            differs: None,
            taint: false,
            needs: Vec::new(),
            note: None,
        };
        if d.door == doors::DECIDE_TURN_END {
            match turn_ends.get(&mark) {
                Some(c) => {
                    row.differs = Some(c.outcome != d.outcome);
                    row.candidate_rule.clone_from(&c.rule);
                    row.candidate = Some(c.outcome.clone());
                }
                None => row.note = Some(turn_note.clone()),
            }
        } else {
            let why = book.explain(d, None, bytes, Some(&policy));
            row.differs = why.agrees.map(|agrees| !agrees);
            if row.differs.is_some() {
                row.candidate_rule = why.decided_by().map(str::to_string);
                row.candidate = why.decided;
            } else {
                row.note = why.trace_note;
            }
            row.needs = why.needs;
        }
        row
    });
    let (rows, unshadowed) = ledgered(&mut ledger, made, point_taint);
    Ok(Shadow {
        tape: header.tape.clone(),
        recorded_by: Some(header.build.clone()),
        shadowed_by,
        candidate: candidate.said(),
        rows,
        unshadowed,
        ledger,
    })
}

/// Feed `rows`, in order, to `ledger`: a row decided again as `Decided` with its `differs`, one
/// not as `Undecided` with its note, and each row's taint is what `taint` reads off the ledger
/// once its point is in. The first row the ledger refuses ends the feed: it is dropped, and every
/// row after it is counted, never made.
fn ledgered(
    ledger: &mut ShadowLedger,
    rows: impl ExactSizeIterator<Item = ShadowRow>,
    taint: fn(&ShadowLedger) -> bool,
) -> (Vec<ShadowRow>, Option<Unshadowed>) {
    let all = rows.len();
    let mut taken = Vec::new();
    for mut row in rows {
        let outcome = match row.differs {
            Some(differs) => ShadowDecisionOutcome::Decided {
                candidate: row.candidate.clone().unwrap_or_default(),
                differs,
            },
            None => ShadowDecisionOutcome::Undecided {
                reason: row.note.clone().unwrap_or_default(),
            },
        };
        if let Err(why) =
            ledger.record_point(row.mark, &row.door, &row.recorded, outcome, Vec::new())
        {
            let refused = Unshadowed {
                mark: row.mark,
                left: all - taken.len(),
                why,
            };
            return (taken, Some(refused));
        }
        row.taint = taint(ledger);
        taken.push(row);
    }
    (taken, None)
}

/// The taint of the point just recorded: `ledger.rows()[i].taint`, set on every point after the
/// first that differs and never on that point itself.
fn point_taint(ledger: &ShadowLedger) -> bool {
    ledger.rows().last().is_some_and(|point| point.taint)
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod tests;

/// The candidate's decision at each turn end on the tape, by its mark's
/// ordinal — from a strict replay on the recording `build` alone — and what
/// a turn end it did not reach is listed with.
fn turn_ends(
    bytes: &[u8],
    book: &Book<'_>,
    build: &str,
    this_build: &str,
    policy: &crate::supervise::SupervisorConfig,
) -> (BTreeMap<u64, Decided>, String) {
    if !book
        .decisions
        .iter()
        .any(|d| d.door == doors::DECIDE_TURN_END)
    {
        return (BTreeMap::new(), String::new());
    }
    if build != this_build {
        return (
            BTreeMap::new(),
            format!(
                "recorded by {build} and shadowed by {this_build}: a turn end decides on a fold \
                 over the whole run, which only a strict replay on the recording build rebuilds"
            ),
        );
    }
    match replay_shadowed(bytes, policy.clone()) {
        Ok((report, rows)) => {
            let note = match &report.verdict {
                Verdict::Reproduced | Verdict::Ended | Verdict::Reached { .. } => {
                    "the strict replay that rebuilds the fold did not reach it".to_string()
                }
                _ => format!(
                    "the strict replay that rebuilds the fold stopped before it: {}",
                    report.render().lines().next().unwrap_or("")
                ),
            };
            (rows.into_iter().collect(), note)
        }
        Err(e) => (BTreeMap::new(), format!("the tape does not replay: {e}")),
    }
}
