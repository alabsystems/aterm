// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VERDICT DISCIPLINE — the most important property in the gate.
//!
//! The bash gate printed
//!
//! ```text
//!   VERIFY: PASS (mode=fast scope=workspace, 0 skipped) — merge contract satisfied
//! ```
//!
//! on ANY `rc == 0`. So a `--scope aterm-grid` run — one crate of sixty-odd built
//! and tested, nothing else compiled — claimed the whole merge contract, and so
//! did a run where tippy was absent, or the Trust stage2 was mid-rebuild, or the
//! GUI smoke skipped for want of a WindowServer session. This repo has NO CI: that
//! sentence is the only thing standing between "I verified" and "I believed I
//! verified", and it was lying whenever the run was narrower than the contract.
//!
//! The repair, preserved here exactly: count and NAME the skips, and refuse the
//! merge-contract sentence for any narrowed run. This module is where that lives.
//! [`MERGE_CONTRACT_SENTENCE`] is the ONE place the words exist, [`verdict`] is
//! the ONE function that may emit them, and it is exhaustively tested over the
//! whole (mode × scope × skips × failures) space below — including the
//! explicit property that a scoped or skipped run CANNOT print it.
//!
//! THE DIFFERENTIAL (2026-09-26). "Nothing failed" became "nothing NEW
//! failed": a run judged against main's receipt for its base
//! ([`crate::differential`]) may claim the contract when every red it found is
//! INHERITED — red on main with the same failure, for less than the age cap —
//! and the verdict names each one and says `N new, M inherited (red on main
//! since <sha>)`. With no base it is the absolute rule, unchanged. A skip, a
//! narrowing or a COULD NOT RUN forfeits the claim exactly as before; the
//! differential only ever excuses a red main demonstrably has.
//!
//! THE TIERS (2026-09-26). The merge contract is the LAND tier: a `--measure`
//! run ran none of it, so it cannot claim it however green it is, and the
//! default run — which does not run the MEASURE tier — names every MEASURE
//! stage it left out in its verdict, under `MEASURE tier: not part of the
//! merge contract` ([`crate::plan::tier_titles`]). Leaving a tier out is a
//! statement the verdict makes, never a silent skip.
//!
//! The scope axis is the SCOPE KIND, not a flag: `--scope`, a `--changed` cone
//! and an empty `--changed` selection are three columns of the same matrix, and
//! any future narrowing joins it by being a [`Scope`] variant that is not
//! `Workspace`. That is why the predicate asks [`Scope::is_workspace`] rather
//! than asking which flag the caller passed — a tier cannot be added without
//! answering the question.

use crate::cli::Mode;
use crate::differential::{self, Against, Disposition, NewWhy, Since, short};
use crate::exit;
use crate::ladder::{Finding, Tally};
use crate::plan::{self, Tier};
use crate::scope::Scope;

/// The claim itself. Nothing else in the crate may spell these words.
pub const MERGE_CONTRACT_SENTENCE: &str = "merge contract satisfied";

/// A rendered verdict: the `=== verdict ===` block, the process exit code, and
/// the one bit everything else is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub text: String,
    pub exit: i32,
    /// True only when this run discharged the WHOLE merge contract.
    pub claims_merge_contract: bool,
}

/// [`claims`] under the absolute rule: no base, so every red counts.
#[must_use]
pub fn discharges_merge_contract(mode: Mode, scope: &Scope, t: &Tally) -> bool {
    claims(mode, scope, t, &Against::absolute())
}

/// Is this run entitled to the merge-contract sentence?
///
/// The contract is the WHOLE-TREE run of the LAND tier with nothing skipped
/// and nothing NEW failed. Five independent ways to lose it, and the caller
/// cannot forget one because this is the only predicate [`verdict_against`]
/// consults:
///  * the run did not run the LAND tier — `--measure` runs the MEASURE tier
///    alone ([`Tier`]) — so it decided nothing the contract asks;
///  * the run was NARROWED — `--scope` to one crate, or `--changed` to the
///    diff's reverse-dependency cone. The question is the SCOPE KIND, never the
///    flag that produced it, so a new narrowing forfeits the claim by existing
///    rather than by remembering to;
///  * a stage was skipped, so nothing is claimed about it;
///  * a gate failed and `against` does not excuse it — with no base every
///    failure counts; against main's receipt a row is excused only when every
///    one of its itemized failures is INHERITED
///    ([`differential::row_excused`]);
///  * the environment was broken (nothing was decided), which nothing excuses.
#[must_use]
pub fn claims(mode: Mode, scope: &Scope, t: &Tally, against: &Against) -> bool {
    mode.runs(Tier::Land)
        && scope.is_workspace()
        && t.skipped() == 0
        && t.could_not_run.is_empty()
        && blocking_rows(t, judged(t, against).as_deref()).is_empty()
}

/// Every finding judged against the base, row by row — `None` under the
/// absolute rule.
fn judged(t: &Tally, against: &Against) -> Option<Vec<Vec<Disposition>>> {
    match against {
        Against::Base(base) => Some(differential::judge_tally(t, base)),
        Against::Absolute(_) => None,
    }
}

/// The findings rows that are NOT excused: every one under the absolute rule.
fn blocking_rows(t: &Tally, judged: Option<&[Vec<Disposition>]>) -> Vec<usize> {
    (0..t.gate_failures.len())
        .filter(|&row| !judged.is_some_and(|j| differential::row_excused(j, row)))
        .collect()
}

/// [`verdict_against`] under the absolute rule.
#[must_use]
pub fn verdict(mode: Mode, scope: &Scope, t: &Tally) -> Verdict {
    verdict_against(mode, scope, t, &Against::absolute())
}

/// Every finding with its disposition, sorted into what the verdict names.
#[derive(Default)]
struct Sorted<'a> {
    new: Vec<(&'a Finding, NewWhy)>,
    expired: Vec<(&'a Finding, &'a Since)>,
    inherited: Vec<(&'a Finding, &'a Since)>,
}

fn sort<'a>(t: &'a Tally, judged: &'a [Vec<Disposition>]) -> Sorted<'a> {
    let mut s = Sorted::default();
    for (row, dispositions) in judged.iter().enumerate() {
        for (f, d) in t.findings_of(row).iter().zip(dispositions) {
            match d {
                Disposition::New(why) => s.new.push((f, *why)),
                Disposition::Expired(since) => s.expired.push((f, since)),
                Disposition::Inherited(since) => s.inherited.push((f, since)),
            }
        }
    }
    s
}

/// `N new, M inherited (red on main since <sha>)`, and how many are past the
/// cap — the one line a reader quotes. `unitemized` rows count as new.
fn summary(s: &Sorted<'_>, unitemized: usize) -> String {
    let mut line = format!(
        "{} new, {} inherited",
        s.new.len() + unitemized,
        s.inherited.len()
    );
    if let Some((_, oldest)) = s.inherited.iter().min_by_key(|(_, since)| since.when) {
        line.push_str(&format!(" (red on main since {})", short(&oldest.commit)));
    }
    if !s.expired.is_empty() {
        line.push_str(&format!(
            ", {} red on main past the {} h cap",
            s.expired.len(),
            differential::INHERITED_CAP_SECS / 3600
        ));
    }
    line
}

/// `- <id> (red on main since <sha>, <n> h)` for each.
fn since_list(text: &mut String, items: &[(&Finding, &Since)], now: u64) {
    for (f, since) in items {
        text.push_str(&format!(
            "      - {} (red on main since {}, {} h)\n",
            f.id,
            short(&since.commit),
            differential::hours(now.saturating_sub(since.when))
        ));
    }
}

/// The inherited reds, named — printed under every verdict that has any,
/// because an inherited red is main's to fix and a reader must see it.
fn inherited_block(text: &mut String, s: &Sorted<'_>, base: &differential::BaseReds) {
    if s.inherited.is_empty() {
        return;
    }
    text.push_str(&format!(
        "          {}, judged against main's {} for {}.\n",
        summary(s, 0),
        base.source,
        short(&base.commit)
    ));
    text.push_str(
        "          INHERITED — red on main with the same failure, so not this change's; named\n\
         \x20         here and in the receipt (`inherited`) for main to fix:\n",
    );
    since_list(text, &s.inherited, base.now);
}

/// Render the verdict block and decide the exit code, judging every red
/// against `against` ([`claims`]). A run of a mode that does not run the
/// MEASURE tier ends the block by naming every MEASURE stage it left out
/// ([`left_out_block`]).
#[must_use]
pub fn verdict_against(mode: Mode, scope: &Scope, t: &Tally, against: &Against) -> Verdict {
    let mut v = judged_verdict(mode, scope, t, against);
    v.text.push_str(&under_load_block(t));
    v.text.push_str(&left_out_block(mode, scope));
    v
}

/// THE TIMING LABEL (2026-09-26): every failure that reads as a clock running
/// out ([`Finding::timing`]) from a stage the machine was busy around
/// ([`crate::ladder::StageLoad::loaded`]: load above half its cores), named
/// with the words that marked it and the load — or nothing, when there is
/// none. A LABEL, NOT AN EXCUSE: it changes no exit code and no claim, and
/// nothing is re-run — a retry once hid a real flock defect (4e15ec313:
/// 9/200 failures with a retry, 33/200 without). It tells the reader which
/// reds to re-take on a quieter machine before believing them.
#[must_use]
pub fn under_load_block(t: &Tally) -> String {
    let mut items = Vec::new();
    for row in 0..t.gate_failures.len() {
        let Some(load) = t.load_of(row).filter(crate::ladder::StageLoad::loaded) else {
            continue;
        };
        for f in t.findings_of(row) {
            if let Some(marker) = f.timing {
                items.push((f, load, marker));
            }
        }
    }
    let Some((_, first, _)) = items.first() else {
        return String::new();
    };
    let mut text = format!(
        "          UNDER LOAD — a label, not an excuse: {} failure(s) read as a clock running\n\
         \x20         out, in a stage whose one-minute load passed half of the machine's {}\n\
         \x20         cores. A busy machine can starve a correct test, and each still counts\n\
         \x20         here; re-take it on a quieter machine before reading it as the change's:\n",
        items.len(),
        first.cores
    );
    for (f, load, marker) in &items {
        text.push_str(&format!(
            "      - {} (`{marker}`; load {})\n",
            f.id,
            load.describe()
        ));
    }
    text
}

/// The MEASURE tier a run did NOT run, named — empty for a mode that runs it.
/// Printed under every verdict of the default run, green or red, so what the
/// merge contract leaves out is a statement and never a silent skip.
#[must_use]
pub fn left_out_block(mode: Mode, scope: &Scope) -> String {
    if mode.runs(Tier::Measure) {
        return String::new();
    }
    let mut text = String::from(
        "          MEASURE tier: not part of the merge contract, not run here; a release cut\n\
         \x20         needs it green (`tools/verify.sh --measure`):\n",
    );
    for title in plan::tier_titles(scope, Tier::Measure) {
        text.push_str(&format!("      - {title}\n"));
    }
    text
}

fn judged_verdict(mode: Mode, scope: &Scope, t: &Tally, against: &Against) -> Verdict {
    let mut text = String::from("\n=== verdict ===\n");
    let scope_word = scope.desc();
    let mode_word = mode.as_str();

    let judged = judged(t, against);
    let sorted = judged.as_deref().map(|j| sort(t, j)).unwrap_or_default();
    let base = match against {
        Against::Base(base) => Some(base),
        Against::Absolute(_) => None,
    };
    let inherited_word = if sorted.inherited.is_empty() {
        String::new()
    } else {
        format!(", {} inherited", sorted.inherited.len())
    };

    // JUDGED AGAINST MAIN, AND SOMETHING IS NEW: the new reds are the findings,
    // and the inherited ones are named apart so nobody fixes them twice.
    if let Some(base) = base
        && !blocking_rows(t, judged.as_deref()).is_empty()
    {
        text.push_str(&format!(
            "  VERIFY: FAIL (mode={mode_word} scope={scope_word}) — DO NOT merge\n"
        ));
        // A row a caller put in the tally without itemizing it has no finding
        // to judge: it is new, named by its label.
        let unitemized: Vec<&String> = blocking_rows(t, judged.as_deref())
            .into_iter()
            .filter(|&row| t.findings_of(row).is_empty())
            .map(|row| &t.gate_failures[row])
            .collect();
        text.push_str(&format!(
            "          {}, judged against main's {} for {}:\n",
            summary(&sorted, unitemized.len()),
            base.source,
            short(&base.commit)
        ));
        if !sorted.new.is_empty() || !unitemized.is_empty() {
            text.push_str(
                "          NEW — findings about the change, and the stage's own block above says why:\n",
            );
            for label in &unitemized {
                text.push_str(&format!("      - {label}\n"));
            }
            for (f, why) in &sorted.new {
                let note = match why {
                    NewWhy::NotOnMain => String::new(),
                    NewWhy::FailsDifferently => {
                        " — red on main too, but failing differently".to_string()
                    }
                    NewWhy::Opaque(why) => format!(" — never inherited: {why}"),
                };
                text.push_str(&format!("      - {}{note}\n", f.id));
            }
        }
        if !sorted.expired.is_empty() {
            text.push_str(&format!(
                "          PAST THE {} h CAP — main has been red on these that long, so they block\n\
                 \x20         again until main is fixed:\n",
                differential::INHERITED_CAP_SECS / 3600
            ));
            since_list(&mut text, &sorted.expired, base.now);
        }
        if !sorted.inherited.is_empty() {
            text.push_str(
                "          INHERITED — red on main with the same failure, so not this change's:\n",
            );
            since_list(&mut text, &sorted.inherited, base.now);
        }
        could_not_run_tail(&mut text, t);
        return Verdict {
            text,
            exit: exit::FAILED,
            claims_merge_contract: false,
        };
    }

    // NAMED, for the reason `Tally` gives: the ladder row that decided the run is
    // one line in tens of thousands, and the verdict is the line a reader quotes.
    if base.is_none() && !t.gate_failures.is_empty() {
        let n = t.gate_failures.len();
        text.push_str(&format!(
            "  VERIFY: FAIL (mode={mode_word} scope={scope_word}) — DO NOT merge\n"
        ));
        text.push_str(&format!(
            "          {n} gate(s) decided AGAINST the tree; each stage's block above says why:\n"
        ));
        for f in &t.gate_failures {
            text.push_str(&format!("      - {f}\n"));
        }
        if let Against::Absolute(Some(why)) = against {
            text.push_str(&format!(
                "          (No differential — {why}. So every red counts, main's own included.)\n"
            ));
        }
        could_not_run_tail(&mut text, t);
        return Verdict {
            text,
            exit: exit::FAILED,
            claims_merge_contract: false,
        };
    }

    // Nothing FAILED and nothing was decided either. Reported as its own verdict
    // so it can never be read as a finding about the change — the same mistake
    // the old blocking .githooks/pre-push refused to make when the driver was
    // missing, and the same one the release cutter's receipt report refuses by
    // treating an unreadable receipt as no pass rather than as permission.
    if !t.could_not_run.is_empty() {
        let n = t.could_not_run.len();
        text.push_str(&format!(
            "  VERIFY: COULD NOT RUN (mode={mode_word} scope={scope_word}) — DO NOT merge\n"
        ));
        // Nothing decided is not "nothing wrong" (2026-09-27): most often the
        // machine (no driver, a missing helper, a refused test), but a paint
        // or spin probe that could not launch its window is COULD NOT RUN too,
        // and that can be a crash in the code — so the reader is sent to each
        // row's own reason, never told the change is cleared or which cause is
        // likelier.
        text.push_str(&format!(
            "          {n} could not execute — no verdict; one that could not launch may be a \
             crash in the change:\n"
        ));
        for c in &t.could_not_run {
            text.push_str(&format!("      - {c}\n"));
        }
        text.push_str("          Fix each reason above and run again.\n");
        if let Some(base) = base {
            inherited_block(&mut text, &sorted, base);
        }
        return Verdict {
            text,
            exit: exit::COULD_NOT_RUN,
            claims_merge_contract: false,
        };
    }

    // GREEN — but say precisely WHICH green. The merge contract is the WHOLE-TREE
    // run with nothing skipped; anything narrower proved something real and
    // something smaller, and printing the same sentence for both is how a scoped
    // run gets mistaken for a landing licence.
    if !claims(mode, scope, t, against) {
        let n = t.skipped();
        text.push_str(&format!(
            "  VERIFY: PASS (mode={mode_word} scope={scope_word}, {n} skipped{inherited_word}) —\n"
        ));
        text.push_str(if sorted.inherited.is_empty() {
            "          NOT the merge contract. Everything that ran was green; the contract\n"
        } else {
            "          NOT the merge contract. Nothing new failed; the contract\n"
        });
        text.push_str(
            "          is the WHOLE-TREE run with nothing skipped, and this run was narrower:\n",
        );
        if !mode.runs(Tier::Land) {
            text.push_str(
                "      - --measure ran the MEASURE tier alone: no stage of the merge contract (the \
                 LAND tier) ran\n",
            );
        }
        if let Some(why) = scope.narrowing() {
            text.push_str(&format!("      - {why}\n"));
        }
        if n != 0 {
            text.push_str(&format!(
                "      - {n} skipped, so nothing is claimed about them:\n"
            ));
            for s in &t.skips {
                text.push_str(&format!("      - {s}\n"));
            }
        }
        text.push_str(if mode.runs(Tier::Land) {
            "          Before you land: `tools/verify.sh`, whole-tree, with nothing skipped.\n"
        } else {
            "          `tools/verify.sh` is the merge contract. A release cut needs a --measure\n\
             \x20         run of the committed tree it cuts, green with nothing skipped.\n"
        });
        if let Some(base) = base {
            inherited_block(&mut text, &sorted, base);
        }
        return Verdict {
            text,
            exit: exit::PASS,
            claims_merge_contract: false,
        };
    }

    text.push_str(&format!(
        "  VERIFY: PASS (mode={mode_word} scope=workspace, 0 skipped{inherited_word}) — \
         {MERGE_CONTRACT_SENTENCE}\n"
    ));
    if let Some(base) = base {
        inherited_block(&mut text, &sorted, base);
    }
    Verdict {
        text,
        exit: exit::PASS,
        claims_merge_contract: true,
    }
}

/// DID THIS RUN MEASURE THE TREE? (2026-09-26) The one predicate behind a
/// receipt's `measured yes`, which the release cutter requires for the tree it
/// cuts — the MEASURE tier's counterpart of [`claims`], and as strict: the run
/// was whole-tree, every stage of [`plan::MEASURE_TIER`] is
/// in `specs`, each one's report decided something and decided only `ok` — no
/// skip, no finding, no could-not-run, nothing excused as inherited — and
/// neither the source nor the toolchain moved under the run (`moved`).
///
/// It asks the tier's OWN stages, never the run's verdict: a `--full` run
/// whose Kani floor skipped (a `--full`-only stage) still measured; one whose
/// paint row went red did not, whatever main's receipt would excuse.
/// `specs` and `reports` are the plan and its stage reports, index for index.
///
/// # Errors
/// Why the tree was not measured, in a few words.
pub fn measured(
    specs: &[plan::StageSpec],
    reports: &[crate::Report],
    scope: &Scope,
    moved: bool,
) -> Result<(), String> {
    if let Some(why) = scope.narrowing() {
        return Err(format!("the run was narrowed — {why}"));
    }
    for id in plan::MEASURE_TIER {
        let Some(at) = specs.iter().position(|s| s.id == id) else {
            return Err(format!("{id:?} was not planned"));
        };
        let title = &specs[at].title;
        let Some(report) = reports.get(at) else {
            return Err(format!("`{title}` left no report"));
        };
        let mut decided = false;
        for (outcome, label) in report.outcomes() {
            decided = true;
            match outcome {
                crate::Outcome::Ok => {}
                crate::Outcome::Skip => return Err(format!("`{title}` skipped: {label}")),
                crate::Outcome::Fail(crate::Severity::CouldNotRun) => {
                    return Err(format!("`{title}` could not run: {label}"));
                }
                crate::Outcome::Fail(crate::Severity::GateFailed) => {
                    return Err(format!("`{title}` FAILED: {label}"));
                }
            }
        }
        if !decided {
            return Err(format!("`{title}` decided nothing"));
        }
    }
    if moved {
        return Err("the source or the toolchain moved under the run".to_string());
    }
    Ok(())
}

/// The ladder's one line about [`measured`], printed above the verdict of
/// every run of the MEASURE tier.
#[must_use]
pub fn measured_line(m: &Result<(), String>) -> String {
    match m {
        Ok(()) => "verify: MEASURE tier — MEASURED: every MEASURE stage ran over the whole tree \
                   and was green\n"
            .to_string(),
        Err(why) => format!("verify: MEASURE tier — NOT MEASURED: {why}\n"),
    }
}

/// The could-not-runs a finding outranks, still named: fixing the finding
/// is otherwise followed by a run that decides LESS and looks like progress.
fn could_not_run_tail(text: &mut String, t: &Tally) {
    if t.could_not_run.is_empty() {
        return;
    }
    let m = t.could_not_run.len();
    text.push_str(&format!(
        "          ({m} more could not execute and decided nothing:\n"
    ));
    for c in &t.could_not_run {
        text.push_str(&format!("      - {c}\n"));
    }
    text.push_str("          fix those too, or the next run decides less than this one.)\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(fails: usize, cnr: usize, skips: &[&str]) -> Tally {
        let named =
            |n: usize, what: &str| (0..n).map(|i| format!("{what} {i}")).collect::<Vec<_>>();
        Tally {
            gate_failures: named(fails, "a finding"),
            could_not_run: named(cnr, "a stage that could not run"),
            skips: skips.iter().map(|s| (*s).to_string()).collect(),
            findings: Vec::new(),
            loads: Vec::new(),
        }
    }

    /// `t` plus one finding row itemized as `items`.
    fn with_row(mut t: Tally, label: &str, items: &[(&str, &str)]) -> Tally {
        let findings: Vec<Finding> = items
            .iter()
            .map(|(id, hash)| Finding {
                id: (*id).to_string(),
                hash: (*hash).to_string(),
                opaque: None,
                timing: None,
            })
            .collect();
        // Rows a caller named by hand come first and have no findings; the
        // itemized row is appended after them, in step with `findings`.
        t.findings.resize(t.gate_failures.len(), Vec::new());
        t.loads.resize(t.gate_failures.len(), None);
        t.record_with(
            crate::Outcome::Fail(crate::Severity::GateFailed),
            label,
            &findings,
        );
        t
    }

    const H: &str = "00000000000000aa";

    /// Main's receipt for the base, listing `(id, hash, red for this long)`.
    fn main_red(items: &[(&str, &str, u64)]) -> Against {
        const NOW: u64 = 2_000_000_000;
        Against::Base(differential::BaseReds {
            commit: "0".repeat(40),
            source: "receipt 000000000".to_string(),
            failures: items
                .iter()
                .map(|(id, hash, age)| crate::receipt::Failure {
                    id: (*id).to_string(),
                    hash: (*hash).to_string(),
                    since: "0".repeat(40),
                    since_when: NOW - age,
                })
                .collect(),
            now: NOW,
        })
    }

    /// THE TIMING LABEL NAMES AND NEVER EXCUSES (2026-09-26): a timing-shaped
    /// failure from a stage run at a load above half the cores is named with
    /// its marker and the load; the same failure on a quiet machine, a
    /// failure that is not timing-shaped, and a finding with no load reading
    /// are not; and the label changes neither the exit code nor the claim.
    #[test]
    fn a_timing_failure_on_a_loaded_machine_is_labeled_and_still_counts() {
        use crate::ladder::StageLoad;
        let finding = |id: &str, timing: Option<&'static str>| Finding {
            id: id.into(),
            hash: H.into(),
            opaque: None,
            timing,
        };
        let busy = StageLoad {
            start: 7482,
            end: 8010,
            cores: 14,
        };
        let quiet = StageLoad {
            start: 350,
            end: 690,
            cores: 14,
        };
        assert!(busy.loaded() && !quiet.loaded());
        assert!(
            StageLoad {
                start: 700,
                end: 701,
                cores: 14
            }
            .loaded(),
            "past half the cores at either end"
        );
        let mut t = Tally::default();
        let fail = crate::Outcome::Fail(crate::Severity::GateFailed);
        t.record_under(
            fail,
            "targo test --workspace --tests",
            &[
                finding("-p x --lib -- a::waits", Some("timeout")),
                finding("-p x --lib -- a::asserts", None),
            ],
            Some(busy),
        );
        t.record_under(
            fail,
            "gate quiet",
            &[finding("-p y --lib -- b::waits", Some("deadline"))],
            Some(quiet),
        );
        t.record_under(
            fail,
            "gate unread",
            &[finding("-p z --lib -- c::waits", Some("timed out"))],
            None,
        );
        let v = verdict(Mode::Fast, &Scope::workspace(), &t);
        assert_eq!(v.exit, exit::FAILED, "{}", v.text);
        assert!(!v.claims_merge_contract);
        assert!(
            v.text.contains(
                "          UNDER LOAD — a label, not an excuse: 1 failure(s) read as a clock \
                 running\n"
            ),
            "{}",
            v.text
        );
        assert!(
            v.text.contains(
                "      - -p x --lib -- a::waits (`timeout`; load 74.82 -> 80.10 on 14 cores)\n"
            ),
            "{}",
            v.text
        );
        for not in ["a::asserts", "b::waits (`", "c::waits (`"] {
            assert!(!v.text.contains(not), "{not} labeled:\n{}", v.text);
        }
        let calm = {
            let mut t = Tally::default();
            t.record_under(
                fail,
                "gate",
                &[finding("-p y --lib -- b::waits", Some("deadline"))],
                Some(quiet),
            );
            t
        };
        assert_eq!(under_load_block(&calm), "");
    }

    /// THE PROPERTY, JUDGED AGAINST MAIN. Over (scope, skips,
    /// could-not-runs, and how main's receipt sees the one red row), the
    /// sentence appears iff the run is whole-tree, complete, and its every red
    /// is inherited inside the cap. A red main lists with another hash, one
    /// past the cap, one main does not list, and a row nothing itemized all
    /// keep it red.
    #[test]
    fn against_main_the_sentence_appears_only_when_every_red_is_inherited() {
        let scopes = [Scope::workspace(), Scope::crate_only("aterm-grid")];
        let cap = differential::INHERITED_CAP_SECS;
        // (main's view — `(id, hash, red for this long)` — and is the row excused)
        type View<'a> = (&'a [(&'a str, &'a str, u64)], bool);
        let views: [View<'_>; 5] = [
            (&[("t", H, 60)], true),
            (&[("t", "00000000000000bb", 60)], false),
            (&[("t", H, cap + 1)], false),
            (&[("other", H, 60)], false),
            (&[], false),
        ];
        let mut claimed = 0;
        for scope in &scopes {
            for cnr in [0usize, 1] {
                for skips in [&[][..], &["a (x)"][..]] {
                    for (view, excused) in &views {
                        let t = with_row(tally(0, cnr, skips), "a gate", &[("t", H)]);
                        let against = main_red(view);
                        let v = verdict_against(Mode::Fast, scope, &t, &against);
                        let earned =
                            scope.is_workspace() && cnr == 0 && skips.is_empty() && *excused;
                        assert_eq!(v.claims_merge_contract, earned, "{view:?}\n{}", v.text);
                        assert_eq!(
                            v.text.contains(MERGE_CONTRACT_SENTENCE),
                            earned,
                            "{view:?}\n{}",
                            v.text
                        );
                        claimed += usize::from(earned);
                    }
                }
            }
        }
        assert_eq!(claimed, 1, "exactly the whole, complete, all-inherited run");
        // A row a caller put in `gate_failures` by hand has nothing to match.
        let by_hand = with_row(tally(1, 0, &[]), "a gate", &[("t", H)]);
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &by_hand,
            &main_red(&[("t", H, 60)]),
        );
        assert!(!v.claims_merge_contract);
        assert!(v.text.contains("1 new, 1 inherited"), "{}", v.text);
        assert!(
            v.text.contains("NEW — findings about the change")
                && v.text.contains("      - a finding 0\n"),
            "{}",
            v.text
        );
    }

    /// WHAT A JUDGED FAILURE SAYS: `N new, M inherited (red on main since
    /// <sha>)` and main's receipt, then the new reds (why each is new), the
    /// ones past the cap with their age, and the inherited ones — each named
    /// once, and exit 1.
    #[test]
    fn a_judged_failure_names_new_expired_and_inherited_reds_apart() {
        let t = with_row(
            tally(0, 0, &[]),
            "targo test --workspace --tests",
            &[
                ("-p x --test a -- new_one", H),
                ("-p x --test a -- changed", H),
                ("-p x --test a -- old_red", H),
                ("-p x --test a -- main_red", H),
            ],
        );
        let cap = differential::INHERITED_CAP_SECS;
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &t,
            &main_red(&[
                ("-p x --test a -- changed", "00000000000000cc", 60),
                ("-p x --test a -- old_red", H, cap + 7200),
                ("-p x --test a -- main_red", H, 3 * 3600),
            ]),
        );
        assert_eq!(v.exit, exit::FAILED);
        assert!(!v.claims_merge_contract);
        let text = &v.text;
        for want in [
            "VERIFY: FAIL (mode=fast scope=workspace) — DO NOT merge",
            "2 new, 1 inherited (red on main since 000000000), 1 red on main past the 24 h cap, \
             judged against main's receipt 000000000 for 000000000:",
            "      - -p x --test a -- new_one\n",
            "      - -p x --test a -- changed — red on main too, but failing differently\n",
            "PAST THE 24 h CAP",
            "      - -p x --test a -- old_red (red on main since 000000000, 26 h)\n",
            "INHERITED — red on main with the same failure",
            "      - -p x --test a -- main_red (red on main since 000000000, 3 h)\n",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
        assert!(!text.contains("this IS a finding about the"), "{text}");
    }

    /// WHAT A JUDGED PASS SAYS: the contract sentence, how many reds it
    /// inherited in the headline, and each one named below — exit 0, because
    /// nothing about the change failed.
    #[test]
    fn a_run_whose_every_red_is_inherited_claims_the_contract_and_names_them() {
        let t = with_row(
            with_row(tally(0, 0, &[]), "tippy lint", &[("tippy lint", H)]),
            "targo test",
            &[("-p x --lib -- flaky_on_main", "00000000000000bb")],
        );
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &t,
            &main_red(&[
                ("tippy lint", H, 7200),
                ("-p x --lib -- flaky_on_main", "00000000000000bb", 3600),
            ]),
        );
        assert!(v.claims_merge_contract, "{}", v.text);
        assert_eq!(v.exit, exit::PASS);
        assert!(
            v.text.contains(&format!(
                "VERIFY: PASS (mode=fast scope=workspace, 0 skipped, 2 inherited) — \
                 {MERGE_CONTRACT_SENTENCE}\n"
            )),
            "{}",
            v.text
        );
        assert!(
            v.text
                .contains("0 new, 2 inherited (red on main since 000000000), judged against"),
            "{}",
            v.text
        );
        assert!(
            v.text
                .contains("      - tippy lint (red on main since 000000000, 2 h)\n")
        );
        assert!(
            v.text.contains(
                "      - -p x --lib -- flaky_on_main (red on main since 000000000, 1 h)\n"
            )
        );
        // The same reds with a skip beside them: exit 0, no contract, still named.
        let skipped = Tally {
            skips: vec!["gui smoke (x)".to_string()],
            ..t
        };
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &skipped,
            &main_red(&[
                ("tippy lint", H, 7200),
                ("-p x --lib -- flaky_on_main", "00000000000000bb", 3600),
            ]),
        );
        assert!(!v.claims_merge_contract);
        assert_eq!(v.exit, exit::PASS);
        assert!(
            v.text
                .contains("(mode=fast scope=workspace, 1 skipped, 2 inherited) —")
        );
        assert!(
            v.text
                .contains("NOT the merge contract. Nothing new failed;")
        );
        assert!(
            !v.text.contains("Everything that ran was green"),
            "{}",
            v.text
        );
        assert!(v.text.contains("      - tippy lint (red on main since"));
    }

    /// NO BASE: the absolute rule, unchanged, and the reason there was no
    /// differential said once under the list.
    #[test]
    fn without_a_base_every_red_counts_and_the_verdict_says_why() {
        let t = with_row(tally(0, 0, &[]), "tippy lint", &[("tippy lint", H)]);
        let why = "no usable receipt for the base 123456789".to_string();
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &t,
            &Against::Absolute(Some(why.clone())),
        );
        assert_eq!(v.exit, exit::FAILED);
        assert!(v.text.contains("1 gate(s) decided AGAINST the tree"));
        assert!(v.text.contains("      - tippy lint\n"));
        assert!(
            v.text.contains(&format!("(No differential — {why}.")),
            "{}",
            v.text
        );
        assert_eq!(
            verdict(Mode::Fast, &Scope::workspace(), &t).text,
            v.text.replace(
                &format!(
                    "          (No differential — {why}. So every red counts, main's own included.)\n"
                ),
                ""
            ),
            "the reason is the only difference from a plain absolute verdict"
        );
    }

    /// THE property. Over the whole cross product of (mode, scope KIND, gate
    /// failures, could-not-runs, skips), the merge-contract sentence appears
    /// if and only if the run was whole-tree, complete and green.
    ///
    /// Every narrowing this gate can express is a column here — `--scope`, a
    /// `--changed` cone, and the empty `--changed` selection that builds nothing
    /// — because the sentence is the only thing a reader quotes as a landing
    /// licence, and a new tier is a fresh chance to print it by accident.
    #[test]
    fn the_merge_contract_sentence_appears_only_for_a_whole_green_run() {
        let modes = [Mode::Fast, Mode::Measure, Mode::Full];
        let scopes = [
            Scope::workspace(),
            Scope::crate_only("aterm-grid"),
            Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true),
            // The docs-only branch: a narrowing that compiled NOTHING, which is
            // the one that looks most like a clean whole-tree run in a ladder.
            Scope::changed("main", vec![], true),
        ];
        let skipsets: [&[&str]; 3] = [&[], &["tippy lint (absent)"], &["a (x)", "b (y)"]];
        let mut claimed = 0;
        let mut total = 0;
        for mode in modes {
            for scope in &scopes {
                for fails in [0usize, 1] {
                    for cnr in [0usize, 1] {
                        for skips in skipsets {
                            total += 1;
                            let t = tally(fails, cnr, skips);
                            let v = verdict(mode, scope, &t);
                            let earned = mode != Mode::Measure
                                && scope.is_workspace()
                                && fails == 0
                                && cnr == 0
                                && skips.is_empty();
                            assert_eq!(
                                v.claims_merge_contract, earned,
                                "claim bit wrong for mode={mode:?} scope={scope:?} \
                                 fails={fails} cnr={cnr} skips={skips:?}"
                            );
                            assert_eq!(
                                v.text.contains(MERGE_CONTRACT_SENTENCE),
                                earned,
                                "sentence leaked/missing for mode={mode:?} scope={scope:?} \
                                 fails={fails} cnr={cnr} skips={skips:?}\n{}",
                                v.text
                            );
                            if earned {
                                claimed += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(total, 3 * 4 * 2 * 2 * 3);
        assert_eq!(
            claimed, 2,
            "exactly the two whole-tree green runs of the LAND tier (fast, full); \
             --measure runs none of it"
        );
    }

    /// A `--changed` run forfeits the sentence EXACTLY as `--scope` does: same
    /// exit code, same "NOT the merge contract" block, a different reason line.
    #[test]
    fn a_change_scoped_run_cannot_print_the_merge_contract_sentence_either() {
        let scoped = verdict(
            Mode::Fast,
            &Scope::crate_only("aterm-grid"),
            &Tally::default(),
        );
        let cone = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        let v = verdict(Mode::Fast, &cone, &Tally::default());

        assert!(!v.claims_merge_contract);
        assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
        assert!(v.text.contains("NOT the merge contract"));
        assert!(v.text.contains("(mode=fast scope=changed:2, 0 skipped) —"));
        assert!(v.text.contains(
            "      - change-scoped against main to 2 crate(s) (aterm-grid aterm-gui): \
             the per-crate test, doctest and lint stages covered no other crate\n"
        ));
        assert_eq!(v.exit, scoped.exit, "narrow is not failure, in either tier");
        assert_eq!(v.claims_merge_contract, scoped.claims_merge_contract);

        // …and the empty selection is louder, not quieter, about proving nothing.
        let nothing = verdict(
            Mode::Fast,
            &Scope::changed("origin/main", vec![], true),
            &Tally::default(),
        );
        assert!(!nothing.claims_merge_contract);
        assert!(!nothing.text.contains(MERGE_CONTRACT_SENTENCE));
        assert!(nothing.text.contains(
            "      - change-scoped against origin/main and NO workspace crate changed: \
             the per-crate test, doctest and lint stages were skipped\n"
        ));
    }

    /// A `--changed` run that could NOT narrow is a whole-tree run, and must be
    /// allowed the claim: it widened, so it built and tested everything.
    #[test]
    fn a_widened_change_scoped_run_is_a_whole_tree_run_and_may_claim_it() {
        // `changed::stage_report` hands back `Scope::workspace()` for a widening,
        // which is the only reason this is reachable — and the reason widening is
        // safe: the verdict cannot tell it from a plain whole-tree run because
        // there is nothing to tell apart.
        let v = verdict(Mode::Fast, &Scope::workspace(), &Tally::default());
        assert!(v.claims_merge_contract);
        assert!(v.text.contains("scope=workspace"));
    }

    #[test]
    fn a_scoped_run_cannot_print_the_merge_contract_sentence() {
        // The regression this gate was caught committing: --scope over one crate
        // read as a landing licence for the whole tree.
        let v = verdict(
            Mode::Fast,
            &Scope::crate_only("aterm-grid"),
            &Tally::default(),
        );
        assert!(!v.claims_merge_contract);
        assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
        assert!(v.text.contains("NOT the merge contract"));
        assert!(v.text.contains(
            "- scoped to -p aterm-grid: the per-crate test, doctest and lint stages covered no \
             other crate"
        ));
        assert_eq!(
            v.exit,
            exit::PASS,
            "narrow is not failure — it is a smaller true claim"
        );
    }

    #[test]
    fn a_skipped_run_cannot_print_the_merge_contract_sentence_and_names_the_skips() {
        let t = tally(
            0,
            0,
            &[
                "tippy lint (Trust stage2 toolchain not built)",
                "gui smoke (macOS only)",
            ],
        );
        let v = verdict(Mode::Full, &Scope::workspace(), &t);
        assert!(!v.claims_merge_contract);
        assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
        assert!(v.text.contains("(mode=full scope=workspace, 2 skipped) —"));
        assert!(
            v.text
                .contains("      - 2 skipped, so nothing is claimed about them:")
        );
        // NAMED, not just counted: an unnamed skip is an invisible skip.
        assert!(
            v.text
                .contains("      - tippy lint (Trust stage2 toolchain not built)")
        );
        assert!(v.text.contains("      - gui smoke (macOS only)"));
    }

    /// The failure side of the skip rule above. A run is read top-to-bottom by
    /// a human or an agent, the ladder is tens of thousands of lines, and the
    /// verdict is the line they quote: a bare `VERIFY: FAIL` makes them grep
    /// for `^  FAIL` to learn what the gate actually decided.
    #[test]
    fn a_failing_run_names_the_gates_that_decided_against_the_tree() {
        let mut t = tally(0, 0, &[]);
        t.gate_failures = vec![
            "tippy --workspace -D warnings".to_string(),
            "license_check.sh".to_string(),
        ];
        t.could_not_run = vec!["libc-oracle/run.sh (no cc)".to_string()];
        let v = verdict(Mode::Fast, &Scope::workspace(), &t);
        assert_eq!(v.exit, exit::FAILED);
        assert!(!v.claims_merge_contract);
        assert!(
            v.text
                .contains("VERIFY: FAIL (mode=fast scope=workspace) — DO NOT merge")
        );
        assert!(v.text.contains("2 gate(s) decided AGAINST the tree"));
        assert!(v.text.contains("      - tippy --workspace -D warnings"));
        assert!(v.text.contains("      - license_check.sh"));
        // The could-not-runs a finding outranks are still named, or fixing the
        // finding is followed by a run that decides LESS and looks like progress.
        assert!(
            v.text
                .contains("(1 more could not execute and decided nothing:")
        );
        assert!(v.text.contains("      - libc-oracle/run.sh (no cc)"));
    }

    /// And the same obligation on the branch where nothing was decided at all.
    #[test]
    fn a_could_not_run_verdict_names_the_stages_that_never_ran() {
        let mut t = tally(0, 0, &[]);
        t.could_not_run = vec![
            "targo not found".to_string(),
            "gui smoke (no WindowServer session)".to_string(),
        ];
        let v = verdict(Mode::Fast, &Scope::workspace(), &t);
        assert_eq!(v.exit, exit::COULD_NOT_RUN);
        assert!(v.text.contains("      - targo not found"));
        assert!(
            v.text
                .contains("      - gui smoke (no WindowServer session)")
        );
    }

    #[test]
    fn the_whole_green_run_is_the_only_claim_and_says_so_exactly() {
        let v = verdict(Mode::Fast, &Scope::workspace(), &Tally::default());
        assert!(v.claims_merge_contract);
        assert_eq!(
            v.text,
            "\n=== verdict ===\n  VERIFY: PASS (mode=fast scope=workspace, 0 skipped) — merge contract satisfied\n\
             \x20         MEASURE tier: not part of the merge contract, not run here; a release cut\n\
             \x20         needs it green (`tools/verify.sh --measure`):\n\
             \x20     - conformance release artifact (paint/spin build it otherwise)\n\
             \x20     - measuring tests (--workspace; run alone)\n\
             \x20     - gui typing-pacing smoke\n"
        );
        assert_eq!(v.exit, exit::PASS);
    }

    /// MEASURED MEANS EVERY MEASURE STAGE, WHOLE-TREE, OK AND NOTHING ELSE
    /// (2026-09-26). The real plans: `--measure` and `--full` with every stage
    /// green measured; one MEASURE row skipped, red or could-not-run, a MEASURE
    /// stage that recorded nothing, a narrowed scope, a moved tree and the
    /// default plan (which has no MEASURE stage) did not — each
    /// saying why. A `--full`-only stage that skipped (the Kani floor with
    /// trust-mc absent) costs nothing: the question is the tier's.
    #[test]
    fn a_run_measured_the_tree_only_when_every_measure_stage_ran_whole_and_green() {
        use crate::Report;
        fn ctx(mode: Mode, scope: Scope) -> crate::Ctx {
            crate::Ctx::new(
                std::path::PathBuf::from("/repo"),
                mode,
                scope,
                crate::EnvSnapshot::default(),
                std::path::PathBuf::from("/tmp"),
            )
        }
        /// How the one bad MEASURE stage decides.
        type Decide = fn(&mut Report);
        let runs = |c: &crate::Ctx, bad: Option<(plan::StageId, Decide)>| {
            let specs = plan::plan(c);
            let reports: Vec<Report> = specs
                .iter()
                .map(|s| {
                    let mut r = Report::new(s.title.clone());
                    match bad {
                        Some((id, f)) if id == s.id => f(&mut r),
                        _ if s.id == plan::StageId::KaniFloor => r.skip("trust-mc (absent)"),
                        _ => r.pass("did the thing"),
                    }
                    r
                })
                .collect();
            (specs, reports)
        };
        let ws = Scope::workspace();
        for mode in [Mode::Measure, Mode::Full] {
            let c = ctx(mode, ws.clone());
            let (specs, reports) = runs(&c, None);
            assert_eq!(measured(&specs, &reports, &ws, false), Ok(()), "{mode:?}");
            assert!(measured_line(&Ok(())).contains("MEASURED"), "{mode:?}");
            let moved = measured(&specs, &reports, &ws, true);
            assert_eq!(
                moved,
                Err("the source or the toolchain moved under the run".to_string())
            );
            let cases: [(Decide, &str); 4] = [
                (|r| r.skip("gui smoke (no WindowServer session)"), "skipped"),
                (|r| r.fail("paint row"), "FAILED"),
                (|r| r.cannot_run("paint UNPROVED"), "could not run"),
                (|_| {}, "decided nothing"),
            ];
            for id in plan::MEASURE_TIER {
                for (f, word) in cases {
                    let (specs, reports) = runs(&c, Some((id, f)));
                    let why = measured(&specs, &reports, &ws, false)
                        .expect_err("one bad MEASURE stage is not measured");
                    assert!(why.contains(word), "{mode:?} {id:?}: {why}");
                    assert!(
                        measured_line(&Err(why.clone())).contains("NOT MEASURED"),
                        "{why}"
                    );
                }
            }
        }
        let narrowed = Scope::crate_only("aterm-conformance");
        let c = ctx(Mode::Measure, narrowed.clone());
        let (specs, reports) = runs(&c, None);
        assert!(
            measured(&specs, &reports, &narrowed, false)
                .is_err_and(|why| why.starts_with("the run was narrowed")),
        );
        let c = ctx(Mode::Fast, ws.clone());
        let (specs, reports) = runs(&c, None);
        assert!(
            measured(&specs, &reports, &ws, false)
                .is_err_and(|why| why.ends_with("was not planned")),
            "the default plan has no MEASURE stage to have measured with"
        );
    }

    /// WHAT A TIER LEAVES OUT IS SAID, AND A TIER THAT IS NOT THE CONTRACT
    /// CANNOT CLAIM IT (2026-09-26). The default run names every MEASURE stage
    /// under `MEASURE tier: not part of the merge contract` in every verdict it
    /// can reach — green, red, could-not-run, narrowed (without a release prime
    /// its scope would not build). A `--measure` run, green and whole-tree, exits
    /// 0 and is refused the sentence, saying it ran the MEASURE tier alone; a
    /// `--full` run runs the tier, so it names nothing left out and claims the
    /// contract when green.
    #[test]
    fn the_default_names_the_measure_tier_and_a_measure_run_cannot_claim_the_contract() {
        const LINE: &str = "MEASURE tier: not part of the merge contract";
        let ws = Scope::workspace();
        for (what, t) in [
            ("green", tally(0, 0, &[])),
            ("red", tally(1, 0, &[])),
            ("could not run", tally(0, 1, &[])),
            ("skipped", tally(0, 0, &["a (x)"])),
        ] {
            let v = verdict(Mode::Fast, &ws, &t);
            assert!(v.text.contains(LINE), "{what}:\n{}", v.text);
            for title in plan::tier_titles(&ws, Tier::Measure) {
                assert!(
                    v.text.contains(&format!("      - {title}\n")),
                    "{what}: {title}"
                );
            }
            let full = verdict(Mode::Full, &ws, &t);
            assert!(
                !full.text.contains(LINE),
                "{what}: --full ran it:\n{}",
                full.text
            );
            assert_eq!(
                (full.exit, full.claims_merge_contract),
                (v.exit, v.claims_merge_contract),
                "{what}: the tiers change what is named, not how a tally is judged"
            );
        }
        let narrowed = verdict(
            Mode::Fast,
            &Scope::crate_only("aterm-grid"),
            &Tally::default(),
        );
        assert!(narrowed.text.contains(LINE), "{}", narrowed.text);
        assert!(
            !narrowed.text.contains("conformance release artifact"),
            "a scope that builds no release prime names none: {}",
            narrowed.text
        );

        let measured = verdict(Mode::Measure, &ws, &Tally::default());
        assert_eq!(measured.exit, exit::PASS);
        assert!(!measured.claims_merge_contract);
        assert!(
            !measured.text.contains(MERGE_CONTRACT_SENTENCE),
            "{}",
            measured.text
        );
        assert!(!measured.text.contains(LINE), "{}", measured.text);
        for want in [
            "VERIFY: PASS (mode=measure scope=workspace, 0 skipped) —",
            "NOT the merge contract",
            "      - --measure ran the MEASURE tier alone: no stage of the merge contract (the \
             LAND tier) ran\n",
            "`tools/verify.sh` is the merge contract. A release cut needs a --measure\n\
             \x20         run of the committed tree it cuts, green with nothing skipped.\n",
        ] {
            assert!(
                measured.text.contains(want),
                "missing {want:?}:\n{}",
                measured.text
            );
        }
        assert!(
            !measured.text.contains("Before you land:"),
            "{}",
            measured.text
        );
        let red = verdict(Mode::Measure, &ws, &tally(1, 0, &[]));
        assert_eq!(red.exit, exit::FAILED, "a red measurement is a finding");
        assert!(
            red.text
                .contains("VERIFY: FAIL (mode=measure scope=workspace)")
        );
    }

    #[test]
    fn a_gate_finding_exits_one_and_outranks_a_broken_environment() {
        let v = verdict(Mode::Fast, &Scope::workspace(), &tally(1, 4, &[]));
        assert_eq!(v.exit, exit::FAILED);
        assert!(
            v.text
                .contains("VERIFY: FAIL (mode=fast scope=workspace) — DO NOT merge")
        );
        assert!(!v.text.contains("COULD NOT RUN"));
    }

    #[test]
    fn a_broken_environment_alone_exits_three_and_claims_no_finding() {
        let v = verdict(Mode::Fast, &Scope::workspace(), &tally(0, 2, &[]));
        assert_eq!(v.exit, exit::COULD_NOT_RUN);
        assert!(
            v.text
                .contains("VERIFY: COULD NOT RUN (mode=fast scope=workspace) — DO NOT merge")
        );
        assert!(v.text.contains(
            "          2 could not execute — no verdict; one that could not launch may be a \
             crash in the change:\n"
        ));
        assert!(
            !v.text.contains("NOT a verdict on your change"),
            "{}",
            v.text
        );
        assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
    }

    #[test]
    fn a_skip_is_never_silently_a_pass() {
        // Green-but-skipped and green-and-complete must not render the same.
        let complete = verdict(Mode::Fast, &Scope::workspace(), &Tally::default());
        let skipped = verdict(
            Mode::Fast,
            &Scope::workspace(),
            &tally(0, 0, &["one (absent)"]),
        );
        assert_ne!(complete.text, skipped.text);
        assert_eq!(
            complete.exit, skipped.exit,
            "both are exit 0 — the words carry the difference"
        );
        assert!(complete.claims_merge_contract && !skipped.claims_merge_contract);
    }
}
