// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHADOW: `aterm drive replay FILE… --shadow [--set k=v…|--policy FILE]`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

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
    /// After the first row that differs.
    pub taint: bool,
    /// What the re-decision needed and the tape did not hold.
    pub needs: Vec<String>,
    /// Why there is no candidate decision.
    pub note: Option<String>,
}

/// A shadow of one tape ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow {
    pub tape: String,
    /// The build that recorded it (`None`: its header does not read here).
    pub recorded_by: Option<String>,
    /// This build.
    pub shadowed_by: String,
    /// The candidate, as said.
    pub candidate: Vec<String>,
    /// Every decision on the tape, in order.
    pub rows: Vec<ShadowRow>,
}

impl Shadow {
    /// How many points the candidate decides otherwise.
    #[must_use]
    pub fn differs(&self) -> usize {
        self.rows.iter().filter(|r| r.differs == Some(true)).count()
    }

    /// The exit code `aterm drive replay --shadow` ends with: 1 when the candidate decides a point
    /// otherwise; else 3 when a point was not decided again (a turn end on another build, one the
    /// strict replay never reached, a box no decision came of): what the candidate would decide
    /// there the tape cannot say; else 0 — every point decided again, and as recorded.
    #[must_use]
    pub fn code(&self) -> u8 {
        if self.differs() > 0 {
            1
        } else if self.rows.iter().any(|r| r.differs.is_none()) {
            3
        } else {
            0
        }
    }

    /// The shadow as text: a heading, a line per decision, what each needed, and the BOUND line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let again = self.rows.iter().filter(|r| r.differs.is_some()).count();
        let _ = writeln!(
            out,
            "SHADOW tape {}, recorded by {}, shadowed by {}: {} decision(s), {} decided again, {} \
             otherwise",
            shown(&self.tape),
            shown(self.recorded_by.as_deref().unwrap_or("-")),
            shown(&self.shadowed_by),
            self.rows.len(),
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
        let _ = writeln!(out, "  {BOUND}");
        out
    }
}

/// The SHADOW of the tape `bytes` under `candidate` ([module docs](self)).
/// # Errors
/// A tape that does not read, one with no decision, a header this build cannot read (the candidate
/// is made from the policy it carries), a candidate the policy's reader refuses.
pub fn shadow(bytes: &[u8], candidate: &Candidate) -> Result<Shadow, String> {
    let tape = show::open(bytes, false)?;
    let book = Book::open(&tape);
    if book.decisions.is_empty() {
        return Err("no decisions on this tape".to_string());
    }
    let header = book.header.as_ref().map_err(Clone::clone)?;
    let policy = candidate.policy(&header.opts.policy)?;
    let shadowed_by = aterm_types::version::build_id();
    let (turn_ends, turn_note) = turn_ends(bytes, &book, &header.build, &shadowed_by, &policy);
    let mut rows: Vec<ShadowRow> = book
        .decisions
        .iter()
        .map(|d| {
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
        })
        .collect();
    if let Some(first) = rows.iter().position(|r| r.differs == Some(true)) {
        for row in &mut rows[first + 1..] {
            row.taint = true;
        }
    }
    Ok(Shadow {
        tape: header.tape.clone(),
        recorded_by: Some(header.build.clone()),
        shadowed_by,
        candidate: candidate.said(),
        rows,
    })
}

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
