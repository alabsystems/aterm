// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CANDIDATE GATE (D27 in `$HOME/trust` docs/DECISION_QUEUE.md): a Trust
//! toolchain is published only when, on the same aterm
//! commit and the same machine, it proves every library at least as well as
//! the PUBLISHED seal does, and gives every library a result within a bounded
//! time. `tools/trust-gate-candidate.sh` runs the whole-tree lane
//! (`tools/trust-gate-all.sh`) twice — the published seal first, then the
//! candidate, each in a target root of its own under the same per-crate wall
//! budget — and asks this module, `aterm-release trust compare`, for the
//! verdict.
//!
//! WHY NOT THE LANE'S OWN VERDICT. Each run's judge holds its library to the
//! floors in `tools/trust-gate-ratchet.tsv`, which name the prover that
//! recorded them: a candidate inherits every floor (a `DRIVER CHANGED:` note),
//! and the published seal's own run fails wherever this host cannot reproduce
//! a floor recorded on a larger one. Neither run's verdict, and neither run's
//! STATUS column, says what the CANDIDATE proves against the SEAL. And the
//! judge prints `-` for the proved count of a CAP-BOUND or `NOT-VERIFIED(clock)`
//! row, so its table hides numbers a comparison needs (the seal's aterm-grid,
//! 986 proved, was `NOT-VERIFIED(clock,<987,48)` there). So this compares what
//! each run MEASURED — [`Measured`], read from each check's own log by the
//! judge's own reader ([`Plan::parse`], [`Plan::measure`]), nothing read twice
//! and no rule copied — library against library.
//!
//! ONE SIDE OF ONE LIBRARY ([`Side`]) is read in the judge's order: the lane's
//! wall budget ended it (OVER-BUDGET), targo would not start it
//! (NOT-VERIFIED(trustd)), verification did not run (NOT-VERIFIED(rc)), it did
//! not finish (NOT-VERIFIED(incomplete): a transport row, a signal, no complete
//! Coverage line), it left no Summary (GATE-BROKE) or one that does not parse —
//! and only then a RESULT, its proved and total counts. A library refused up
//! front for memory, or one the tree's own lists never gate, is that decision.
//!
//! PER OBLIGATION, NOT PER COUNT ([`crate::trust_oblig`]). Where
//! both checks gave a result, the candidate must prove EVERY OBLIGATION the
//! seal proved — each at its exact key (function, kind, site, description) —
//! read from the per-obligation record each check left beside its log (the
//! lane's `--obligations`). Obligations one key holds more than once (TWINS)
//! are told apart by what else both runs report of them, or else held to
//! their worst case: a candidate proving a different twin than the seal did
//! has lost one. Gains never offset a loss: a count hides one behind the other
//! (trust main, 2026-10-02: aterm-grid lost 147 seal-proved obligations and
//! gained 87, -60 by count). A loss is excused only by an audited row of the
//! RETRACTION LEDGER (`tools/trust-gate-retractions.tsv`, `--retractions`),
//! and only the very obligation it was audited for: its row applies only where
//! the aterm tree both runs measured is the tree of the commit it was audited
//! at, but for the ledger itself (read in the repository the ledger is in), the
//! base ran the seal it was audited against, and the obligation its key names
//! is at the site it records — anywhere else it is NOT-APPLIED (a key's place
//! among look-alikes names another obligation once the tree or the seal
//! changes). The seal's proof was FALSE, and the
//! trust commit that retracts it — which the candidate's own trust must hold
//! (`--candidate-trust-commit`, checked against its run's driver line, in
//! `--trust-repo`: an ancestor, not reverted since, its reproducer still in
//! the candidate's tree) — is named with the trust test that reproduces the
//! false proof. A retraction the candidate still proves is STALE, one already
//! in the published seal is wrong, and either refuses; so does a ledger with
//! rows read where that cannot be told (the seal's commit unknown to
//! `--trust-repo`, a shallow or grafted `--trust-repo`; git there runs with no
//! inherited `GIT_*`, no global config and replace refs ignored). One that
//! names nothing the seal proved is UNUSED (said, to be pruned). A seal
//! obligation the candidate reports nothing at (MISSING) no retraction
//! excuses. Two runs whose plans do not both say they captured the records are
//! COULD NOT COMPARE, and a library whose record is missing, cut short, of
//! another package, or not its Summary's counts gives no result on that side.
//!
//! THE OUTCOME per library ([`Outcome`]), candidate against base:
//!
//! * OK — both have a result, and every obligation the seal proved the
//!   candidate proves too, but those the ledger excuses;
//! * LOSS — an obligation the seal proved that the candidate does not, and no
//!   retraction excuses (each one listed, `LOST`); without both records, the
//!   candidate proving fewer than the seal: than its result here, or, where the
//!   seal's own check gave none, than what the seal is on record proving
//!   ([`Record`]). A shortfall the candidate's lapsed clocks or trustd refusals
//!   could cover is named as possibly the machine's, and refused all the same:
//!   re-run it;
//! * NO-RESULT — the candidate gave no result: over its wall budget, not
//!   started, not run, not finished, no Summary, no usable per-obligation record;
//! * OK-FLOOR — what runs that captured no per-obligation records (which are
//!   COULD NOT COMPARE) say where the published seal's own check gave no
//!   result, for a reason that is the seal's (over the same budget, did not
//!   finish without a signal, verification did not run, broke with an exit
//!   status, an unparseable Summary), and the candidate proves at least what
//!   the seal is on record proving ([`Record`]: the floor the seal's own prover
//!   recorded in the ratchet, or the count its unfinished check here reached,
//!   whichever is higher). A count never stands for obligations: compared
//!   obligation by obligation, the same row is NO-BASELINE;
//! * NO-BASELINE — the same, with nothing on record of WHICH obligations the
//!   seal proves there (no record at all, or only a count, which a candidate
//!   proving none of the seal's obligations can meet): refused (fail closed).
//!   Run the seal so that it finishes the library;
//! * BASE-UNDECIDED — the seal's check gave no result for a reason that is the
//!   machine's, not the seal's (trustd kept it from starting; it, or its
//!   compiler, was ended by a signal no budget sent; it left no exit status), or
//!   left no usable per-obligation record: refused, re-run the base;
//! * NOT-ON-HOST — refused up front for memory under either toolchain: nothing
//!   was compared, and the gate refuses, naming what the run's own refusal
//!   names (the need, this host's ceiling, the RAM that verifies it);
//! * SKIPPED — the same policy decision under both (NO-LIB, EXCLUDED, a queued
//!   fork): the tree's choice, not the compiler's;
//! * INPUTS-DIFFER — one run selected it and the other did not, or they decided
//!   it differently, or its records lose fewer obligations than its counts do
//!   (impossible for records that are their checks'): the two runs did not
//!   measure the same thing.
//!
//! THE RUNS AS A WHOLE must be comparable, or nothing is (COULD NOT COMPARE):
//! both concluded (the plan's PASS 3 `trustd` line), both measured the whole
//! tree a release reads (a `file` section with no reason it may not file:
//! no `ATERM_GATE_ONLY`, no `--per-commit`, the tree's own roster and scope
//! list, a clean checkout that did not change during the run), on the same
//! commit and tree, on the same machine (RAM, and the cores the lane read),
//! under the same per-crate wall budget (`crate-budget`, which each plan must
//! record) and slot count (`jobs`), with two different provers (the same prover
//! twice compares nothing), each library named once in each plan (one log is
//! read per name), each run's per-obligation records captured, and at least one
//! library with a result under both — and, when the caller names it, the base's
//! prover is the published seal's signed tree_root. A retraction ledger that
//! does not read, names a commit or reproducer the trust repository lacks, a
//! commit already in the seal, or one the candidate contradicts (STALE), and a
//! trust commit its run's driver line disagrees with, are COULD NOT COMPARE too.
//!
//! EXIT: 0 `VERDICT: PUBLISH-OK` — only OK and SKIPPED rows (OK-FLOOR is
//! said only by runs that compare nothing, exit 2);
//! 1 `VERDICT: PUBLISH-REFUSED` — a LOSS, NO-RESULT, NO-BASELINE,
//! BASE-UNDECIDED or NOT-ON-HOST row; 2 `VERDICT: PUBLISH-REFUSED — COULD NOT
//! COMPARE`, which outranks 1 (the findings are not of two comparable runs).
//! Anything but 0 publishes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::ledger::GitRunner;
use crate::trust_facts::{
    Crate, Decision, Host, Measured, Plan, floor_recorded_by, memory_refusal,
    memory_refusal_fallback, one_line, parse_summary,
};
use crate::trust_oblig::{
    self, AtermRepo, Audited, Contains, LibCmp, Retraction, TrustGit, TrustRepo, compare_lib,
    driver_hashes, full_sha, parse_ledger,
};

/// A finished check's counts, and what could have taken proofs from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scored {
    pub proved: u64,
    pub total: u64,
    /// Obligations whose wall-clock budget lapsed (`timed_out=`), plus the proof
    /// checks ay's caller cancelled: the clock's share of a shortfall.
    pub clock: u64,
    /// Obligations trustd refused admission.
    pub refused: u64,
    /// Bodies skipped at the verifier's memory ceiling, plus the functions that
    /// ran out of the per-function memory budget.
    pub ceiling: u64,
}

/// What one run decided, or measured, about one library — read in the judge's
/// own order ([`crate::trust_facts::judge`]), never against a floor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// A finished check with a Summary that parses.
    Result(Scored),
    /// The lane ended the check at its per-crate wall budget (`<log>.budget`).
    OverBudget(String),
    /// targo would not start the check: no trustd of its build could serve it.
    Trustd(String),
    /// Verification did not run (`verification did not run`, the
    /// `transport:missing-json` row), with the check's exit status.
    NotRun(String),
    /// The check did not finish ([`Measured::ended`]), and whether a signal
    /// ended it or its compiler.
    Incomplete { ended: String, signal: bool },
    /// The check left no Summary, with its exit status (`?` when none was
    /// recorded).
    Broke(String),
    /// A Summary whose counts do not parse.
    Unparsed,
    /// Refused up front for memory (`trust admit`): never run. With the run's
    /// own refusal line (the need, this host's ceiling, the RAM that verifies
    /// it), else what of those is known.
    Memory(String),
    /// Not gated by the tree's own decision, as the judge prints it.
    Policy(String),
}

/// Whether a check that did not finish was ENDED BY A SIGNAL: its own exit
/// status was 128+N, or cargo saw its compiler end by one — the two phrases
/// [`Measured::of_log`] writes into [`Measured::ended`] (`the check itself
/// ended by signal N`, `cargo: <program> ended by signal N`).
fn ended_by_signal(m: &Measured) -> bool {
    m.rc.trim()
        .parse::<u32>()
        .is_ok_and(|c| (129..=192).contains(&c))
        || m.ended.contains(" ended by signal ")
}

impl Side {
    /// Read one library of one run, on the run's `host`.
    #[must_use]
    pub fn of(host: &Host, c: &Crate) -> Side {
        let m = match &c.decision {
            Decision::Gate(m) => m,
            Decision::Memory(seen) => {
                return Side::Memory(
                    memory_refusal(host, &c.name, true, &c.memory, seen.as_ref())
                        .map_or_else(|| memory_refusal_fallback(host, &c.memory), |r| r.line),
                );
            }
            Decision::Excluded => return Side::Policy("EXCLUDED".into()),
            Decision::FullOnly => return Side::Policy("FULL-ONLY".into()),
            Decision::NoLib => return Side::Policy("NO-LIB".into()),
            Decision::Undeclared => return Side::Policy("FORK-UNDECLARED".into()),
            Decision::Queued(s) => return Side::Policy(format!("QUEUED({s})")),
            Decision::Ambiguous => return Side::Policy("AMBIGUOUS-SPEC".into()),
        };
        if !m.wall_budget.is_empty() {
            return Side::OverBudget(m.wall_budget.clone());
        }
        if !m.launch.is_empty() {
            return Side::Trustd(m.launch.clone());
        }
        if m.transport {
            return Side::NotRun(m.rc.clone());
        }
        if !m.ended.is_empty() {
            return Side::Incomplete {
                ended: m.ended.clone(),
                signal: ended_by_signal(m),
            };
        }
        if m.summary.is_empty() {
            return Side::Broke(m.rc.clone());
        }
        match parse_summary(&m.summary) {
            (Some(proved), Some(total)) => Side::Result(Scored {
                proved,
                total,
                clock: m.lapsed.saturating_add(m.cancelled),
                refused: m.refused,
                ceiling: m.skipped.saturating_add(m.budget_rows),
            }),
            _ => Side::Unparsed,
        }
    }

    /// The status, as the judge's STATUS column says it — or `<proved>/<total>`.
    #[must_use]
    pub fn status(&self) -> String {
        match self {
            Side::Result(s) => format!("{}/{}", s.proved, s.total),
            Side::OverBudget(b) => format!("OVER-BUDGET({b}s)"),
            Side::Trustd(_) => "NOT-VERIFIED(trustd)".into(),
            Side::NotRun(rc) => format!("NOT-VERIFIED(rc={rc})"),
            Side::Incomplete { .. } => "NOT-VERIFIED(incomplete)".into(),
            Side::Broke(rc) => format!("GATE-BROKE(rc={rc})"),
            Side::Unparsed => "SUMMARY-UNPARSED".into(),
            Side::Memory(_) => "REFUSED(memory)".into(),
            Side::Policy(w) => w.clone(),
        }
    }

    /// The TSV's word for the side: `RESULT`, else its status.
    fn word(&self) -> String {
        match self {
            Side::Result(_) => "RESULT".into(),
            other => other.status(),
        }
    }

    /// What its check did, said after "the <toolchain>'s check".
    fn said(&self) -> String {
        match self {
            Side::Result(s) => format!("proved {} of {}", s.proved, s.total),
            Side::OverBudget(b) => format!(
                "was still running at its per-crate wall budget of {b} s, and the lane ended it there"
            ),
            Side::Trustd(why) => {
                format!("never started: targo found no trustd of its own build to serve it ({why})")
            }
            Side::NotRun(rc) => format!("did not run verification (exit {rc})"),
            Side::Incomplete { ended, .. } => format!("did not finish ({ended})"),
            Side::Broke(rc) => format!("left no Summary (exit {rc})"),
            Side::Unparsed => "left a Summary whose counts do not parse".into(),
            Side::Memory(_) => "was refused up front for memory".into(),
            Side::Policy(w) => format!("was not gated ({w})"),
        }
    }

    /// For a base that gave no result: whether that is the SEAL's own, so a
    /// candidate result is held to what the seal is on record proving
    /// (OK-FLOOR, LOSS or NO-BASELINE), rather than the machine's, which
    /// decides nothing (BASE-UNDECIDED). A signal no budget sent, a check
    /// trustd kept from starting, or one that left no exit status is the
    /// machine's.
    fn seals_own(&self) -> bool {
        match self {
            Side::OverBudget(_) | Side::NotRun(_) | Side::Unparsed => true,
            Side::Incomplete { signal, .. } => !signal,
            Side::Broke(rc) => rc
                .trim()
                .parse::<u32>()
                .is_ok_and(|c| !(129..=192).contains(&c)),
            Side::Trustd(_) | Side::Result(_) | Side::Memory(_) | Side::Policy(_) => false,
        }
    }
}

/// What the published seal is ON RECORD proving of a library its own check here
/// left without a result: the floor its own prover recorded in the ratchet (the
/// row's fourth column is the seal's `--version` line), or the proved count its
/// unfinished check's Summary reached on this host, whichever is higher — the
/// least a candidate must prove there. `None` when neither exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub proved: u64,
    /// Where the number comes from, said.
    pub from: String,
}

/// The ratchet the comparison reads [`Record`]s from: its text, or why there
/// is none.
#[derive(Clone, Debug, Default)]
pub struct Ratchet {
    pub text: Option<String>,
    /// The file, as named.
    pub path: String,
}

/// What the seal is on record proving of `c` (its run's crate), when its own
/// check there gave no result: [`Record`].
fn record_of(c: &Crate, ratchet: &Ratchet, driver_row: &str) -> Option<Record> {
    let floor = ratchet
        .text
        .as_deref()
        .and_then(|t| floor_recorded_by(t, &c.name, driver_row));
    let partial = match &c.decision {
        Decision::Gate(m) if !m.summary.is_empty() => parse_summary(&m.summary).0,
        _ => None,
    };
    let floor_said = |f: u64| {
        format!(
            "its floor of {f} in {}, recorded by the seal's own prover ({driver_row})",
            ratchet.path
        )
    };
    let partial_said =
        |p: u64| format!("the {p} its own unfinished check here counted before it stopped");
    match (floor, partial) {
        (Some(f), Some(p)) if p > f => Some(Record {
            proved: p,
            from: format!("{}; {}", partial_said(p), floor_said(f)),
        }),
        (Some(f), Some(p)) => Some(Record {
            proved: f,
            from: format!("{}; {}", floor_said(f), partial_said(p)),
        }),
        (Some(f), None) => Some(Record {
            proved: f,
            from: floor_said(f),
        }),
        (None, Some(p)) => Some(Record {
            proved: p,
            from: partial_said(p),
        }),
        (None, None) => None,
    }
}

/// What the comparison decided about one library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Loss,
    NoResult,
    NoBaseline,
    BaseUndecided,
    NotOnHost,
    InputsDiffer,
    Floor,
    Skipped,
    Ok,
}

impl Outcome {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Ok => "OK",
            Outcome::Loss => "LOSS",
            Outcome::NoResult => "NO-RESULT",
            Outcome::Floor => "OK-FLOOR",
            Outcome::NoBaseline => "NO-BASELINE",
            Outcome::BaseUndecided => "BASE-UNDECIDED",
            Outcome::NotOnHost => "NOT-ON-HOST",
            Outcome::Skipped => "SKIPPED",
            Outcome::InputsDiffer => "INPUTS-DIFFER",
        }
    }

    /// An outcome that refuses the publish (exit 1).
    #[must_use]
    pub fn refuses(self) -> bool {
        matches!(
            self,
            Outcome::Loss
                | Outcome::NoResult
                | Outcome::NoBaseline
                | Outcome::BaseUndecided
                | Outcome::NotOnHost
        )
    }
}

/// What the per-obligation records say of one library whose checks both gave
/// a result ([`crate::trust_oblig`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Oblig {
    /// Both records read and compared.
    Compared(LibCmp),
    /// A side's record cannot be used, and why: no result on that side.
    Unusable {
        base: Option<String>,
        cand: Option<String>,
    },
}

/// One library, both sides and the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub base: Option<Side>,
    pub cand: Option<Side>,
    pub outcome: Outcome,
    /// What the seal is on record proving, when its own check gave no result
    /// and the candidate was held to that ([`Record`]).
    pub record: Option<Record>,
    /// The per-obligation comparison, when the runs captured the records.
    pub oblig: Option<Oblig>,
    /// Why, in one line.
    pub why: String,
}

impl Row {
    /// What the candidate is held to: the seal's result, else the record it
    /// was held to.
    #[must_use]
    pub fn against(&self) -> Option<u64> {
        match (&self.base, &self.record) {
            (Some(Side::Result(b)), _) => Some(b.proved),
            (_, Some(r)) => Some(r.proved),
            _ => None,
        }
    }

    /// The candidate's proved count less what it was held to.
    #[must_use]
    pub fn delta(&self) -> Option<i128> {
        match (&self.cand, self.against()) {
            (Some(Side::Result(c)), Some(b)) => Some(i128::from(c.proved) - i128::from(b)),
            _ => None,
        }
    }

    /// The obligations the seal proved that the candidate does not, unexcused,
    /// when the records were compared.
    #[must_use]
    pub fn lost(&self) -> Option<usize> {
        match &self.oblig {
            Some(Oblig::Compared(c)) => Some(c.lost.len()),
            _ => None,
        }
    }

    /// How a refusal names its loss: the obligations lost, else the count's.
    fn loss_said(&self) -> String {
        match self.lost() {
            Some(n) => format!("{} {n} lost", self.name),
            None => format!("{} {}", self.name, self.delta().unwrap_or(0)),
        }
    }
}

/// A delta as the table prints it: `+2`, `0`, `-1`.
fn signed(d: i128) -> String {
    if d > 0 {
        format!("+{d}")
    } else {
        d.to_string()
    }
}

/// `1 obligation`, `2 obligations`.
fn obligations(n: usize) -> String {
    if n == 1 {
        "1 obligation".into()
    } else {
        format!("{n} obligations")
    }
}

/// Decide one library ([`Outcome`] in the module docs). `record` is what the
/// seal is on record proving of it, read only when its own check gave no
/// result; `oblig` what the per-obligation records say, when the runs captured
/// them.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn decide(
    name: &str,
    base: Option<&Side>,
    cand: Option<&Side>,
    record: Option<&Record>,
    oblig: Option<&Oblig>,
    per_obligation: bool,
) -> Row {
    let row = |outcome, why: String, record: Option<&Record>| Row {
        name: name.into(),
        base: base.cloned(),
        cand: cand.cloned(),
        outcome,
        record: record.cloned(),
        oblig: oblig.cloned(),
        why: one_line(&why),
    };
    let (b, c) = match (base, cand) {
        (Some(b), Some(c)) => (b, c),
        (None, _) => {
            return row(
                Outcome::InputsDiffer,
                "only the candidate's run selected it: the runs did not measure the same tree"
                    .into(),
                None,
            );
        }
        (_, None) => {
            return row(
                Outcome::InputsDiffer,
                "only the published seal's run selected it: the runs did not measure the same tree"
                    .into(),
                None,
            );
        }
    };
    if matches!(b, Side::Memory(_)) || matches!(c, Side::Memory(_)) {
        let under = match (b, c) {
            (Side::Memory(_), Side::Memory(_)) => "under both toolchains",
            (Side::Memory(_), _) => "under the published seal",
            _ => "under the candidate",
        };
        // Whichever run has a refusal line says it, without the library's name
        // and "not verified here" the row already carries.
        let said = [c, b].into_iter().find_map(|s| match s {
            Side::Memory(line) => Some(
                line.strip_prefix(&format!("{name} not verified here: "))
                    .unwrap_or(line)
                    .to_string(),
            ),
            _ => None,
        });
        let why = said.unwrap_or_default();
        return row(
            Outcome::NotOnHost,
            format!("refused up front for memory {under}: {why}"),
            None,
        );
    }
    match (b, c) {
        (Side::Policy(x), Side::Policy(y)) if x == y => {
            return row(
                Outcome::Skipped,
                format!("{x} under both: the tree's own decision, not the compiler's"),
                None,
            );
        }
        (Side::Policy(_), _) | (_, Side::Policy(_)) => {
            return row(
                Outcome::InputsDiffer,
                format!(
                    "the runs decided it differently ({} under the published seal, {} under the candidate)",
                    b.status(),
                    c.status()
                ),
                None,
            );
        }
        _ => {}
    }
    let Side::Result(cs) = c else {
        return row(
            Outcome::NoResult,
            format!(
                "the candidate's check {}, so it gave no result and nothing says it proves what the published seal proves (the published seal's check {})",
                c.said(),
                b.said()
            ),
            None,
        );
    };
    let (against, record_said, record) = match b {
        Side::Result(bs) => (bs.proved, None, None),
        _ if !b.seals_own() => {
            return row(
                Outcome::BaseUndecided,
                format!(
                    "the published seal's own check {} — the machine's doing, not the seal's, so nothing says what the seal proves here; run the gate again",
                    b.said()
                ),
                None,
            );
        }
        _ => match record {
            Some(r) => (r.proved, Some(r.from.as_str()), Some(r)),
            None => {
                return row(
                    Outcome::NoBaseline,
                    format!(
                        "the published seal's own check {}, and nothing records what the seal proves of it (no floor of its own prover, no count from its check): the candidate's {} of {} is refused — record the seal's floor first (a run of the seal that finishes it, --update-ratchet)",
                        b.said(),
                        cs.proved,
                        cs.total
                    ),
                    None,
                );
            }
        },
    };
    let slack = cs.clock.saturating_add(cs.refused);
    // What could have taken proofs from the candidate: said on a loss.
    let machine = |why: &mut String, short: u64| {
        if slack >= short {
            why.push_str(&format!(
                "; within what the candidate's run let lapse or trustd refused ({} lapsed, {} refused), so it may be the machine's — run the gate again; refused until then",
                cs.clock, cs.refused
            ));
        }
        if cs.ceiling > 0 {
            why.push_str(&format!(
                "; the candidate's verifier ran out of its memory ceiling ({} bodies or functions)",
                cs.ceiling
            ));
        }
    };
    let counts = format!(
        "{} proved under the candidate against {against} under the published seal ({})",
        cs.proved,
        signed(i128::from(cs.proved) - i128::from(against))
    );
    // PER OBLIGATION: both checks gave a result, and the runs captured records.
    if let (Side::Result(bs), Some(Oblig::Compared(cmp))) = (b, oblig) {
        let excused = cmp.excused.len();
        let also = |out: &mut String| {
            if excused > 0 {
                out.push_str(&format!(
                    "; {} more excused by the retraction ledger",
                    obligations(excused)
                ));
            }
            if cmp.gained > 0 {
                out.push_str(&format!(
                    "; {} gained, which offset nothing",
                    obligations(usize::try_from(cmp.gained).unwrap_or(usize::MAX))
                ));
            }
        };
        if !cmp.lost.is_empty() {
            let missing = match cmp.missing() {
                0 => String::new(),
                m => format!(
                    " ({m} of them MISSING: the candidate reports nothing at their key, which no retraction excuses)"
                ),
            };
            let mut why = format!(
                "{} the published seal proved {} not proved by the candidate, in {} function{}{missing}",
                obligations(cmp.lost.len()),
                if cmp.lost.len() == 1 { "is" } else { "are" },
                cmp.functions_lost(),
                if cmp.functions_lost() == 1 { "" } else { "s" },
            );
            also(&mut why);
            why.push_str(&format!("; {counts}"));
            machine(&mut why, cmp.lost.len() as u64);
            return row(Outcome::Loss, why, None);
        }
        // A count shortfall the records do not lose cannot be: the records
        // were held to their checks' Summaries, so each proof fewer is an
        // obligation fewer. The old count rule stands as this assertion.
        let short = bs.proved.saturating_sub(cs.proved);
        if short > excused as u64 {
            return row(
                Outcome::InputsDiffer,
                format!(
                    "the counts say the candidate proves {short} fewer, and its record loses {excused} (excused) and nothing more: the records are not the checks' ({counts})"
                ),
                None,
            );
        }
        let mut why = format!(
            "every obligation the published seal proved is proved by the candidate; {} proved under the candidate, {against} under the published seal",
            cs.proved
        );
        also(&mut why);
        return row(Outcome::Ok, why, None);
    }
    if cs.proved < against {
        let short = against - cs.proved;
        let mut why = match record_said {
            None => format!(
                "{} proved under the candidate against {against} under the published seal (-{short})",
                cs.proved
            ),
            Some(from) => format!(
                "{} proved under the candidate against the {against} the published seal is on record proving ({from}) (-{short}); the published seal's own check here {}",
                cs.proved,
                b.said()
            ),
        };
        machine(&mut why, short);
        if let Some(Oblig::Unusable { base: ub, cand: uc }) = oblig {
            why.push_str(&format!(
                "; and no per-obligation record names what was lost ({})",
                [uc.as_deref(), ub.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        return row(Outcome::Loss, why, record);
    }
    if let Some(Oblig::Unusable { base: ub, cand: uc }) = oblig {
        if let Some(why) = uc {
            return row(
                Outcome::NoResult,
                format!(
                    "the candidate's check left no usable per-obligation record ({why}), so nothing shows it proves each obligation the published seal proves ({counts})"
                ),
                None,
            );
        }
        if let Some(why) = ub {
            return row(
                Outcome::BaseUndecided,
                format!(
                    "the published seal's check left no usable per-obligation record ({why}), so nothing says which obligations it proves here; run the gate again ({counts})"
                ),
                None,
            );
        }
    }
    match record_said {
        None => row(
            Outcome::Ok,
            format!(
                "{} proved under the candidate, {against} under the published seal",
                cs.proved
            ),
            None,
        ),
        // A COUNT is no record of WHICH obligations the seal proves: under the
        // per-obligation comparison it holds a candidate to nothing: a
        // candidate proving the seal's count but none of its obligations would
        // pass.
        Some(from) if per_obligation => row(
            Outcome::NoBaseline,
            format!(
                "the published seal's own check {}; it is on record proving {against} by count ({from}), and the candidate proved {} of {}, but no record says WHICH obligations the seal proves here, and the candidate must prove each one: refused — a run of the seal that finishes it",
                b.said(),
                cs.proved,
                cs.total
            ),
            record,
        ),
        Some(from) => row(
            Outcome::Floor,
            format!(
                "the published seal's own check {}; the candidate proved {} of {}, at least the {against} the seal is on record proving ({from})",
                b.said(),
                cs.proved,
                cs.total
            ),
            record,
        ),
    }
}

/// One loaded run.
struct Run<'a> {
    plan: &'a Plan,
    what: &'static str,
}

impl Run<'_> {
    fn file_field(&self, f: impl Fn(&crate::trust_facts::FilePlan) -> &str) -> String {
        self.plan
            .file
            .as_ref()
            .map_or_else(|| "-".to_string(), |p| f(p).to_string())
    }

    /// Why this run is no whole-tree measurement of a tree a release reads, if
    /// it is not.
    fn not_whole(&self) -> Vec<String> {
        let (what, f) = (self.what, &self.plan.facts);
        let mut out = Vec::new();
        if f.trustd.is_empty() {
            out.push(format!(
                "the {what} run did not conclude: its plan has no PASS 3 `trustd` line, so the lane was ended before its judge read the logs"
            ));
        }
        if !f.only.is_empty() {
            out.push(format!(
                "the {what} run was narrowed by ATERM_GATE_ONLY ({}), so it is no whole-tree measurement",
                f.only
            ));
        }
        if f.per_commit {
            out.push(format!(
                "the {what} run was a --per-commit run, which skips the full-only libraries"
            ));
        }
        match &self.plan.file {
            None if f.only.is_empty() && !f.per_commit => out.push(format!(
                "the {what} run's plan has no filing section, so nothing says which tree it measured"
            )),
            None => {}
            Some(p) => {
                if !p.refused.is_empty() {
                    out.push(format!(
                        "the {what} run measured no tree a release reads: {}",
                        p.refused
                    ));
                }
                if p.commit.is_empty() || p.commit == "-" || p.tree.is_empty() || p.tree == "-" {
                    out.push(format!(
                        "the {what} run names no commit and tree it measured"
                    ));
                }
                if p.prover.is_empty() {
                    out.push(format!(
                        "the {what} run's prover could not be identified (no trustc beside its targo?)"
                    ));
                }
            }
        }
        if self.plan.crate_budget.is_empty() {
            out.push(format!(
                "the {what} run's plan records no per-crate wall budget (`crate-budget`), so nothing says its checks were bounded at all"
            ));
        }
        if !self.plan.obligations {
            out.push(format!(
                "the {what} run captured no per-obligation records (its plan has no `obligations 1` line: the lane ran without --obligations, or predates it), so neither a loss behind a gain in one library nor an audited retraction can be told from what it measured"
            ));
        }
        // One log is read per name (`Plan::parse` keeps the last spec of a name):
        // a name listed twice hides one of its checks from the comparison.
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &f.crates {
            *count.entry(c.name.as_str()).or_default() += 1;
        }
        for (name, times) in count.iter().filter(|(_, n)| **n > 1) {
            out.push(format!(
                "the {what} run lists {name} {times} times: one log is read per name, so the comparison cannot see each of its checks"
            ));
        }
        out
    }
}

/// The comparison of two runs.
#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// The header line.
    pub header: String,
    pub rows: Vec<Row>,
    /// Why the two runs cannot be compared at all; empty when they can.
    pub incomparable: Vec<String>,
    /// What the report says beside the findings (the ledger's, the commits').
    pub notes: Vec<String>,
    /// The retraction ledger's path, as given.
    pub ledger: String,
    /// The aterm commit both runs measured (`-` when they name none, or not
    /// the same one): a ledger row's aterm commit.
    pub aterm: String,
    /// The published seal's trust commit, as given and held to the base run's
    /// driver line (`-` when it cannot be used): a ledger row's seal.
    pub seal: String,
}

/// The retraction ledger and what applying it needs ([`crate::trust_oblig`]).
#[derive(Default)]
pub struct Retractions<'a> {
    /// The ledger's text and the path it was read from; `None`, none given (no
    /// loss is excused).
    pub ledger: Option<(String, String)>,
    /// The trust repository its commits and reproducers are read in.
    pub trust: Option<TrustRepo<'a>>,
    /// The aterm repository a row's audited commit is read in, when it is not
    /// the commit the runs measured (`trust compare` reads it where the
    /// ledger is); `None`, such a row does not apply.
    pub aterm: Option<AtermRepo<'a>>,
    /// The candidate's trust commit (its trustc's `commit-hash`).
    pub candidate_commit: Option<String>,
    /// The published seal's.
    pub base_commit: Option<String>,
}

/// A run's trust commit as given, held to its run's driver line: `Ok` when it
/// is 40 hex and every hash the driver names is its prefix; `Err(why)` when it
/// cannot be used (no retraction then applies). A commit its driver line
/// contradicts also makes the runs incomparable.
fn trust_commit(
    what: &str,
    given: Option<&str>,
    driver_id: &str,
    incomparable: &mut Vec<String>,
) -> std::result::Result<String, String> {
    let Some(sha) = given else {
        return Err(format!("the {what} trust commit was not given"));
    };
    if !full_sha(sha) {
        return Err(format!(
            "the {what} trust commit `{}` is not 40 lowercase hex",
            one_line(sha)
        ));
    }
    let named = driver_hashes(driver_id);
    if named.is_empty() {
        return Err(format!(
            "the {what} run's driver ({}) names no commit, so nothing ties trust {} to it",
            one_line(driver_id),
            short(sha)
        ));
    }
    if let Some(other) = named.iter().find(|h| !sha.starts_with(h.as_str())) {
        let why = format!(
            "the {what} trust commit {} is not the one its run's driver names ({other}, in `{}`)",
            short(sha),
            one_line(driver_id)
        );
        incomparable.push(why.clone());
        return Err(why);
    }
    Ok(sha.to_string())
}

/// A library's per-obligation record, from `plan`'s log directory, held to the
/// package and to its check's Summary (`proved` of `total`).
fn record_of_check(
    plan: &Plan,
    name: &str,
    proved: u64,
    total: u64,
) -> std::result::Result<trust_oblig::Record, String> {
    let spec = plan.specs.get(name).map_or(name, String::as_str);
    let log = format!("{spec}.log");
    let path = plan
        .logdir
        .join(format!("{log}{}", trust_oblig::RECORD_SUFFIX));
    if !path.exists() {
        let err = plan
            .logdir
            .join(format!("{log}{}", trust_oblig::ERROR_SUFFIX));
        let said = fs::read_to_string(&err).unwrap_or_default();
        let said = said.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(if said.is_empty() {
            format!("{log}{} is missing", trust_oblig::RECORD_SUFFIX)
        } else {
            format!("the lane's reduction made none: {}", one_line(said))
        });
    }
    let rec = fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|text| trust_oblig::Record::parse(&text))
        .map_err(|why| format!("{log}{}: {why}", trust_oblig::RECORD_SUFFIX))?;
    if rec.package != name {
        return Err(format!(
            "{log}{} is of package {}, not {name}",
            trust_oblig::RECORD_SUFFIX,
            rec.package
        ));
    }
    if (rec.proved(), rec.total()) != (proved, total) {
        return Err(format!(
            "{log}{} holds {} proved of {} and the check's Summary says {proved} of {total}",
            trust_oblig::RECORD_SUFFIX,
            rec.proved(),
            rec.total()
        ));
    }
    Ok(rec)
}

/// Compare two concluded runs (their plans parsed and measured). `base_prover`
/// is the prover the base must be (the published seal's signed tree_root),
/// when the caller knows it; `ratchet` is where the seal's own floors are read
/// ([`Record`]); `retractions` the ledger and what applying it needs.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn compare(
    base: &Plan,
    cand: &Plan,
    base_prover: Option<&str>,
    ratchet: &Ratchet,
    retractions: &Retractions<'_>,
) -> Comparison {
    let (b, c) = (
        Run {
            plan: base,
            what: "published seal's",
        },
        Run {
            plan: cand,
            what: "candidate's",
        },
    );
    let mut incomparable = b.not_whole();
    incomparable.extend(c.not_whole());
    let (bc, cc) = (
        b.file_field(|p| p.commit.as_str()),
        c.file_field(|p| p.commit.as_str()),
    );
    let (bt, ct) = (
        b.file_field(|p| p.tree.as_str()),
        c.file_field(|p| p.tree.as_str()),
    );
    if bc != cc || bt != ct {
        incomparable.push(format!(
            "the runs measured different trees: the published seal's aterm {} (tree {}), the candidate's {} (tree {})",
            short(&bc),
            short(&bt),
            short(&cc),
            short(&ct)
        ));
    }
    let (bh, ch) = (
        b.file_field(|p| p.host.as_str()),
        c.file_field(|p| p.host.as_str()),
    );
    if base.facts.host != cand.facts.host || bh != ch {
        incomparable.push(format!(
            "the runs were on different machines: the published seal's on {bh} ({} bytes of RAM), the candidate's on {ch} ({} bytes)",
            base.facts.host.mem, cand.facts.host.mem
        ));
    }
    if !base.crate_budget.is_empty()
        && !cand.crate_budget.is_empty()
        && base.crate_budget != cand.crate_budget
    {
        incomparable.push(format!(
            "the runs were bounded differently: the published seal's checks at a per-crate wall budget of {} s, the candidate's at {} s",
            base.crate_budget, cand.crate_budget
        ));
    }
    let (bj, cj) = (
        b.file_field(|p| p.jobs.as_str()),
        c.file_field(|p| p.jobs.as_str()),
    );
    if bj != cj {
        incomparable.push(format!(
            "the runs gated with different slot counts (the published seal's {bj}, the candidate's {cj}), so they loaded the machine differently"
        ));
    }
    let (bp, cp) = (
        b.file_field(|p| p.prover.as_str()),
        c.file_field(|p| p.prover.as_str()),
    );
    if bp == cp && bp != "-" && !bp.is_empty() {
        incomparable.push(format!(
            "the candidate IS the published seal (both runs' prover is {}): there is nothing to compare",
            short(&bp)
        ));
    }
    if let Some(want) = base_prover
        && bp != want
    {
        incomparable.push(if bp == "-" || bp.is_empty() {
            format!(
                "the base run names no prover, so nothing shows it is the published seal ({})",
                short(want)
            )
        } else {
            format!(
                "the base run's prover {} is not the published seal's ({}): the base must run with the published build's own bin/",
                short(&bp),
                short(want)
            )
        });
    }
    if base.facts.vanished != cand.facts.vanished || base.facts.ambiguous != cand.facts.ambiguous {
        incomparable.push(
            "the runs read different graphs (their vanished forks or ambiguous names differ)"
                .into(),
        );
    }
    let header = format!(
        "CANDIDATE GATE: published seal {} (prover {}) vs candidate {} (prover {}) — aterm {} (tree {}), {}",
        id_of(base),
        short(&bp),
        id_of(cand),
        short(&cp),
        short(&bc),
        short(&bt),
        if bh == ch {
            bh.clone()
        } else {
            format!("{bh} / {ch}")
        }
    );
    // THE RETRACTION LEDGER: read and held to its form; each row's commit and
    // reproducer checked in the trust repository, and its commit held apart
    // from the published seal's; the candidate's commit tied to its run.
    let mut notes = Vec::new();
    let libraries: BTreeSet<String> = base
        .facts
        .crates
        .iter()
        .chain(&cand.facts.crates)
        .map(|c| c.name.clone())
        .collect();
    let ledger = match &retractions.ledger {
        Some((text, path)) => parse_ledger(text, path, &libraries),
        None => {
            notes.push("no retraction ledger was given (--retractions): no loss is excused".into());
            trust_oblig::Ledger::default()
        }
    };
    incomparable.extend(ledger.errors.iter().cloned());
    let cand_commit = trust_commit(
        "candidate's",
        retractions.candidate_commit.as_deref(),
        &cand.facts.driver_id,
        &mut incomparable,
    );
    let base_commit = trust_commit(
        "published seal's",
        retractions.base_commit.as_deref(),
        &base.facts.driver_id,
        &mut incomparable,
    );
    let mut git_errors: Vec<String> = Vec::new();
    let cand_resolved: std::result::Result<String, String> = match (&retractions.trust, cand_commit)
    {
        (_, Err(why)) => Err(why),
        (None, Ok(_)) => Err(
            "no trust repository was given (--trust-repo) to read the candidate's trust commit in"
                .into(),
        ),
        (Some(t), Ok(c)) => match t.is_commit(&c) {
            Ok(true) => Ok(c),
            Ok(false) => Err(format!(
                "the candidate's trust commit {} is not a commit of {}",
                short(&c),
                t.shown
            )),
            Err(e) => Err(e),
        },
    };
    let usable: Vec<&Retraction> = match &retractions.trust {
        _ if ledger.rows.is_empty() => Vec::new(),
        None => {
            incomparable.push(format!(
                "{}: {} retraction(s), and no trust repository (--trust-repo) to check their commits and reproducers in",
                ledger.path,
                ledger.rows.len()
            ));
            Vec::new()
        }
        Some(t) => vet_rows(&ledger, t, &base_commit, &mut incomparable),
    };
    // Whether the candidate holds a retraction's fix, asked once per fix.
    let mut asked: BTreeMap<(String, String, String), Contains> = BTreeMap::new();
    let mut contains = |r: &Retraction| -> Contains {
        let key = (r.commit.clone(), r.test_path.clone(), r.test_fn.clone());
        if let Some(a) = asked.get(&key) {
            return a.clone();
        }
        let answer = match (&cand_resolved, &retractions.trust) {
            (Err(why), _) => Contains::Unknown(why.clone()),
            (Ok(_), None) => Contains::Unknown("no trust repository was given".into()),
            (Ok(cc), Some(t)) => match t.holds_fix(r, cc) {
                Ok(c) => c,
                Err(e) => {
                    git_errors.push(e.clone());
                    Contains::Unknown(e)
                }
            },
        };
        asked.insert(key, answer.clone());
        answer
    };
    // Whether a row was audited against the seal the base ran and in the
    // aterm tree the runs measured, the tree asked once per audited commit:
    // its own commit is; another's tree is read where the ledger is.
    let measured: std::result::Result<String, String> = if bc != cc {
        Err("the runs measured different aterm commits".into())
    } else if full_sha(&bc) {
        Ok(bc.clone())
    } else {
        Err(format!(
            "the runs name no aterm commit they measured (`{}`)",
            one_line(&bc)
        ))
    };
    let mut trees: BTreeMap<String, Audited> = BTreeMap::new();
    let mut audited = |r: &Retraction| -> Audited {
        match &base_commit {
            Ok(seal) if r.seal == *seal => {}
            Ok(seal) => {
                return Audited::Changed(format!(
                    "it was audited against the published seal at trust {} and the base ran trust {}, whose record may order its look-alikes otherwise — audit it again against trust {} and re-stamp it",
                    short(&r.seal),
                    short(seal),
                    short(seal)
                ));
            }
            Err(why) => {
                return Audited::Unknown(format!(
                    "whether the base ran the seal it was audited against cannot be told: {why}"
                ));
            }
        }
        let m = match &measured {
            Ok(m) => m,
            Err(why) => {
                return Audited::Unknown(format!(
                    "whether the aterm tree measured is the one it was audited in cannot be told: {why}"
                ));
            }
        };
        if r.aterm == *m {
            return Audited::Same;
        }
        if let Some(a) = trees.get(&r.aterm) {
            return a.clone();
        }
        let answer = match &retractions.aterm {
            None => Audited::Unknown(format!(
                "it was audited at aterm {} and the runs measured aterm {}, and no aterm repository was given to compare their trees in",
                short(&r.aterm),
                short(m)
            )),
            Some(a) => a.audited_tree(&r.aterm, m).unwrap_or_else(Audited::Unknown),
        };
        trees.insert(r.aterm.clone(), answer.clone());
        answer
    };
    let per_obligation = base.obligations && cand.obligations;
    let driver_row = base.facts.driver_row();
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for bcrate in &base.facts.crates {
        if !seen.insert(bcrate.name.as_str()) {
            continue;
        }
        let ccrate = cand.facts.crates.iter().find(|x| x.name == bcrate.name);
        let bside = Side::of(&base.facts.host, bcrate);
        let cside = ccrate.map(|c| Side::of(&cand.facts.host, c));
        let record = match bside {
            Side::Result(_) => None,
            _ => record_of(bcrate, ratchet, &driver_row),
        };
        let oblig = match (&bside, &cside) {
            (_, Some(Side::Result(cs))) if per_obligation => {
                let crec = record_of_check(cand, &bcrate.name, cs.proved, cs.total);
                match &bside {
                    Side::Result(bs) => {
                        match (
                            record_of_check(base, &bcrate.name, bs.proved, bs.total),
                            crec,
                        ) {
                            (Ok(brec), Ok(crec)) => {
                                let mine: Vec<&Retraction> = usable
                                    .iter()
                                    .copied()
                                    .filter(|r| r.library == bcrate.name)
                                    .collect();
                                Some(Oblig::Compared(compare_lib(
                                    &brec,
                                    &crec,
                                    &mine,
                                    &mut audited,
                                    &mut contains,
                                )))
                            }
                            (b, c) => Some(Oblig::Unusable {
                                base: b.err(),
                                cand: c.err(),
                            }),
                        }
                    }
                    // No per-obligation base (the count-vs-record rule): the
                    // candidate's record must still be one it can stand by.
                    _ => crec.err().map(|why| Oblig::Unusable {
                        base: None,
                        cand: Some(why),
                    }),
                }
            }
            _ => None,
        };
        rows.push(decide(
            &bcrate.name,
            Some(&bside),
            cside.as_ref(),
            record.as_ref(),
            oblig.as_ref(),
            per_obligation,
        ));
    }
    for ccrate in &cand.facts.crates {
        if seen.insert(ccrate.name.as_str()) {
            rows.push(decide(
                &ccrate.name,
                None,
                Some(&Side::of(&cand.facts.host, ccrate)),
                None,
                None,
                per_obligation,
            ));
        }
    }
    incomparable.extend(git_errors);
    // A retraction the candidate contradicts makes the ledger wrong for it.
    for r in &rows {
        if let Some(Oblig::Compared(c)) = &r.oblig {
            for (line, why) in &c.stale {
                incomparable.push(format!(
                    "{} line {line} is STALE for this candidate ({}): {why} — correct or remove the row",
                    ledger.path, r.name
                ));
            }
        }
    }
    if !ledger.rows.is_empty() && !per_obligation {
        notes.push(format!(
            "{} retraction(s) in {} applied to nothing: the runs captured no per-obligation records",
            ledger.rows.len(),
            ledger.path
        ));
    }
    let both = rows
        .iter()
        .filter(|r| {
            matches!(
                (&r.base, &r.cand),
                (Some(Side::Result(_)), Some(Side::Result(_)))
            )
        })
        .count();
    if both == 0 {
        incomparable
            .push("no library has a result under both toolchains: nothing was compared".into());
    }
    Comparison {
        header,
        rows,
        incomparable,
        notes,
        ledger: ledger.path,
        aterm: measured.unwrap_or_else(|_| "-".into()),
        seal: base_commit.unwrap_or_else(|_| "-".into()),
    }
}

/// The ledger's rows the trust repository vouches for: each row's commit a
/// commit there holding its reproducer, and not already in the published
/// seal. Why any is not, or why none can be read there at all (a repository
/// that cuts or rewrites ancestry, the seal's commit unknown), goes to
/// `incomparable`.
fn vet_rows<'l>(
    ledger: &'l trust_oblig::Ledger,
    t: &TrustRepo<'_>,
    base_commit: &std::result::Result<String, String>,
    incomparable: &mut Vec<String>,
) -> Vec<&'l Retraction> {
    let all = format!("{}: {} retraction(s)", ledger.path, ledger.rows.len());
    // Ancestry read in a repository that cuts or rewrites it answers wrongly
    // both ways: refused, and no row is used.
    if let Some(why) = t.unfit() {
        incomparable.push(format!("{all}, and {why}"));
        return Vec::new();
    }
    // A row whose fix the seal already holds retracts nothing of the seal's:
    // with the seal's commit unknown that cannot be told, so no row is used.
    let seal = match base_commit {
        Ok(bc) => match t.is_commit(bc) {
            Ok(true) => bc.clone(),
            Ok(false) => {
                incomparable.push(format!(
                    "{all}, and whether a row's commit is already in the published seal cannot be checked: the published seal's trust commit {} is not a commit of {}",
                    short(bc),
                    t.shown
                ));
                return Vec::new();
            }
            Err(e) => {
                incomparable.push(format!("{all}, and {e}"));
                return Vec::new();
            }
        },
        Err(why) => {
            incomparable.push(format!(
                "{all}, and whether a row's commit is already in the published seal cannot be checked: {why}"
            ));
            return Vec::new();
        }
    };
    let mut usable = Vec::new();
    for r in &ledger.rows {
        let checked = t.is_commit(&r.commit).and_then(|known| {
            if !known {
                return Ok(Some(format!(
                    "trust {} is not a commit of {}",
                    short(&r.commit),
                    t.shown
                )));
            }
            if let Some(missing) = t.reproducer_missing(r, &r.commit)? {
                return Ok(Some(format!("its reproducer: {missing}")));
            }
            Ok(t.is_ancestor(&r.commit, &seal)?.then(|| {
                format!(
                    "trust {} is already in the published seal ({}), so the seal's proof is not one it retracts",
                    short(&r.commit),
                    short(&seal)
                )
            }))
        });
        match checked {
            Ok(None) => usable.push(r),
            Ok(Some(why)) | Err(why) => {
                incomparable.push(format!("{} line {}: {why}", ledger.path, r.line));
            }
        }
    }
    usable
}

fn short(s: &str) -> &str {
    s.get(..12).unwrap_or(s)
}

fn id_of(p: &Plan) -> String {
    if p.facts.driver_id.is_empty() {
        "(no --version line)".into()
    } else {
        p.facts.driver_id.clone()
    }
}

/// What a comparison prints and answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub report: String,
    pub tsv: String,
    /// Every per-obligation finding, one row each (`--obligations-tsv`): a
    /// lost row's library-to-seal columns are a retraction's first ten.
    pub obligations: String,
    pub exit: i32,
}

/// `name a, name b` for the libraries of one outcome.
fn names(rows: &[&Row]) -> String {
    rows.iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The obligations TSV's header. A LOST row's `library` to `seal` are a
/// retraction's first ten columns (`site` where the seal reported it, `aterm`
/// the commit both runs measured, `seal` the published seal's trust commit).
pub const OBLIGATIONS_TSV_HEADER: &str = "# finding\tlibrary\tfunction\tkind\tfile\tanchor\tnth\tdescription\tsite\taterm\tseal\tcandidate\tnote\n";

/// Render a comparison: the table, one line per finding, and the verdict.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render(cmp: &Comparison) -> Rendered {
    let mut out = String::new();
    out.push_str(&cmp.header);
    out.push('\n');
    out.push_str(&format!(
        "{:<28} {:<26} {:<26} {:>6}  {}\n",
        "LIBRARY", "PUBLISHED SEAL", "CANDIDATE", "DELTA", "OUTCOME"
    ));
    let mut tsv = String::from(
        "# library\tbase\tbase_proved\tbase_total\tcandidate\tcand_proved\tcand_total\tdelta\tlost\toutcome\twhy\n",
    );
    let mut obl = String::from(OBLIGATIONS_TSV_HEADER);
    let dash = || "-".to_string();
    for r in &cmp.rows {
        let st = |s: &Option<Side>| s.as_ref().map_or_else(|| "(absent)".into(), Side::status);
        let delta = r.delta().map_or_else(dash, signed);
        out.push_str(&format!(
            "{:<28} {:<26} {:<26} {:>6}  {}\n",
            r.name,
            st(&r.base),
            st(&r.cand),
            delta,
            r.outcome.word()
        ));
        let counts = |s: &Option<Side>| match s {
            Some(Side::Result(x)) => (x.proved.to_string(), x.total.to_string()),
            _ => (dash(), dash()),
        };
        let word = |s: &Option<Side>| s.as_ref().map_or_else(|| "ABSENT".into(), Side::word);
        let ((bp, bt), (cp, ct)) = (counts(&r.base), counts(&r.cand));
        let lost = r.lost().map_or_else(dash, |n| n.to_string());
        tsv.push_str(&format!(
            "{}\t{}\t{bp}\t{bt}\t{}\t{cp}\t{ct}\t{delta}\t{lost}\t{}\t{}\n",
            one_line(&r.name),
            word(&r.base),
            word(&r.cand),
            r.outcome.word(),
            r.why
        ));
        if let Some(Oblig::Compared(c)) = &r.oblig {
            for l in &c.lost {
                obl.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    if l.missing { "MISSING" } else { "LOST" },
                    one_line(&r.name),
                    l.key.columns(),
                    l.site,
                    cmp.aterm,
                    cmp.seal,
                    l.cand,
                    l.note
                ));
            }
            for e in &c.excused {
                obl.push_str(&format!(
                    "EXCUSED\t{}\t{}\t{}\t{}\t{}\t{}\t{} line {}: trust {} ({}; {})\n",
                    one_line(&r.name),
                    e.key.columns(),
                    e.site,
                    cmp.aterm,
                    cmp.seal,
                    e.cand,
                    cmp.ledger,
                    e.line,
                    e.commit,
                    e.reproducer,
                    e.reviewed
                ));
            }
        }
    }
    let of = |o: Outcome| {
        cmp.rows
            .iter()
            .filter(|r| r.outcome == o)
            .collect::<Vec<_>>()
    };
    let mut losses = of(Outcome::Loss);
    losses.sort_by_key(|r| {
        (
            r.lost()
                .map_or_else(|| r.delta().unwrap_or(0), |n| -(n as i128)),
            r.name.clone(),
        )
    });
    let (noresult, nobase, undecided, nothost, differ, floor, ok, skipped) = (
        of(Outcome::NoResult),
        of(Outcome::NoBaseline),
        of(Outcome::BaseUndecided),
        of(Outcome::NotOnHost),
        of(Outcome::InputsDiffer),
        of(Outcome::Floor),
        of(Outcome::Ok),
        of(Outcome::Skipped),
    );
    // Both sides scored: the libraries a total can be said over.
    let (mut n, mut bsum, mut csum) = (0usize, 0u64, 0u64);
    for r in &cmp.rows {
        if let (Some(Side::Result(b)), Some(Side::Result(c))) = (&r.base, &r.cand) {
            n += 1;
            bsum += b.proved;
            csum += c.proved;
        }
    }
    out.push_str(&format!(
        "COMPARED: {n} libraries with a result under both: {bsum} proved under the published seal, {csum} under the candidate; {} OK, {} lose, {} no result, {} held to the seal's record, {} no baseline, {} base undecided, {} not on this host, {} skipped by policy, {} inputs differ\n",
        ok.len(),
        losses.len(),
        noresult.len(),
        floor.len(),
        nobase.len(),
        undecided.len(),
        nothost.len(),
        skipped.len(),
        differ.len()
    ));
    // Per obligation, over the libraries whose records were compared.
    let compared: Vec<&LibCmp> = cmp
        .rows
        .iter()
        .filter_map(|r| match &r.oblig {
            Some(Oblig::Compared(c)) => Some(c),
            _ => None,
        })
        .collect();
    let excused: usize = compared.iter().map(|c| c.excused.len()).sum();
    if !compared.is_empty() {
        out.push_str(&format!(
            "OBLIGATIONS: {} libraries compared obligation by obligation: {} lost (proved by the published seal, not by the candidate; {} of them MISSING), {excused} excused by the retraction ledger, {} gained (offsetting nothing)\n",
            compared.len(),
            compared.iter().map(|c| c.lost.len()).sum::<usize>(),
            compared.iter().map(|c| c.missing()).sum::<usize>(),
            compared.iter().map(|c| c.gained).sum::<u64>()
        ));
    }
    let loss_list = losses
        .iter()
        .map(|r| match (r.against(), &r.cand, r.lost()) {
            (Some(b), Some(Side::Result(c)), Some(lost)) => {
                format!("{} {lost} lost ({} -> {})", r.name, b, c.proved)
            }
            (Some(b), Some(Side::Result(c)), None) => format!(
                "{} {} ({} -> {})",
                r.name,
                r.delta().unwrap_or(0),
                b,
                c.proved
            ),
            _ => r.name.clone(),
        })
        .collect::<Vec<_>>();
    if !losses.is_empty() {
        out.push_str(&format!("LOSS: {}\n", loss_list.join(", ")));
    }
    // Every finding, one line each: what refuses, what makes the runs
    // incomparable, and what was let through on the seal's record — a loss
    // with each obligation lost, one `LOST` line each.
    for (label, rows) in [
        ("REFUSED", &losses),
        ("REFUSED", &noresult),
        ("REFUSED", &nobase),
        ("REFUSED", &undecided),
        ("REFUSED", &nothost),
        ("COULD NOT COMPARE", &differ),
        ("NOTE", &floor),
    ] {
        for r in rows {
            out.push_str(&format!(
                "{label} {} ({}): {}\n",
                r.name,
                r.outcome.word(),
                r.why
            ));
            if let Some(Oblig::Compared(c)) = &r.oblig {
                for l in &c.lost {
                    out.push_str(&format!(
                        "{} {} {} | at {} | candidate: {}{}\n",
                        if l.missing { "MISSING" } else { "LOST" },
                        r.name,
                        l.key.said(),
                        l.site,
                        l.cand,
                        if l.note.is_empty() {
                            String::new()
                        } else {
                            format!(" | {}", l.note)
                        }
                    ));
                }
            }
        }
    }
    // What the ledger excused, and the retractions that excused nothing.
    for r in &cmp.rows {
        if let Some(Oblig::Compared(c)) = &r.oblig {
            for e in &c.excused {
                out.push_str(&format!(
                    "EXCUSED {} {} | at {} | candidate: {} | retracted by trust {} ({} line {}; reproducer {}; reviewed {})\n",
                    r.name,
                    e.key.said(),
                    e.site,
                    e.cand,
                    short(&e.commit),
                    cmp.ledger,
                    e.line,
                    e.reproducer,
                    e.reviewed
                ));
            }
            for (line, word, why) in &c.notes {
                out.push_str(&format!(
                    "NOTE {} line {line} ({word}, {}): {why}\n",
                    cmp.ledger, r.name
                ));
            }
        }
    }
    for note in &cmp.notes {
        out.push_str(&format!("NOTE: {note}\n"));
    }
    for why in &cmp.incomparable {
        out.push_str(&format!("COULD NOT COMPARE: {why}\n"));
    }
    let mut refusals = Vec::new();
    if !losses.is_empty() {
        refusals.push(format!(
            "{} lose proofs ({})",
            losses.len(),
            losses
                .iter()
                .map(|r| r.loss_said())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (rows, said) in [
        (&noresult, "gave no result"),
        (&nobase, "with nothing on record of the seal"),
        (&undecided, "base undecided"),
        (&nothost, "not verified on this host"),
    ] {
        if !rows.is_empty() {
            refusals.push(format!("{} {said} ({})", rows.len(), names(rows)));
        }
    }
    // Every refusing row is named in `refusals` above, by its outcome.
    let refusing = cmp.rows.iter().any(|r| r.outcome.refuses());
    let (verdict, exit) = if !cmp.incomparable.is_empty() || !differ.is_empty() {
        let mut why = cmp.incomparable.clone();
        if !differ.is_empty() {
            why.push(format!(
                "{} libraries were not measured alike ({})",
                differ.len(),
                names(&differ)
            ));
        }
        let also = if refusals.is_empty() {
            String::new()
        } else {
            format!("; and as measured, {}", refusals.join(", "))
        };
        (
            format!(
                "VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: {}{also}; nothing is published",
                why.join("; ")
            ),
            2,
        )
    } else if refusing {
        (
            format!(
                "VERDICT: PUBLISH-REFUSED — {}; nothing is published",
                refusals.join(", ")
            ),
            1,
        )
    } else {
        (
            format!(
                "VERDICT: PUBLISH-OK — the candidate proves every obligation the published seal proves on this host ({} compared, {excused} excused by the retraction ledger, {} held to the seal's own record where its check gave no result, {} skipped by policy)",
                ok.len(),
                floor.len(),
                skipped.len()
            ),
            0,
        )
    };
    out.push_str(&verdict);
    out.push('\n');
    Rendered {
        report: out,
        tsv,
        obligations: obl,
        exit,
    }
}

/// `trust compare`'s inputs ([`crate::trust_facts::TrustCmd::Compare`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub base: PathBuf,
    pub candidate: PathBuf,
    /// Where each run's logs are; default: the directory holding its plan (the
    /// lane writes `<logdir>/.plan`).
    pub base_logdir: Option<PathBuf>,
    pub candidate_logdir: Option<PathBuf>,
    pub tsv: Option<PathBuf>,
    /// The prover the base run must have measured with.
    pub base_prover: Option<String>,
    /// The ratchet the seal's own floors are read from ([`Record`]); default:
    /// the one the base run's plan names.
    pub ratchet: Option<PathBuf>,
    /// The retraction ledger (`tools/trust-gate-retractions.tsv`); none, no
    /// loss is excused. A row audited at another aterm commit than the runs
    /// measured is compared with it in the git repository the ledger is in.
    pub retractions: Option<PathBuf>,
    /// The trust repository the ledger's commits and reproducers are read in.
    pub trust_repo: Option<PathBuf>,
    /// The candidate's trust commit (its trustc's `commit-hash`), held to its
    /// run's driver line.
    pub candidate_trust_commit: Option<String>,
    /// The published seal's.
    pub base_trust_commit: Option<String>,
    /// Where to write every per-obligation finding (LOST, MISSING, EXCUSED).
    pub obligations_tsv: Option<PathBuf>,
}

/// Load one run: its plan, read from its log directory.
fn load(what: &str, plan: &Path, logdir: Option<&Path>) -> std::result::Result<Plan, String> {
    let text =
        fs::read_to_string(plan).map_err(|e| format!("the {what} plan {}: {e}", plan.display()))?;
    let mut p =
        Plan::parse(&text).map_err(|why| format!("the {what} plan {}: {why}", plan.display()))?;
    p.logdir = match logdir {
        Some(d) => d.to_path_buf(),
        None => plan.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    p.measure();
    Ok(p)
}

/// Read the ratchet the seal's floors come from: `--ratchet`, else the base
/// plan's. One that cannot be read records nothing (every library the seal's
/// own check left without a result is then held to its partial count, or is
/// NO-BASELINE).
fn ratchet_for(given: Option<&Path>, base: &Plan) -> Ratchet {
    let path = given.map_or_else(|| base.ratchet.clone(), Path::to_path_buf);
    let shown = path.display().to_string();
    Ratchet {
        text: (!shown.is_empty())
            .then(|| fs::read_to_string(&path).ok())
            .flatten(),
        path: shown,
    }
}

/// Run `trust compare`: print the report, write the TSV, answer 0, 1 or 2.
pub fn run(args: &Args) -> i32 {
    let base = load("published seal's", &args.base, args.base_logdir.as_deref());
    let cand = load(
        "candidate's",
        &args.candidate,
        args.candidate_logdir.as_deref(),
    );
    let (base, cand) = match (base, cand) {
        (Ok(b), Ok(c)) => (b, c),
        (b, c) => {
            let why = [b.err(), c.err()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            println!("COULD NOT COMPARE: {why}");
            println!("VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: {why}; nothing is published");
            return 2;
        }
    };
    let ratchet = ratchet_for(args.ratchet.as_deref(), &base);
    // A ledger named and unreadable excuses nothing and compares nothing.
    let ledger = match &args.retractions {
        None => None,
        Some(p) => match fs::read_to_string(p) {
            Ok(text) => Some((text, p.display().to_string())),
            Err(e) => {
                let why = format!("the retraction ledger {}: {e}", p.display());
                println!("COULD NOT COMPARE: {why}");
                println!(
                    "VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: {why}; nothing is published"
                );
                return 2;
            }
        },
    };
    let git = args.trust_repo.as_ref().map(TrustGit::new);
    // A row audited at another aterm commit is read in the repository the
    // ledger is committed in: the directory holding it.
    let aterm_dir = args.retractions.as_ref().map(|p| match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    });
    let aterm_git = aterm_dir.as_ref().map(TrustGit::new);
    let retractions = Retractions {
        ledger,
        trust: git
            .as_ref()
            .zip(args.trust_repo.as_ref())
            .map(|(g, p)| TrustRepo {
                git: g as &dyn GitRunner,
                shown: p.display().to_string(),
            }),
        aterm: aterm_git
            .as_ref()
            .zip(aterm_dir.as_ref())
            .map(|(g, d)| AtermRepo {
                git: g as &dyn GitRunner,
                shown: format!("the repository holding the ledger ({})", d.display()),
            }),
        candidate_commit: args.candidate_trust_commit.clone(),
        base_commit: args.base_trust_commit.clone(),
    };
    let rendered = render(&compare(
        &base,
        &cand,
        args.base_prover.as_deref(),
        &ratchet,
        &retractions,
    ));
    print!("{}", rendered.report);
    for (path, text) in [
        (&args.tsv, &rendered.tsv),
        (&args.obligations_tsv, &rendered.obligations),
    ] {
        if let Some(path) = path
            && let Err(e) = fs::write(path, text)
        {
            eprintln!("aterm-release trust compare: {}: {e}", path.display());
            return 2;
        }
    }
    rendered.exit
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEAL: &str = "a519d423f47ff6fb19d505012f210f13ad39f2f6f31cb3c3af5aff508551d0b2";
    const MAIN: &str = "61a997f0b92df5c78bb1ab075ca3044ceb871789c82c66db2743c03f9f537db1";
    const COMMIT: &str = "36b2c3df36eac4971b8189da9967ab7cc442bbf2";
    const TREE: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A whole-tree run's plan, as the lane writes it: `crates` are
    /// `(name, decision word)`, each spec `<name>@1.0.0`; `edit` rewrites it.
    fn plan(prover: &str, crates: &[(&str, &str)], edit: impl Fn(String) -> String) -> String {
        let mut t = String::from("aterm-trust plan 1\n");
        t.push_str("driver /t/bin/targo\ndriver-id targo 1.99.0-dev (stub)\nonly \nper-commit 0\n");
        t.push_str("logdir /gone\nhost-mem 25769803776\nhost-mem-os -\nnow 2026-10-03T00:00:00Z\n");
        t.push_str("crate-budget 21600\nobligations 1\n");
        for (n, w) in crates {
            let word = if *w == "memory" { "gate" } else { *w };
            t.push_str(&format!("crate {n}\t{n}@1.0.0\t{word}\n"));
        }
        for (n, w) in crates {
            if *w == "memory" {
                t.push_str(&format!("refused {n}\t-\t-\t-\t-\n"));
            }
        }
        t.push_str("trustd /tmp/none.sock\nfile \nstore /s\n");
        t.push_str(&format!("commit {COMMIT}\ntree {TREE}\nprover {prover}\n"));
        t.push_str("host 14 cores, 24 GiB\njobs 1\n");
        edit(t)
    }

    const DONE: &str = "Coverage: 10/10 eligible function bodies verified (complete)\n";

    fn summary(p: u64, t: u64) -> String {
        format!(
            "Summary: {p} proved, 0 failed, 0 runtime-checked, 0 assumed, 0 mandated, 0 contract-panic, 0 inconclusive ({t} total); not-reproducible\n{DONE}"
        )
    }

    struct RunDir {
        dir: aterm_tempfile::TempDir,
    }

    impl RunDir {
        fn new(plan_text: &str) -> RunDir {
            let dir = aterm_tempfile::Builder::new()
                .prefix("trust-compare")
                .tempdir()
                .unwrap();
            fs::write(dir.path().join(".plan"), plan_text).unwrap();
            RunDir { dir }
        }
        /// `<name>@1.0.0.log` and its `.rc` — and, when its Summary counts,
        /// the per-obligation record the lane would have left: its `total`
        /// obligations `<name>::f` row 1, row 2, …, the first `proved` proved.
        fn log(&self, name: &str, text: &str, rc: &str) -> &RunDir {
            let log = self.dir.path().join(format!("{name}@1.0.0.log"));
            fs::write(&log, text).unwrap();
            fs::write(self.dir.path().join(format!("{name}@1.0.0.log.rc")), rc).unwrap();
            let record = log.with_extension("log.oblig.tsv");
            let _ = fs::remove_file(&record);
            let last = text
                .lines()
                .rfind(|l| l.contains("Summary: "))
                .unwrap_or("");
            if let (Some(p), Some(t)) = parse_summary(last) {
                fs::write(&record, rows_record(name, p, t, 0)).unwrap();
            }
            self
        }
        /// Replace `<name>`'s record with `text`.
        fn record(&self, name: &str, text: &str) -> &RunDir {
            fs::write(
                self.dir.path().join(format!("{name}@1.0.0.log.oblig.tsv")),
                text,
            )
            .unwrap();
            self
        }
        fn mark(&self, name: &str, suffix: &str, text: &str) -> &RunDir {
            fs::write(
                self.dir.path().join(format!("{name}@1.0.0.log.{suffix}")),
                text,
            )
            .unwrap();
            self
        }
        fn plan(&self) -> PathBuf {
            self.dir.path().join(".plan")
        }
    }

    /// A record of `total` obligations of `<name>::f`, row `i` at line `i`:
    /// rows `skip + 1 ..= skip + proved` proved, the rest failed.
    fn rows_record(name: &str, proved: u64, total: u64, skip: u64) -> String {
        let rows = (1..=total)
            .map(|i| trust_oblig::Obligation {
                function: format!("{name}::f"),
                kind: "assertion".into(),
                file: "src/lib.rs".into(),
                span: trust_oblig::Span {
                    l0: u32::try_from(i).unwrap(),
                    c0: 0,
                    l1: u32::try_from(i).unwrap(),
                    c1: 1,
                },
                status: if i > skip && i <= skip + proved {
                    "proved".into()
                } else {
                    "failed".into()
                },
                anchor: format!("row {i}"),
                desc: "assertion".into(),
            })
            .collect();
        trust_oblig::Record {
            package: name.into(),
            unit: format!("cargo-target(package=\"{name}\")"),
            functions: 1,
            rows,
        }
        .to_text()
    }

    fn answer(base: &RunDir, cand: &RunDir) -> Rendered {
        answer_r(base, cand, None)
    }

    /// The seal's prover as the plans above name it, as a ratchet row names it.
    const DRIVER: &str = "targo 1.99.0-dev (stub)";

    /// `answer` with a ratchet's text (the seal's own floors, [`Record`]).
    fn answer_r(base: &RunDir, cand: &RunDir, ratchet: Option<&str>) -> Rendered {
        let b = load("published seal's", &base.plan(), None).unwrap();
        let c = load("candidate's", &cand.plan(), None).unwrap();
        let ratchet = Ratchet {
            text: ratchet.map(str::to_string),
            path: "tools/trust-gate-ratchet.tsv".into(),
        };
        render(&compare(&b, &c, None, &ratchet, &Retractions::default()))
    }

    fn row<'a>(r: &'a Rendered, name: &str) -> &'a str {
        r.report
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("no row {name}:\n{}", r.report))
    }

    fn verdict(r: &Rendered) -> &str {
        r.report.lines().last().unwrap()
    }

    /// Two runs of `alpha` and `beta`, the base proving 5 and 7.
    fn pair(alpha: (&str, &str), beta: (&str, &str)) -> (RunDir, RunDir) {
        let crates = [("alpha", "gate"), ("beta", "gate"), ("tool", "no-lib")];
        let b = RunDir::new(&plan(SEAL, &crates, |t| t));
        b.log("alpha", &summary(5, 10), "0");
        b.log("beta", &summary(7, 9), "0");
        let c = RunDir::new(&plan(MAIN, &crates, |t| t));
        c.log("alpha", alpha.0, alpha.1);
        c.log("beta", beta.0, beta.1);
        (b, c)
    }

    #[test]
    fn a_candidate_that_proves_as_much_passes() {
        let (b, c) = pair((&summary(5, 11), "0"), (&summary(9, 12), "0"));
        let r = answer(&b, &c);
        assert_eq!(r.exit, 0, "{}", r.report);
        assert!(row(&r, "alpha").ends_with(" 0  OK"), "{}", r.report);
        assert!(row(&r, "beta").contains("+2  OK"), "{}", r.report);
        assert!(row(&r, "tool").ends_with("SKIPPED"), "{}", r.report);
        assert!(
            verdict(&r).starts_with("VERDICT: PUBLISH-OK — "),
            "{}",
            r.report
        );
        assert!(
            r.report.contains(
                "COMPARED: 2 libraries with a result under both: 12 proved under the published seal, 14 under the candidate"
            ),
            "{}",
            r.report
        );
        assert!(
            r.tsv
                .lines()
                .any(|l| l == "beta\tRESULT\t7\t9\tRESULT\t9\t12\t+2\t0\tOK\tevery obligation the published seal proved is proved by the candidate; 9 proved under the candidate, 7 under the published seal; 2 obligations gained, which offset nothing"),
            "{}",
            r.tsv
        );
    }

    #[test]
    fn one_proof_fewer_refuses_and_names_the_library() {
        let (b, c) = pair((&summary(4, 20), "0"), (&summary(7, 9), "0"));
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(row(&r, "alpha").contains("5/10") && row(&r, "alpha").ends_with("-1  LOSS"));
        assert!(
            r.report.contains("\nLOSS: alpha 1 lost (5 -> 4)\n"),
            "{}",
            r.report
        );
        // The obligation lost, by name: row 5, which the seal proved.
        assert!(
            r.report.contains("\nLOST alpha alpha::f | assertion | src/lib.rs | `row 5` #1/1 | assertion | at src/lib.rs:5:0-5:1 | candidate: failed\n"),
            "{}",
            r.report
        );
        assert_eq!(
            verdict(&r),
            "VERDICT: PUBLISH-REFUSED — 1 lose proofs (alpha 1 lost); nothing is published"
        );
    }

    #[test]
    fn a_shortfall_the_clock_could_cover_is_refused_and_said() {
        let lapsed = format!("[UNKNOWN] f: timed_out=3\n{}", summary(4, 10));
        let (b, c) = pair((&lapsed, "0"), (&summary(7, 9), "0"));
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains("REFUSED alpha (LOSS): 1 obligation the published seal proved is not proved by the candidate, in 1 function; 4 proved under the candidate against 5 under the published seal (-1); within what the candidate's run let lapse or trustd refused (3 lapsed, 0 refused), so it may be the machine's"),
            "{}",
            r.report
        );
    }

    #[test]
    fn every_way_a_candidate_gives_no_result_refuses() {
        let cut = format!(
            "  process didn't exit successfully: `/t/bin/trustc --crate-name alpha` (signal: 15, SIGTERM: termination signal)\n  [UNKNOWN] [transport:missing-crate-summary] compiler emitted function rows without an end-of-compile target inventory (targo-trust)\n{}Coverage: unknown (compiler emitted no coverage_summary transport row; older toolchain?)\n",
            summary(3, 10).lines().next().unwrap().to_owned() + "\n"
        );
        /// What breaks the candidate's check of `alpha`.
        type Break<'a> = Box<dyn Fn(&RunDir) + 'a>;
        let cases: Vec<(&str, Break<'_>, &str)> = vec![
            (
                "incomplete",
                Box::new(|c: &RunDir| {
                    c.log("alpha", &cut, "101");
                }),
                "NOT-VERIFIED(incomplete)",
            ),
            (
                "over-budget",
                Box::new(|c: &RunDir| {
                    c.log("alpha", "Compiling alpha\n", "143")
                        .mark("alpha", "budget", "600\n");
                }),
                "OVER-BUDGET(600s)",
            ),
            (
                "trustd",
                Box::new(|c: &RunDir| {
                    c.log(
                        "alpha",
                        "error: could not establish the selected same-sysroot trustd memory authority\n",
                        "1",
                    );
                }),
                "NOT-VERIFIED(trustd)",
            ),
            (
                "not run",
                Box::new(|c: &RunDir| {
                    c.log("alpha", "verification did not run\n", "2");
                }),
                "NOT-VERIFIED(rc=2)",
            ),
            (
                "no log",
                Box::new(|c: &RunDir| {
                    fs::remove_file(c.dir.path().join("alpha@1.0.0.log")).unwrap();
                    fs::remove_file(c.dir.path().join("alpha@1.0.0.log.rc")).unwrap();
                }),
                "GATE-BROKE(rc=?)",
            ),
            (
                "unparsed",
                Box::new(|c: &RunDir| {
                    c.log("alpha", &format!("Summary: many proved\n{DONE}"), "0");
                }),
                "SUMMARY-UNPARSED",
            ),
        ];
        for (what, make, status) in cases {
            let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
            make(&c);
            let r = answer(&b, &c);
            assert_eq!(r.exit, 1, "{what}: {}", r.report);
            let alpha = row(&r, "alpha");
            assert!(
                alpha.contains(status) && alpha.ends_with("NO-RESULT"),
                "{what}: {alpha}"
            );
            assert_eq!(
                verdict(&r),
                "VERDICT: PUBLISH-REFUSED — 1 gave no result (alpha); nothing is published",
                "{what}"
            );
        }
    }

    #[test]
    fn a_base_without_a_result_is_held_to_what_the_seal_is_on_record_proving() {
        // The seal's own failures (each with no count of its own): with nothing on
        // record of what the seal proves there, the candidate is REFUSED whatever it
        // proved (NO-BASELINE); with the floor the seal's own prover recorded, it is
        // held to that floor — a LOSS below it, and at or above it still REFUSED
        // under the per-obligation comparison (a count says nothing of WHICH
        // obligations the seal proves: the 2026-10-07 refutation); OK-FLOOR is
        // what runs that captured no records would have said, which compare
        // nothing.
        let seals_own = [
            (
                "  [UNKNOWN] [transport:missing-crate-summary] no inventory (targo-trust)\nCoverage: unknown (compiler emitted no coverage_summary transport row)\n".to_string(),
                "101",
                false,
            ),
            ("Compiling\n".to_string(), "143", true),
            ("verification did not run\n".to_string(), "2", false),
            ("error[E0308]: mismatched types\n".to_string(), "101", false),
            ("".to_string(), "1", false),
            ("".to_string(), "0", false),
            (format!("Summary: 1,000 proved (2,000 total)\n{DONE}"), "0", false),
        ];
        for (log, rc, budget) in &seals_own {
            let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
            b.log("alpha", log, rc);
            if *budget {
                b.mark("alpha", "budget", "21600\n");
            }
            let r = answer(&b, &c);
            assert_eq!(r.exit, 1, "{log}: {}", r.report);
            assert!(row(&r, "alpha").ends_with("NO-BASELINE"), "{}", r.report);
            assert!(
                r.report
                    .contains("REFUSED alpha (NO-BASELINE): the published seal's own check")
                    && r.report.contains("record the seal's floor first"),
                "{}",
                r.report
            );
            assert_eq!(
                verdict(&r),
                "VERDICT: PUBLISH-REFUSED — 1 with nothing on record of the seal (alpha); nothing is published",
                "{log}"
            );
            // a floor ANOTHER prover recorded is not the seal's record
            let other = "alpha\t4\t10\ttargo 1.99.0-dev (another) (targo 0.1.0)\n";
            let r = answer_r(&b, &c, Some(other));
            assert!(row(&r, "alpha").ends_with("NO-BASELINE"), "{}", r.report);
            // the seal's own floor, met: a count is no record of which obligations
            // the seal proves, so the candidate is refused all the same
            let met = format!("alpha\t5\t10\t{DRIVER}\n");
            let r = answer_r(&b, &c, Some(&met));
            assert_eq!(r.exit, 1, "{log}: {}", r.report);
            assert!(
                row(&r, "alpha").ends_with(" 0  NO-BASELINE"),
                "{}",
                r.report
            );
            assert!(
                r.report.contains("REFUSED alpha (NO-BASELINE): the published seal's own check")
                    && r.report.contains("it is on record proving 5 by count (its floor of 5 in tools/trust-gate-ratchet.tsv, recorded by the seal's own prover (targo 1.99.0-dev (stub))), and the candidate proved 5 of 10, but no record says WHICH obligations the seal proves here"),
                "{}",
                r.report
            );
            assert_eq!(
                verdict(&r),
                "VERDICT: PUBLISH-REFUSED — 1 with nothing on record of the seal (alpha); nothing is published",
                "{log}"
            );
            // …where runs captured no records the count would have held it
            // (OK-FLOOR), and those runs compare nothing.
            for d in [&b, &c] {
                let t = fs::read_to_string(d.plan()).unwrap();
                fs::write(d.plan(), t.replace("obligations 1\n", "")).unwrap();
            }
            let r = answer_r(&b, &c, Some(&met));
            assert_eq!(r.exit, 2, "{log}: {}", r.report);
            assert!(row(&r, "alpha").ends_with(" 0  OK-FLOOR"), "{}", r.report);
            for d in [&b, &c] {
                let t = fs::read_to_string(d.plan()).unwrap();
                fs::write(
                    d.plan(),
                    t.replace("crate-budget", "obligations 1\ncrate-budget"),
                )
                .unwrap();
            }
            // …and missed by one: a LOSS against the record
            let missed = format!("alpha\t6\t10\t{DRIVER}\n");
            let r = answer_r(&b, &c, Some(&missed));
            assert_eq!(r.exit, 1, "{log}: {}", r.report);
            assert!(row(&r, "alpha").ends_with("-1  LOSS"), "{}", r.report);
            assert!(
                r.report.contains("\nLOSS: alpha -1 (6 -> 5)\n"),
                "{}",
                r.report
            );
        }
        // An unfinished seal check's own count is a record too, and the higher of it
        // and the floor is what the candidate must reach: the seal's check ran past
        // its budget at 9 proved (a floor of 4 recorded), the candidate proves 5.
        let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
        b.log("alpha", "Summary: 9 proved, 0 failed (10 total)\n", "143")
            .mark("alpha", "budget", "21600\n");
        let r = answer_r(&b, &c, Some(&format!("alpha\t4\t10\t{DRIVER}\n")));
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            row(&r, "alpha").contains("OVER-BUDGET(21600s)")
                && row(&r, "alpha").ends_with("-4  LOSS"),
            "{}",
            r.report
        );
        assert!(
            r.report.contains("against the 9 the published seal is on record proving (the 9 its own unfinished check here counted before it stopped; its floor of 4"),
            "{}",
            r.report
        );
        // A FINISHED seal check (100 of 200, complete Coverage) whose log also says
        // `verification did not run` somewhere reads as not run, the judge's order —
        // and its 100 still holds the candidate: 10 is a loss, not a free pass.
        let (b, c) = pair((&summary(10, 200), "0"), (&summary(7, 9), "0"));
        b.log(
            "alpha",
            &format!(
                "note: a helper: verification did not run\n{}",
                summary(100, 200)
            ),
            "0",
        );
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(row(&r, "alpha").ends_with("-90  LOSS"), "{}", r.report);
        // the machine's: refused until measured again
        for (log, rc) in [
            ("Compiling\n", "143"),
            (
                "  process didn't exit successfully: `/s/bin/trustc --crate-name alpha` (signal: 9, SIGKILL: kill)\n",
                "101",
            ),
            (
                "could not establish the selected same-sysroot trustd memory authority\n",
                "1",
            ),
        ] {
            let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
            b.log("alpha", log, rc);
            let r = answer(&b, &c);
            assert_eq!(r.exit, 1, "{log}: {}", r.report);
            assert!(row(&r, "alpha").ends_with("BASE-UNDECIDED"), "{}", r.report);
            assert!(
                verdict(&r).contains("1 base undecided (alpha)"),
                "{}",
                r.report
            );
        }
        // a base that left no exit status at all
        let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
        fs::remove_file(b.dir.path().join("alpha@1.0.0.log")).unwrap();
        fs::remove_file(b.dir.path().join("alpha@1.0.0.log.rc")).unwrap();
        let r = answer(&b, &c);
        assert!(row(&r, "alpha").ends_with("BASE-UNDECIDED"), "{}", r.report);
        // and both without a result is the candidate's NO-RESULT
        let (b, c) = pair(("verification did not run\n", "2"), (&summary(7, 9), "0"));
        b.log("alpha", "verification did not run\n", "2");
        let r = answer(&b, &c);
        assert!(row(&r, "alpha").ends_with("NO-RESULT"), "{}", r.report);
    }

    #[test]
    fn a_library_refused_for_memory_on_either_side_refuses() {
        for (bw, cw, under) in [
            ("memory", "memory", "under both toolchains"),
            ("memory", "gate", "under the published seal"),
            ("gate", "memory", "under the candidate"),
        ] {
            // beta, compared, keeps the runs comparable around alpha
            let b = RunDir::new(&plan(SEAL, &[("alpha", bw), ("beta", "gate")], |t| t));
            let c = RunDir::new(&plan(MAIN, &[("alpha", cw), ("beta", "gate")], |t| t));
            for d in [&b, &c] {
                d.log("alpha", &summary(5, 10), "0");
                d.log("beta", &summary(7, 9), "0");
            }
            let r = answer(&b, &c);
            assert_eq!(r.exit, 1, "{}", r.report);
            assert!(row(&r, "alpha").ends_with("NOT-ON-HOST"), "{}", r.report);
            assert!(r.report.contains(under), "{}", r.report);
            assert!(
                r.report.contains("too little for its floor"),
                "{}",
                r.report
            );
            assert!(verdict(&r).contains("1 not verified on this host (alpha)"));
        }
        // With a memory row, the row says the run's own refusal: the need, this
        // host's ceiling and the RAM that verifies it.
        let row_of = |t: String| {
            t.replace(
                "refused alpha",
                "memory alpha\t16.0\t24\t4.0\t3.0\t=\tm\nrefused alpha",
            )
        };
        let b = RunDir::new(&plan(SEAL, &[("alpha", "gate"), ("beta", "gate")], |t| t));
        let c = RunDir::new(&plan(
            MAIN,
            &[("alpha", "memory"), ("beta", "gate")],
            row_of,
        ));
        for d in [&b, &c] {
            d.log("alpha", &summary(5, 10), "0");
            d.log("beta", &summary(7, 9), "0");
        }
        let r = answer(&b, &c);
        assert!(row(&r, "alpha").ends_with("NOT-ON-HOST"), "{}", r.report);
        assert!(
            r.report.contains(
                "refused up front for memory under the candidate: needs ~3.0 GiB, this host gives 3.0 GiB"
            ),
            "{}",
            r.report
        );
        assert!(r.report.contains("verifies it"), "{}", r.report);
    }

    #[test]
    fn inputs_that_differ_cannot_be_compared() {
        // a library one run selected and the other did not
        let b = RunDir::new(&plan(SEAL, &[("alpha", "gate"), ("beta", "gate")], |t| t));
        let c = RunDir::new(&plan(MAIN, &[("alpha", "gate")], |t| t));
        for s in [&b, &c] {
            s.log("alpha", &summary(5, 10), "0");
        }
        b.log("beta", &summary(1, 1), "0");
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(row(&r, "beta").ends_with("INPUTS-DIFFER"));
        assert!(verdict(&r).starts_with("VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: "));
        // a policy decision against a gated check
        let b = RunDir::new(&plan(SEAL, &[("alpha", "no-lib")], |t| t));
        let c = RunDir::new(&plan(MAIN, &[("alpha", "gate")], |t| t));
        c.log("alpha", &summary(5, 10), "0");
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        // COULD NOT COMPARE outranks a refusal, and names it too
        let (b, c) = pair((&summary(1, 10), "0"), (&summary(7, 9), "0"));
        let edited = fs::read_to_string(c.plan()).unwrap() + "crate gamma\tgamma@1.0.0\tno-lib\n";
        fs::write(c.plan(), edited.replace("trustd /tmp/none.sock\n", "")).unwrap();
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            verdict(&r).contains("; and as measured, 1 lose proofs (alpha 4 lost)"),
            "{}",
            r.report
        );
    }

    #[test]
    fn runs_that_are_not_two_whole_tree_runs_of_one_tree_are_refused() {
        /// How the candidate's plan is edited.
        type Edit = fn(String) -> String;
        let cases: [(&str, Edit, &str); 9] = [
            (
                "same prover",
                |t| t.replace(MAIN, SEAL),
                "the candidate IS the published seal",
            ),
            (
                "another commit",
                |t| t.replace(COMMIT, "1111111111111111111111111111111111111111"),
                "the runs measured different trees",
            ),
            (
                "another tree",
                |t| t.replace(TREE, "2222222222222222222222222222222222222222"),
                "the runs measured different trees",
            ),
            (
                "no tree",
                |t| {
                    t.replace(&format!("commit {COMMIT}"), "commit -")
                        .replace(&format!("tree {TREE}"), "tree -")
                },
                "names no commit and tree",
            ),
            (
                "filed nothing",
                |t| {
                    t.replace(
                        "file \n",
                        "file its roster or scope list is not the tree's own\n",
                    )
                },
                "measured no tree a release reads: its roster or scope list is not the tree's own",
            ),
            (
                "narrowed",
                |t| {
                    let cut = t.find("file \n").unwrap();
                    t[..cut].replace("only \n", "only alpha\n")
                },
                "narrowed by ATERM_GATE_ONLY (alpha)",
            ),
            (
                "per commit",
                |t| t.replace("per-commit 0", "per-commit 1"),
                "a --per-commit run",
            ),
            (
                "not concluded",
                |t| t.replace("trustd /tmp/none.sock\n", ""),
                "did not conclude",
            ),
            (
                "another machine",
                |t| {
                    t.replace("host 14 cores, 24 GiB", "host 18 cores, 128 GiB")
                        .replace("host-mem 25769803776", "host-mem 137438953472")
                },
                "different machines",
            ),
        ];
        for (what, edit, said) in cases {
            let crates = [("alpha", "gate")];
            let b = RunDir::new(&plan(SEAL, &crates, |t| t));
            let c = RunDir::new(&plan(MAIN, &crates, edit));
            b.log("alpha", &summary(5, 10), "0");
            c.log("alpha", &summary(6, 10), "0");
            let r = answer(&b, &c);
            assert_eq!(r.exit, 2, "{what}: {}", r.report);
            assert!(r.report.contains(said), "{what}: {}", r.report);
            assert!(
                verdict(&r).starts_with("VERDICT: PUBLISH-REFUSED — COULD NOT COMPARE: "),
                "{what}: {}",
                r.report
            );
        }
        // the base must be the published seal when the caller names it
        let crates = [("alpha", "gate")];
        let b = RunDir::new(&plan(SEAL, &crates, |t| t));
        let c = RunDir::new(&plan(MAIN, &crates, |t| t));
        b.log("alpha", &summary(5, 10), "0");
        c.log("alpha", &summary(6, 10), "0");
        let bp = load("published seal's", &b.plan(), None).unwrap();
        let cp = load("candidate's", &c.plan(), None).unwrap();
        let none = Ratchet::default();
        let no = Retractions::default();
        assert_eq!(render(&compare(&bp, &cp, Some(SEAL), &none, &no)).exit, 0);
        let r = render(&compare(&bp, &cp, Some(MAIN), &none, &no));
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report
                .contains("is not the published seal's (61a997f0b92d)"),
            "{}",
            r.report
        );
    }

    #[test]
    fn an_unreadable_plan_is_could_not_compare() {
        let (b, _) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
        let args = Args {
            base: b.plan(),
            candidate: b.dir.path().join("no-such-plan"),
            ..Args::default()
        };
        assert_eq!(run(&args), 2);
        fs::write(b.plan(), "not a plan\n").unwrap();
        let args = Args {
            base: b.plan(),
            candidate: b.plan(),
            ..Args::default()
        };
        assert_eq!(run(&args), 2);
    }

    #[test]
    fn a_signal_is_read_from_the_words_of_log_reader() {
        let m = Measured::of_log(
            "  process didn't exit successfully: `/t/trustc --crate-name a` (signal: 15, SIGTERM: termination signal)\n"
                .as_bytes(),
            "101",
        );
        assert!(
            m.ended.contains("cargo: trustc ended by signal 15"),
            "{}",
            m.ended
        );
        assert!(ended_by_signal(&m));
        let m = Measured::of_log("Compiling\n".as_bytes(), "137");
        assert!(ended_by_signal(&m), "{}", m.ended);
        let m = Measured::of_log(
            "[UNKNOWN] [transport:missing-crate-summary] none (targo-trust)\nSummary: 1 proved (2 total)\nCoverage: unknown\n"
                .as_bytes(),
            "101",
        );
        assert!(!m.ended.is_empty() && !ended_by_signal(&m), "{}", m.ended);
    }

    #[test]
    fn runs_bounded_or_slotted_differently_are_not_compared() {
        type Edit = fn(String) -> String;
        let cases: [(&str, Edit, Edit, &str); 4] = [
            (
                "budgets differ",
                |t| t.replace("crate-budget 21600\n", "crate-budget 60\n"),
                |t| t,
                "the runs were bounded differently: the published seal's checks at a per-crate wall budget of 60 s, the candidate's at 21600 s",
            ),
            (
                "the base records no budget",
                |t| t.replace("crate-budget 21600\n", ""),
                |t| t,
                "the published seal's run's plan records no per-crate wall budget",
            ),
            (
                "the candidate records no budget",
                |t| t,
                |t| t.replace("crate-budget 21600\n", ""),
                "the candidate's run's plan records no per-crate wall budget",
            ),
            (
                "slot counts differ",
                |t| t,
                |t| t.replace("jobs 1\n", "jobs 4\n"),
                "the runs gated with different slot counts (the published seal's 1, the candidate's 4)",
            ),
        ];
        for (what, eb, ec, said) in cases {
            let crates = [("alpha", "gate")];
            let b = RunDir::new(&plan(SEAL, &crates, eb));
            let c = RunDir::new(&plan(MAIN, &crates, ec));
            b.log("alpha", &summary(5, 10), "0");
            c.log("alpha", &summary(6, 10), "0");
            let r = answer(&b, &c);
            assert_eq!(r.exit, 2, "{what}: {}", r.report);
            assert!(r.report.contains(said), "{what}: {}", r.report);
        }
    }

    #[test]
    fn a_name_listed_twice_is_not_compared() {
        // Two `crate dup` lines: one log is read per name, so dup@1.0.0's drop
        // from 100 to 5 would hide behind dup@2.0.0's equal 7 and 7.
        let edit = |t: String| {
            t.replace(
                "crate alpha\talpha@1.0.0\tgate\n",
                "crate alpha\talpha@1.0.0\tgate\ncrate dup\tdup@1.0.0\tgate\ncrate dup\tdup@2.0.0\tgate\n",
            )
        };
        let crates = [("alpha", "gate")];
        let b = RunDir::new(&plan(SEAL, &crates, edit));
        let c = RunDir::new(&plan(MAIN, &crates, edit));
        for (d, dup1) in [(&b, 100), (&c, 5)] {
            d.log("alpha", &summary(5, 10), "0");
            fs::write(d.dir.path().join("dup@1.0.0.log"), summary(dup1, 200)).unwrap();
            fs::write(d.dir.path().join("dup@1.0.0.log.rc"), "0").unwrap();
            fs::write(d.dir.path().join("dup@2.0.0.log"), summary(7, 9)).unwrap();
            fs::write(d.dir.path().join("dup@2.0.0.log.rc"), "0").unwrap();
        }
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report
                .contains("the published seal's run lists dup 2 times")
                && r.report.contains("the candidate's run lists dup 2 times"),
            "{}",
            r.report
        );
    }

    #[test]
    fn nothing_compared_is_never_a_pass() {
        // Every seal check failed its own way (a full disk, say) and the candidate
        // proves nothing anywhere: no library has a result under both, and no
        // record holds the candidate — never PUBLISH-OK.
        let crates = [("alpha", "gate"), ("beta", "gate")];
        let b = RunDir::new(&plan(SEAL, &crates, |t| t));
        let c = RunDir::new(&plan(MAIN, &crates, |t| t));
        for n in ["alpha", "beta"] {
            b.log(
                n,
                "error: failed to write: No space left on device\n",
                "101",
            );
            c.log(n, &summary(0, 999), "0");
        }
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report.contains(
                "COULD NOT COMPARE: no library has a result under both toolchains: nothing was compared"
            ),
            "{}",
            r.report
        );
        // …even when the seal's floors would each be met
        let floors = format!("alpha\t0\t9\t{DRIVER}\nbeta\t0\t9\t{DRIVER}\n");
        let r = answer_r(&b, &c, Some(&floors));
        assert_eq!(r.exit, 2, "{}", r.report);
    }

    #[test]
    fn a_base_that_names_no_prover_is_said_so() {
        let crates = [("alpha", "gate")];
        let b = RunDir::new(&plan(SEAL, &crates, |t| {
            let cut = t.find("file \n").unwrap();
            t[..cut].replace("only \n", "only alpha\n")
        }));
        let c = RunDir::new(&plan(MAIN, &crates, |t| t));
        b.log("alpha", &summary(5, 10), "0");
        c.log("alpha", &summary(6, 10), "0");
        let bp = load("published seal's", &b.plan(), None).unwrap();
        let cp = load("candidate's", &c.plan(), None).unwrap();
        let r = render(&compare(
            &bp,
            &cp,
            Some(SEAL),
            &Ratchet::default(),
            &Retractions::default(),
        ));
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report.contains("the base run names no prover, so nothing shows it is the published seal (a519d423f47f)")
                && !r.report.contains("prover - is not"),
            "{}",
            r.report
        );
    }

    #[test]
    fn runs_that_captured_no_records_cannot_be_compared() {
        // The lane ran without --obligations (or predates it): a count says
        // nothing of a loss behind a gain, so nothing is compared — and what the
        // counts show is still said.
        let (b, c) = pair((&summary(4, 10), "0"), (&summary(7, 9), "0"));
        for d in [&b, &c] {
            let t = fs::read_to_string(d.plan()).unwrap();
            fs::write(d.plan(), t.replace("obligations 1\n", "")).unwrap();
        }
        let r = answer(&b, &c);
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report.contains("COULD NOT COMPARE: the published seal's run captured no per-obligation records (its plan has no `obligations 1` line")
                && r.report.contains("COULD NOT COMPARE: the candidate's run captured no per-obligation records"),
            "{}",
            r.report
        );
        assert!(
            verdict(&r).contains("; and as measured, 1 lose proofs (alpha -1)"),
            "{}",
            r.report
        );
        // `obligations 0` is the same; anything else is no plan this judge reads.
        let t = fs::read_to_string(c.plan()).unwrap();
        fs::write(
            c.plan(),
            t.replace("crate-budget", "obligations 0\ncrate-budget"),
        )
        .unwrap();
        assert_eq!(answer(&b, &c).exit, 2);
        fs::write(
            c.plan(),
            t.replace("crate-budget", "obligations yes\ncrate-budget"),
        )
        .unwrap();
        assert!(load("candidate's", &c.plan(), None).is_err());
    }

    #[test]
    fn a_gain_never_offsets_a_loss() {
        // The seal proves alpha's rows 1-5; the candidate rows 2-7. By count it
        // gains one; by obligation it loses row 1, and is refused.
        let (b, c) = pair((&summary(6, 10), "0"), (&summary(7, 9), "0"));
        c.record("alpha", &rows_record("alpha", 6, 10, 1));
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(row(&r, "alpha").ends_with("+1  LOSS"), "{}", r.report);
        assert!(
            r.report.contains("REFUSED alpha (LOSS): 1 obligation the published seal proved is not proved by the candidate, in 1 function; 2 obligations gained, which offset nothing; 6 proved under the candidate against 5 under the published seal (+1)")
                && r.report.contains("\nLOST alpha alpha::f | assertion | src/lib.rs | `row 1` #1/1 | assertion | at src/lib.rs:1:0-1:1 | candidate: failed\n"),
            "{}",
            r.report
        );
        assert_eq!(
            verdict(&r),
            "VERDICT: PUBLISH-REFUSED — 1 lose proofs (alpha 1 lost); nothing is published"
        );
        assert!(
            r.obligations.lines().any(|l| l == format!("LOST\talpha\talpha::f\tassertion\tsrc/lib.rs\trow 1\t1/1\tassertion\tsrc/lib.rs:1:0-1:1\t{COMMIT}\t-\tfailed\t")),
            "{}",
            r.obligations
        );
        // A seal obligation the candidate reports nothing at is MISSING.
        let renamed = rows_record("alpha", 5, 10, 0).replace(
            "o\talpha::f\tassertion\tsrc/lib.rs\t3\t",
            "o\talpha::g\tassertion\tsrc/lib.rs\t3\t",
        );
        c.log("alpha", &summary(5, 10), "0")
            .record("alpha", &renamed);
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains("(1 of them MISSING: the candidate reports nothing at their key, which no retraction excuses)")
                && r.report.contains("\nMISSING alpha alpha::f | assertion | src/lib.rs | `row 3` #1/1 | assertion | at src/lib.rs:3:0-3:1 | candidate: MISSING\n"),
            "{}",
            r.report
        );
    }

    #[test]
    fn a_record_a_check_cannot_stand_by_gives_no_result_on_its_side() {
        fn missing(d: &RunDir, name: &str) {
            fs::remove_file(d.dir.path().join(format!("{name}@1.0.0.log.oblig.tsv"))).unwrap();
        }
        // The candidate's: gone, with the lane's reason; another package's; not
        // its Summary's counts; a key of no form this gate knows; cut short.
        let good = rows_record("alpha", 5, 10, 0);
        /// What breaks the candidate's record of `alpha`.
        type Break = Box<dyn Fn(&RunDir)>;
        let cases: Vec<(Break, &str)> = vec![
            (
                Box::new(|c: &RunDir| {
                    missing(c, "alpha");
                    c.mark("alpha", "oblig.err", "/x/report.ndjson: no per-obligation record: line 7: a `transport` record, which this gate does not read (a transport record?)\n");
                }),
                "the lane's reduction made none: /x/report.ndjson: no per-obligation record: line 7: a `transport` record",
            ),
            (
                Box::new(|c: &RunDir| missing(c, "alpha")),
                "alpha@1.0.0.log.oblig.tsv is missing",
            ),
            (
                Box::new(move |c: &RunDir| {
                    c.record("alpha", &rows_record("beta", 5, 10, 0));
                }),
                "is of package beta, not alpha",
            ),
            (
                Box::new(|c: &RunDir| {
                    c.record("alpha", &rows_record("alpha", 4, 10, 0));
                }),
                "holds 4 proved of 10 and the check's Summary says 5 of 10",
            ),
            (
                Box::new({
                    let good = good.clone();
                    move |c: &RunDir| {
                        c.record(
                            "alpha",
                            &good.replacen("\tsrc/lib.rs\t", "\t/opt/abs/lib.rs\t", 1),
                        );
                    }
                }),
                "the file `/opt/abs/lib.rs` is of no form this gate knows",
            ),
            (
                Box::new({
                    let good = good.clone();
                    move |c: &RunDir| {
                        c.record(
                            "alpha",
                            &good.replacen("\tassertion\t", "\tAssertion!\t", 1),
                        );
                    }
                }),
                "the kind `Assertion!` is of no form this gate knows",
            ),
            (
                Box::new({
                    let good = good.clone();
                    move |c: &RunDir| {
                        c.record("alpha", &good.replace("end\t10\n", ""));
                    }
                }),
                "it was cut short",
            ),
        ];
        for (make, said) in cases {
            let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
            make(&c);
            let r = answer(&b, &c);
            assert_eq!(r.exit, 1, "{said}: {}", r.report);
            assert!(
                row(&r, "alpha").ends_with(" 0  NO-RESULT"),
                "{said}: {}",
                r.report
            );
            assert!(
                r.report.contains("REFUSED alpha (NO-RESULT): the candidate's check left no usable per-obligation record (")
                    && r.report.contains(said),
                "{said}: {}",
                r.report
            );
        }
        // The seal's record gone: BASE-UNDECIDED — run the base again.
        let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
        missing(&b, "alpha");
        let r = answer(&b, &c);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            row(&r, "alpha").ends_with("BASE-UNDECIDED")
                && r.report.contains("REFUSED alpha (BASE-UNDECIDED): the published seal's check left no usable per-obligation record (alpha@1.0.0.log.oblig.tsv is missing)"),
            "{}",
            r.report
        );
        // …and a count loss with no record to name it is a LOSS all the same.
        c.log("alpha", &summary(4, 10), "0");
        missing(&c, "alpha");
        let r = answer(&b, &c);
        assert!(
            row(&r, "alpha").ends_with("-1  LOSS")
                && r.report.contains("; and no per-obligation record names what was lost (alpha@1.0.0.log.oblig.tsv is missing; alpha@1.0.0.log.oblig.tsv is missing)"),
            "{}",
            r.report
        );
        // A seal check without a result holds the candidate to its record by
        // count — and the candidate's record must still stand.
        let (b, c) = pair((&summary(5, 10), "0"), (&summary(7, 9), "0"));
        b.log("alpha", "verification did not run\n", "2");
        missing(&c, "alpha");
        let r = answer_r(&b, &c, Some(&format!("alpha\t5\t10\t{DRIVER}\n")));
        assert!(row(&r, "alpha").ends_with("NO-RESULT"), "{}", r.report);
    }

    /// A trust repository: the seal's commit S, the fix F (adding the
    /// reproducer), the candidate C after F, and another candidate O after S
    /// that lacks F.
    struct Trust {
        dir: aterm_tempfile::TempDir,
        s: String,
        f: String,
        c: String,
        o: String,
    }

    impl Trust {
        fn new() -> Trust {
            let dir = aterm_tempfile::Builder::new()
                .prefix("trust-repo")
                .tempdir()
                .unwrap();
            let root = dir.path().to_path_buf();
            let git = |args: &[&str]| {
                let out = crate::ledger::testing::git()
                    .arg("-C")
                    .arg(&root)
                    .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                    .args([
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "core.hooksPath=/dev/null",
                    ])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            };
            git(&["init", "-q", "-b", "main"]);
            fs::write(root.join("README"), "trust\n").unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "the seal"]);
            let s = git(&["rev-parse", "HEAD"]);
            fs::create_dir_all(root.join("crates/trust-vcgen/tests")).unwrap();
            fs::write(
                root.join("crates/trust-vcgen/tests/false_proofs.rs"),
                "#[test]\nfn guard_by_reaching_def() {}\n",
            )
            .unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "fix: the false proof"]);
            let f = git(&["rev-parse", "HEAD"]);
            fs::write(root.join("README"), "trust main\n").unwrap();
            git(&["commit", "-q", "-am", "the candidate"]);
            let c = git(&["rev-parse", "HEAD"]);
            git(&["checkout", "-q", "-b", "other", &s]);
            fs::write(root.join("OTHER"), "o\n").unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "another candidate"]);
            let o = git(&["rev-parse", "HEAD"]);
            Trust { dir, s, f, c, o }
        }
    }

    /// The driver line of a run at trust `sha`.
    fn driver_of(sha: &str) -> String {
        format!(
            "driver-id targo 1.99.0-dev ({} 2026-10-07) (targo 0.10.0+g{})",
            &sha[..9],
            &sha[..9]
        )
    }

    /// The seal (at `seal`) proving alpha's rows 1-5, the candidate (whose run's
    /// driver names `drives`) rows 1-`cand_proved`.
    fn trust_pair(seal: &str, drives: &str, cand_proved: u64) -> (RunDir, RunDir) {
        let crates = [("alpha", "gate"), ("beta", "gate")];
        let (bd, cd) = (driver_of(seal), driver_of(drives));
        let b = RunDir::new(&plan(SEAL, &crates, |t| {
            t.replace("driver-id targo 1.99.0-dev (stub)", &bd)
        }));
        let c = RunDir::new(&plan(MAIN, &crates, |t| {
            t.replace("driver-id targo 1.99.0-dev (stub)", &cd)
        }));
        b.log("alpha", &summary(5, 10), "0")
            .log("beta", &summary(7, 9), "0");
        c.log("alpha", &summary(cand_proved, 10), "0")
            .log("beta", &summary(7, 9), "0");
        (b, c)
    }

    /// The ledger, its header and `rows`.
    fn ledger(rows: &[String]) -> String {
        let mut t = format!("{}\n", trust_oblig::LEDGER_MAGIC);
        for r in rows {
            t.push_str(r);
            t.push('\n');
        }
        t
    }

    /// A retraction of alpha's row `n` by `commit`, reproduced by `test`,
    /// audited where the runs measured (aterm [`COMMIT`], row `n` at line `n`)
    /// against the published seal at trust `seal`.
    fn retract(n: u64, seal: &str, commit: &str, test: &str) -> String {
        retract_at(n, COMMIT, seal, commit, test)
    }

    /// [`retract`], audited at aterm `aterm`.
    fn retract_at(n: u64, aterm: &str, seal: &str, commit: &str, test: &str) -> String {
        format!(
            "alpha\talpha::f\tassertion\tsrc/lib.rs\trow {n}\t1/1\tassertion\tsrc/lib.rs:{n}:0-{n}:1\t{aterm}\t{seal}\t{commit}\tcrates/trust-vcgen/tests/false_proofs.rs::{test}\treviewer 2026-10-07 class A (stale field read)"
        )
    }

    /// Compare with `ledger`, the trust repository (when given) and the two
    /// runs' commits.
    fn answer_l(
        base: &RunDir,
        cand: &RunDir,
        ledger: Option<&str>,
        trust: Option<&Trust>,
        commits: (Option<&str>, Option<&str>),
    ) -> Rendered {
        let b = load("published seal's", &base.plan(), None).unwrap();
        let c = load("candidate's", &cand.plan(), None).unwrap();
        let git = trust.map(|t| crate::ledger::testing::Git::new(t.dir.path()));
        let retractions = Retractions {
            ledger: ledger.map(|t| {
                (
                    t.to_string(),
                    "tools/trust-gate-retractions.tsv".to_string(),
                )
            }),
            trust: git.as_ref().map(|g| TrustRepo {
                git: g,
                shown: "the trust repository".into(),
            }),
            aterm: None,
            candidate_commit: commits.1.map(str::to_string),
            base_commit: commits.0.map(str::to_string),
        };
        render(&compare(&b, &c, None, &Ratchet::default(), &retractions))
    }

    #[test]
    fn a_retraction_excuses_a_loss_only_with_its_commit_in_the_candidate_and_the_obligation_unproved()
     {
        let t = Trust::new();
        let row5 = retract(5, &t.s, &t.f, "guard_by_reaching_def");
        // The candidate at C, which contains the fix F, no longer proves row 5,
        // which the ledger retracts: excused, and the publish proceeds.
        let (b, c) = trust_pair(&t.s, &t.c, 4);
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(std::slice::from_ref(&row5))),
            Some(&t),
            (Some(&t.s), Some(&t.c)),
        );
        assert_eq!(r.exit, 0, "{}", r.report);
        assert!(row(&r, "alpha").ends_with("-1  OK"), "{}", r.report);
        assert!(
            r.report.contains(&format!(
                "\nEXCUSED alpha alpha::f | assertion | src/lib.rs | `row 5` #1/1 | assertion | at src/lib.rs:5:0-5:1 | candidate: failed | retracted by trust {} (tools/trust-gate-retractions.tsv line 2; reproducer crates/trust-vcgen/tests/false_proofs.rs::guard_by_reaching_def; reviewed reviewer 2026-10-07 class A (stale field read))\n",
                &t.f[..12]
            )),
            "{}",
            r.report
        );
        assert!(
            verdict(&r).starts_with("VERDICT: PUBLISH-OK — the candidate proves every obligation the published seal proves on this host (2 compared, 1 excused by the retraction ledger,"),
            "{}",
            r.report
        );
        assert!(
            r.obligations.lines().any(|l| l.starts_with(
                "EXCUSED\talpha\talpha::f\tassertion\tsrc/lib.rs\trow 5\t1/1\tassertion\t"
            )),
            "{}",
            r.obligations
        );
        // The same loss in a candidate WITHOUT the fix (O): refused, naming the row.
        let (b, c) = trust_pair(&t.s, &t.o, 4);
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(std::slice::from_ref(&row5))),
            Some(&t),
            (Some(&t.s), Some(&t.o)),
        );
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains(&format!(
                "| candidate: failed | retraction line 2 does not apply: the candidate does not contain trust {}\n",
                &t.f[..12]
            )),
            "{}",
            r.report
        );
        // No candidate commit given, or one its run's driver does not name: no
        // retraction applies — and the second cannot be compared at all.
        let (b, c) = trust_pair(&t.s, &t.c, 4);
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(std::slice::from_ref(&row5))),
            Some(&t),
            (Some(&t.s), None),
        );
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains(
                "retraction line 2 does not apply: the candidate's trust commit was not given"
            ),
            "{}",
            r.report
        );
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(std::slice::from_ref(&row5))),
            Some(&t),
            (Some(&t.s), Some(&t.o)),
        );
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report.contains(&format!("COULD NOT COMPARE: the candidate's trust commit {} is not the one its run's driver names ({}", &t.o[..12], &t.c[..9])),
            "{}",
            r.report
        );
        // A candidate that contains the fix and STILL proves row 5: the ledger is
        // wrong for it — stale, refused.
        let (b, c) = trust_pair(&t.s, &t.c, 5);
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(std::slice::from_ref(&row5))),
            Some(&t),
            (Some(&t.s), Some(&t.c)),
        );
        assert_eq!(r.exit, 2, "{}", r.report);
        assert!(
            r.report.contains("COULD NOT COMPARE: tools/trust-gate-retractions.tsv line 2 is STALE for this candidate (alpha): the candidate contains trust")
                && r.report.contains("and still proves alpha::f | assertion | src/lib.rs | `row 5` #1/1 | assertion (at src/lib.rs:5:0-5:1: proved)"),
            "{}",
            r.report
        );
        // A row that names nothing the seal proved is said (UNUSED), not refused.
        let (b, c) = trust_pair(&t.s, &t.c, 5);
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(&[retract(7, &t.s, &t.f, "guard_by_reaching_def")])),
            Some(&t),
            (Some(&t.s), Some(&t.c)),
        );
        assert_eq!(r.exit, 0, "{}", r.report);
        assert!(
            r.report.contains("NOTE tools/trust-gate-retractions.tsv line 2 (UNUSED, alpha): the published seal did not prove it (failed)"),
            "{}",
            r.report
        );
    }

    #[test]
    fn a_retraction_applies_only_against_the_seal_in_the_aterm_tree_and_at_the_site_it_was_audited_at()
     {
        let t = Trust::new();
        let (b, c) = trust_pair(&t.s, &t.c, 4);
        let commits = (Some(t.s.as_str()), Some(t.c.as_str()));
        // CONTROL: audited where the runs measured, at the seal's site — excused.
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(&[retract(5, &t.s, &t.f, "guard_by_reaching_def")])),
            Some(&t),
            commits,
        );
        assert_eq!(r.exit, 0, "{}", r.report);
        // Audited at another aterm commit, and no aterm repository to compare
        // the two trees in: not applied, and the loss refuses.
        let elsewhere = "0123456789abcdef0123456789abcdef01234567";
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(&[retract_at(
                5,
                elsewhere,
                &t.s,
                &t.f,
                "guard_by_reaching_def",
            )])),
            Some(&t),
            commits,
        );
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains(&format!(
                "| candidate: failed | retraction line 2 does not apply: it was audited at aterm 0123456789ab and the runs measured aterm {}, and no aterm repository was given to compare their trees in\n",
                &COMMIT[..12]
            )),
            "{}",
            r.report
        );
        // Audited against another seal than the base ran: its record may order
        // look-alikes otherwise — not applied.
        let r = answer_l(
            &b,
            &c,
            Some(&ledger(&[retract(5, &t.o, &t.f, "guard_by_reaching_def")])),
            Some(&t),
            commits,
        );
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains(&format!(
                "| candidate: failed | retraction line 2 does not apply: it was audited against the published seal at trust {} and the base ran trust {}",
                &t.o[..12],
                &t.s[..12]
            )),
            "{}",
            r.report
        );
        // At a site the published seal does not report the obligation at.
        let moved = retract(5, &t.s, &t.f, "guard_by_reaching_def")
            .replace("\tsrc/lib.rs:5:0-5:1\t", "\tsrc/lib.rs:5:0-5:2\t");
        let r = answer_l(&b, &c, Some(&ledger(&[moved])), Some(&t), commits);
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains("retraction line 2 does not apply: the obligation its key names is at src/lib.rs:5:0-5:1 under the published seal, and the row was audited at src/lib.rs:5:0-5:2"),
            "{}",
            r.report
        );
        // The lost obligation's TSV row: a retraction's first ten columns, the
        // commit the runs measured and the seal's among them.
        assert!(
            r.obligations.lines().any(|l| l.starts_with(&format!(
                "LOST\talpha\talpha::f\tassertion\tsrc/lib.rs\trow 5\t1/1\tassertion\tsrc/lib.rs:5:0-5:1\t{COMMIT}\t{}\tfailed\tretraction line 2 does not apply: ",
                t.s
            ))),
            "{}",
            r.obligations
        );
    }

    #[test]
    fn a_retraction_the_trust_repository_cannot_vouch_for_compares_nothing() {
        let t = Trust::new();
        let (b, c) = trust_pair(&t.s, &t.c, 4);
        let no_such = "0123456789012345678901234567890123456789";
        for (rows, trust, seal, said) in [
            (vec![retract(5, &t.s, no_such, "guard_by_reaching_def")], true, t.s.as_str(), format!("line 2: trust {} is not a commit of the trust repository", &no_such[..12])),
            (vec![retract(5, &t.s, &t.f, "no_such_test")], true, t.s.as_str(), "line 2: its reproducer: crates/trust-vcgen/tests/false_proofs.rs at trust".to_string()),
            (vec![retract(5, &t.s, &t.s, "guard_by_reaching_def")], true, t.s.as_str(), "line 2: its reproducer: crates/trust-vcgen/tests/false_proofs.rs is not a file of trust".to_string()),
            (vec![retract(5, &t.s, &t.f, "guard_by_reaching_def")], true, t.c.as_str(), format!("line 2: trust {} is already in the published seal ({}), so the seal's proof is not one it retracts", &t.f[..12], &t.c[..12])),
            (vec![retract(5, &t.s, &t.f, "guard_by_reaching_def")], false, t.s.as_str(), "tools/trust-gate-retractions.tsv: 1 retraction(s), and no trust repository (--trust-repo)".to_string()),
            (vec![retract(5, &t.s, &t.f, "guard_by_reaching_def").replace("\tassertion\t", "\tAssert\t")], true, t.s.as_str(), "line 2: the kind `Assert` is of no form this gate knows".to_string()),
            (vec![retract(5, &t.s, &t.f, "guard_by_reaching_def").replace("\t1/1\tassertion\t", "\tfirst\tassertion\t")], true, t.s.as_str(), "line 2: nth `first` is not <place>/<of>".to_string()),
            (vec!["alpha\tonly three\tfields".to_string()], true, t.s.as_str(), "line 2: 3 fields, and a retraction has exactly 13".to_string()),
        ] {
            let (b, _) = trust_pair(seal, &t.c, 4);
            let text = ledger(&rows);
            let r = answer_l(&b, &c, Some(&text), trust.then_some(&t), (Some(seal), Some(&t.c)));
            assert_eq!(r.exit, 2, "{said}: {}", r.report);
            assert!(r.report.contains(&said), "{said}: {}", r.report);
            // …and the loss it would have excused is still named.
            assert!(verdict(&r).contains("; and as measured, 1 lose proofs (alpha 1 lost)"), "{said}: {}", r.report);
        }
        // No ledger at all: nothing is excused, and the report says so.
        let r = answer_l(&b, &c, None, Some(&t), (Some(&t.s), Some(&t.c)));
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(
            r.report.contains(
                "NOTE: no retraction ledger was given (--retractions): no loss is excused"
            ),
            "{}",
            r.report
        );
        // A ledger with its header only (the committed one today) is no error.
        let r = answer_l(&b, &c, Some(&ledger(&[])), None, (None, None));
        assert_eq!(r.exit, 1, "{}", r.report);
        assert!(!r.report.contains("COULD NOT COMPARE"), "{}", r.report);
        // The committed ledger reads.
        let committed = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/trust-gate-retractions.tsv"
        ))
        .unwrap();
        let l = trust_oblig::parse_ledger(
            &committed,
            "tools/trust-gate-retractions.tsv",
            &BTreeSet::new(),
        );
        assert!(l.errors.is_empty(), "{:?}", l.errors);
    }

    #[test]
    fn the_ratchet_is_the_one_the_base_plan_names_unless_given() {
        let crates = [("alpha", "gate"), ("beta", "gate")];
        let dir = aterm_tempfile::Builder::new()
            .prefix("trust-compare-ratchet")
            .tempdir()
            .unwrap();
        let named = dir.path().join("named.tsv");
        let given = dir.path().join("given.tsv");
        fs::write(&named, format!("alpha\t5\t10\t{DRIVER}\n")).unwrap();
        fs::write(&given, format!("alpha\t6\t10\t{DRIVER}\n")).unwrap();
        let ratchet_line = format!("ratchet {}\n", named.display());
        let b = RunDir::new(&plan(SEAL, &crates, |t| {
            t.replace("crate-budget", &format!("{ratchet_line}crate-budget"))
        }));
        let c = RunDir::new(&plan(MAIN, &crates, |t| t));
        b.log("alpha", "verification did not run\n", "2");
        b.log("beta", &summary(7, 9), "0");
        c.log("alpha", &summary(5, 10), "0");
        c.log("beta", &summary(7, 9), "0");
        let base = load("published seal's", &b.plan(), None).unwrap();
        let cand = load("candidate's", &c.plan(), None).unwrap();
        let alpha = |given: Option<&Path>| {
            let r = render(&compare(
                &base,
                &cand,
                None,
                &ratchet_for(given, &base),
                &Retractions::default(),
            ));
            row(&r, "alpha").to_string()
        };
        // The base plan's ratchet holds alpha at 5 (met by count, refused per
        // obligation); --ratchet at 6 (a loss).
        assert!(alpha(None).ends_with(" 0  NO-BASELINE"), "{}", alpha(None));
        assert!(
            alpha(Some(&given)).ends_with("-1  LOSS"),
            "{}",
            alpha(Some(&given))
        );
        let args = Args {
            base: b.plan(),
            candidate: c.plan(),
            ratchet: Some(given),
            ..Args::default()
        };
        assert_eq!(run(&args), 1);
    }
}
