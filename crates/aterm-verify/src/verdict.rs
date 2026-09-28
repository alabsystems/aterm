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
//! The scope axis is the SCOPE KIND, not a flag: `--scope`, a `--changed` cone
//! and an empty `--changed` selection are three columns of the same matrix, and
//! any future narrowing joins it by being a [`Scope`] variant that is not
//! `Workspace`. That is why the predicate asks [`Scope::is_workspace`] rather
//! than asking which flag the caller passed — a tier cannot be added without
//! answering the question.

use crate::cli::Mode;
use crate::exit;
use crate::ladder::Tally;
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

/// Is this run entitled to the merge-contract sentence?
///
/// The contract is the WHOLE-TREE run with nothing skipped and nothing failed.
/// Four independent ways to lose it, and the caller cannot forget one because
/// this is the only predicate [`verdict`] consults:
///  * the run was NARROWED — `--scope` to one crate, or `--changed` to the
///    diff's reverse-dependency cone. The question is the SCOPE KIND, never the
///    flag that produced it, so a new narrowing forfeits the claim by existing
///    rather than by remembering to;
///  * a stage was skipped, so nothing is claimed about it;
///  * a gate failed;
///  * the environment was broken (nothing was decided).
#[must_use]
pub fn discharges_merge_contract(scope: &Scope, t: &Tally) -> bool {
    scope.is_workspace() && t.skipped() == 0 && !t.failed()
}

/// Render the verdict block and decide the exit code.
#[must_use]
pub fn verdict(mode: Mode, scope: &Scope, t: &Tally) -> Verdict {
    let mut text = String::from("\n=== verdict ===\n");
    let scope_word = scope.desc();
    let mode = mode.as_str();

    // NAMED, for the reason `Tally` gives: the ladder row that decided the run is
    // one line in tens of thousands, and the verdict is the line a reader quotes.
    if !t.gate_failures.is_empty() {
        let n = t.gate_failures.len();
        text.push_str(&format!(
            "  VERIFY: FAIL (mode={mode} scope={scope_word}) — DO NOT merge\n"
        ));
        text.push_str(&format!(
            "          {n} gate(s) decided AGAINST the tree — this IS a finding about the\n"
        ));
        text.push_str("          change, and the stage's own block above says why:\n");
        for f in &t.gate_failures {
            text.push_str(&format!("      - {f}\n"));
        }
        if !t.could_not_run.is_empty() {
            let m = t.could_not_run.len();
            text.push_str(&format!(
                "          ({m} further stage(s) could not execute and decided nothing:\n"
            ));
            for c in &t.could_not_run {
                text.push_str(&format!("      - {c}\n"));
            }
            text.push_str(
                "          fix those too, or the next run decides less than this one.)\n",
            );
        }
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
            "  VERIFY: COULD NOT RUN (mode={mode} scope={scope_word}) — DO NOT merge\n"
        ));
        text.push_str(&format!(
            "          {n} stage(s) could not execute, so the gate reached no verdict on\n"
        ));
        text.push_str(
            "          them. This is NOT a finding about your change — the environment\n",
        );
        text.push_str("          is broken (no driver, or a helper the gate needs is missing).\n");
        for c in &t.could_not_run {
            text.push_str(&format!("      - {c}\n"));
        }
        text.push_str(
            "          Fix it and run again; a gate that never ran has decided nothing.\n",
        );
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
    if !discharges_merge_contract(scope, t) {
        let n = t.skipped();
        text.push_str(&format!(
            "  VERIFY: PASS (mode={mode} scope={scope_word}, {n} skipped) —\n"
        ));
        text.push_str(
            "          NOT the merge contract. Everything that ran was green; the contract\n",
        );
        text.push_str(
            "          is the WHOLE-TREE run with nothing skipped, and this run was narrower:\n",
        );
        if let Some(why) = scope.narrowing() {
            text.push_str(&format!("      - {why}\n"));
        }
        if n != 0 {
            text.push_str(&format!(
                "      - {n} stage(s) did not run, so nothing is claimed about them:\n"
            ));
            for s in &t.skips {
                text.push_str(&format!("      - {s}\n"));
            }
        }
        text.push_str(
            "          Re-run whole-tree, with the missing tools installed, before you land.\n",
        );
        return Verdict {
            text,
            exit: exit::PASS,
            claims_merge_contract: false,
        };
    }

    text.push_str(&format!(
        "  VERIFY: PASS (mode={mode} scope=workspace, 0 skipped) — {MERGE_CONTRACT_SENTENCE}\n"
    ));
    Verdict {
        text,
        exit: exit::PASS,
        claims_merge_contract: true,
    }
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
        }
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
        let modes = [Mode::Fast, Mode::Full];
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
                            let earned =
                                scope.is_workspace() && fails == 0 && cnr == 0 && skips.is_empty();
                            assert_eq!(
                                v.claims_merge_contract, earned,
                                "claim bit wrong for scope={scope:?} fails={fails} cnr={cnr} \
                                 skips={skips:?}"
                            );
                            assert_eq!(
                                v.text.contains(MERGE_CONTRACT_SENTENCE),
                                earned,
                                "sentence leaked/missing for scope={scope:?} fails={fails} \
                                 cnr={cnr} skips={skips:?}\n{}",
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
        assert_eq!(total, 2 * 4 * 2 * 2 * 3);
        assert_eq!(
            claimed, 2,
            "exactly the two whole-tree green runs (fast, full)"
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
             every other workspace crate was not built or tested\n"
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
             nothing was built or tested\n"
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
            "- scoped to -p aterm-grid: the rest of the workspace was not built or tested"
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
                .contains("      - 2 stage(s) did not run, so nothing is claimed about them:")
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
        assert!(v.text.contains("1 further stage(s) could not execute"));
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
            "\n=== verdict ===\n  VERIFY: PASS (mode=fast scope=workspace, 0 skipped) — merge contract satisfied\n"
        );
        assert_eq!(v.exit, exit::PASS);
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
        assert!(v.text.contains("NOT a finding about your change"));
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
