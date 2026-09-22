// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ladder: `ok` / `FAIL` / `skip`, one line per decision, grouped under a
//! `=== stage ===` header so a whole run is scannable in one screenful.
//!
//! The vocabulary is not decoration. Every reviewer instruction, every process
//! doc and every agent prompt in this repo teaches the same three words, and a
//! skip is an honest "tool absent" — NEVER a silent pass. A run that skipped a
//! gating stage has not discharged the merge contract, so skips are counted AND
//! NAMED here, and [`crate::verdict`] downgrades the claim accordingly.

use crate::exec::Run;

/// Why a stage failed — the distinction `.githooks/pre-push` already draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// A real finding about the tree: a lint, a test, a guard, a proof.
    GateFailed,
    /// Nothing was decided: no driver, no helper script, no temp dir. The ladder
    /// still says `FAIL` (fail-closed), but the exit code says COULD NOT RUN so a
    /// caller cannot confuse a broken machine with a broken change.
    CouldNotRun,
}

/// One ladder decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Ok,
    Skip,
    Fail(Severity),
}

impl Outcome {
    /// The exact eight-column prefix of `tools/verify.sh`: `'  ok    '`,
    /// `'  skip  '`, `'  FAIL  '`.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Outcome::Ok => "  ok    ",
            Outcome::Skip => "  skip  ",
            Outcome::Fail(_) => "  FAIL  ",
        }
    }
}

/// A line of stage output: either a ladder decision or verbatim text (a NOTICE,
/// a captured child log). Keeping both in one ordered list preserves the script's
/// interleaving — a build's output still appears above the `FAIL` it explains.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Entry {
    Ladder { outcome: Outcome, label: String },
    Raw(String),
}

/// One stage's complete output, buffered so that concurrently-run stages still
/// print in the ladder's declared order.
#[derive(Clone, Debug)]
pub struct Report {
    pub title: String,
    pub entries: Vec<Entry>,
}

impl Report {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            entries: Vec::new(),
        }
    }

    pub fn pass(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Ok,
            label: label.into(),
        });
    }

    pub fn skip(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Skip,
            label: label.into(),
        });
    }

    /// A gate decided against the tree.
    pub fn fail(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Fail(Severity::GateFailed),
            label: label.into(),
        });
    }

    /// The stage could not execute. Still a `FAIL` line: fail-closed is the rule,
    /// and a missing tool that a stage NEEDED is not an honest skip.
    pub fn cannot_run(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Fail(Severity::CouldNotRun),
            label: label.into(),
        });
    }

    /// A child that did not pass, as the FAIL row of the severity its failure
    /// HAS: COULD NOT RUN when it never spawned or died of the machine
    /// ([`Run::environment_failure`] — a full disk, measured 2026-09-20), a
    /// finding otherwise.
    ///
    /// WHAT THIS COVERS, stated narrowly because the sentence here was once
    /// wider than the code (corrected 2026-09-21). Every stage that decides a
    /// row from a child's EXIT STATUS comes through here or
    /// [`Report::child_could_not_run`]: the script stages, the libc oracle,
    /// the cells and kani gates, and the nine drive binaries, whose own exit
    /// contracts cannot see a full disk — `libc-oracle/run.sh` maps a cargo
    /// child that died of ENOSPC (exit 101, since targo never answers 3) onto
    /// a plain failure, which arrived here as a FINDING about the tree until
    /// the guard was added. So an ENOSPC on any of those is no longer counted
    /// against the tree or written into its receipt as `verdict FAIL`.
    ///
    /// What does NOT come through here is a row decided from a running
    /// process's ANSWER rather than its exit status — the smoke stages' socket
    /// replies, metrics readings and frame checks. A child that never spawned
    /// there was already reported by the stage that launched it, and a reply
    /// that is wrong is a finding whatever the disk is doing.
    pub fn fail_child(&mut self, run: &Run, label: impl Into<String>) {
        let label = label.into();
        if !self.child_could_not_run(run, &label) {
            self.fail(label);
        }
    }

    /// [`Report::decide`] from a child: `ok` on success, else [`Report::fail_child`].
    pub fn decide_child(&mut self, run: &Run, label: impl Into<String>) {
        if run.ok {
            self.pass(label);
        } else {
            self.fail_child(run, label);
        }
    }

    /// The environment's failure, if this child had one, recorded as a COULD
    /// NOT RUN row under `label`: `true` when it was. The stages whose child
    /// says MORE than pass/fail ask this before reading the output, so an
    /// ENOSPC never reaches their own verdict logic.
    pub fn child_could_not_run(&mut self, run: &Run, label: &str) -> bool {
        match run.environment_failure() {
            Some(why) => {
                self.cannot_run(format!("{label} — could not run: {why}"));
                true
            }
            None => false,
        }
    }

    /// Verbatim output (child logs, NOTICE lines). Trailing newlines are trimmed;
    /// the renderer supplies exactly one.
    pub fn raw(&mut self, text: impl Into<String>) {
        let text = text.into();
        let trimmed = text.trim_end_matches('\n');
        if !trimmed.is_empty() {
            self.entries.push(Entry::Raw(trimmed.to_string()));
        }
    }

    /// Record an already-computed outcome — for the stages whose child reports
    /// more than pass/fail, so the mapping stays a pure, testable function
    /// instead of control flow spread through the stage.
    pub fn record(&mut self, outcome: Outcome, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome,
            label: label.into(),
        });
    }

    /// Record an outcome by boolean, the shape most ported stages want.
    pub fn decide(&mut self, ok: bool, label: impl Into<String>) {
        if ok {
            self.pass(label);
        } else {
            self.fail(label);
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut s = format!("\n=== {} ===\n", self.title);
        for e in &self.entries {
            match e {
                Entry::Ladder { outcome, label } => {
                    s.push_str(outcome.tag());
                    s.push_str(label);
                    s.push('\n');
                }
                Entry::Raw(text) => {
                    s.push_str(text);
                    s.push('\n');
                }
            }
        }
        s
    }

    /// Ladder outcomes only, for tests and accounting.
    pub fn outcomes(&self) -> impl Iterator<Item = (Outcome, &str)> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Ladder { outcome, label } => Some((*outcome, label.as_str())),
            Entry::Raw(_) => None,
        })
    }
}

/// The accounting the verdict is computed from: the NAME of every finding,
/// every could-not-run and every skip.
///
/// ALL THREE ARE NAMED, for one reason (2026-09-17). The skips were named from
/// the start because "3 stages were skipped" is how a skipped gate becomes
/// invisible — and a bare failure count is the same sentence with a worse
/// ending. MEASURED on the `--fast` run of ca7e3dbfe: one stage was red, the
/// verdict said `VERIFY: FAIL (mode=fast scope=workspace) — DO NOT merge` and
/// nothing else, and finding WHICH stage meant grepping 30,713 lines of ladder
/// for `^  FAIL`. The row is right there in the stage's own block; the verdict
/// is the line a reader quotes, so it says the names too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Findings, named in ladder order: a gate decided against the tree.
    pub gate_failures: Vec<String>,
    /// Stages that decided NOTHING, named in ladder order.
    pub could_not_run: Vec<String>,
    /// Named, in ladder order. The verdict prints these — "3 stages were skipped"
    /// without saying which is how a skipped gate becomes invisible.
    pub skips: Vec<String>,
}

impl Tally {
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.skips.len()
    }

    #[must_use]
    pub fn failed(&self) -> bool {
        !self.gate_failures.is_empty() || !self.could_not_run.is_empty()
    }

    /// Add one decision.
    pub fn record(&mut self, outcome: Outcome, label: &str) {
        match outcome {
            Outcome::Ok => {}
            Outcome::Skip => self.skips.push(label.to_string()),
            Outcome::Fail(Severity::GateFailed) => self.gate_failures.push(label.to_string()),
            Outcome::Fail(Severity::CouldNotRun) => self.could_not_run.push(label.to_string()),
        }
    }
}

/// Fold every stage report into the run's accounting.
#[must_use]
pub fn tally(reports: &[Report]) -> Tally {
    let mut t = Tally::default();
    for r in reports {
        for (outcome, label) in r.outcomes() {
            t.record(outcome, label);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_columns_match_the_script_byte_for_byte() {
        let mut r = Report::new("build (--workspace)");
        r.pass("targo build --workspace");
        r.skip("tippy lint (selftest: not executed)");
        r.fail("license_check.sh");
        r.cannot_run("targo not found");
        assert_eq!(
            r.render(),
            "\n=== build (--workspace) ===\n\
             \x20 ok    targo build --workspace\n\
             \x20 skip  tippy lint (selftest: not executed)\n\
             \x20 FAIL  license_check.sh\n\
             \x20 FAIL  targo not found\n"
        );
    }

    #[test]
    fn a_could_not_run_still_prints_fail_never_skip() {
        // Fail-closed: the only difference between the two failure severities is
        // the exit code, never the word on the ladder.
        assert_eq!(
            Outcome::Fail(Severity::CouldNotRun).tag(),
            Outcome::Fail(Severity::GateFailed).tag()
        );
        assert_ne!(
            Outcome::Fail(Severity::CouldNotRun).tag(),
            Outcome::Skip.tag()
        );
    }

    #[test]
    fn raw_entries_interleave_where_the_stage_put_them() {
        let mut r = Report::new("trust-mc / Kani BMC floor (config-free parser harnesses)");
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN\n");
        r.skip("trust-mc / Kani BMC floor (tool unavailable; pending build)");
        assert_eq!(
            r.render(),
            "\n=== trust-mc / Kani BMC floor (config-free parser harnesses) ===\n\
             \x20 NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN\n\
             \x20 skip  trust-mc / Kani BMC floor (tool unavailable; pending build)\n"
        );
    }

    #[test]
    fn every_skip_is_counted_and_named() {
        let mut a = Report::new("a");
        a.skip("targo test (no targo)");
        a.pass("something real");
        let mut b = Report::new("b");
        b.skip("gui smoke (macOS only)");
        b.fail("a finding");
        b.cannot_run("no driver");

        let t = tally(&[a, b]);
        assert_eq!(t.skipped(), 2);
        assert_eq!(t.skips, ["targo test (no targo)", "gui smoke (macOS only)"]);
        assert_eq!(t.gate_failures, ["a finding"]);
        assert_eq!(t.could_not_run, ["no driver"]);
        assert!(t.failed());
    }

    #[test]
    fn a_clean_run_tallies_to_nothing() {
        let mut a = Report::new("a");
        a.pass("x");
        a.pass("y");
        let t = tally(&[a]);
        assert_eq!(t, Tally::default());
        assert!(!t.failed());
        assert_eq!(t.skipped(), 0);
    }

    /// THE ROW A DYING CHILD LEAVES. A child that ran and failed is a finding
    /// (`FAIL`, exit 1); one that never spawned or ran out of disk is COULD NOT
    /// RUN — the same `FAIL` word on the ladder, the other severity in the
    /// tally, and so `verdict COULD-NOT-RUN` in the receipt rather than a
    /// judgement of the tree.
    #[test]
    fn a_child_that_died_of_the_machine_is_could_not_run_and_one_that_failed_is_a_finding() {
        let run = |ok: bool, output: &str, spawn: Option<&str>| Run {
            ok,
            output: output.into(),
            code: None,
            spawn_error: spawn.map(str::to_string),
        };
        let mut r = Report::new("test (--workspace)");
        r.decide_child(&run(true, "", None), "passed");
        r.decide_child(&run(false, "error[E0308]", None), "a finding");
        r.decide_child(
            &run(
                false,
                "aterm-verify: cannot run /s2/targo: No space left on device (os error 28)",
                Some("No space left on device (os error 28)"),
            ),
            "never spawned",
        );
        r.fail_child(
            &run(
                false,
                "error: failed to write lib.rmeta: No space left on device (os error 28)",
                None,
            ),
            "starved",
        );
        let mut asked = Report::new("cells");
        assert!(!asked.child_could_not_run(&run(false, "no", None), "gate cells"));
        assert!(asked.child_could_not_run(
            &run(false, "x: No space left on device", None),
            "gate cells"
        ));

        let t = tally(&[r, asked]);
        assert_eq!(t.gate_failures, ["a finding"]);
        assert_eq!(
            t.could_not_run,
            [
                "never spawned — could not run: No space left on device (os error 28)",
                "starved — could not run: the child ran out of disk (No space left on device)",
                "gate cells — could not run: the child ran out of disk (No space left on device)",
            ]
        );
        assert_eq!(t.skipped(), 0);
    }

    #[test]
    fn raw_text_never_becomes_a_skip() {
        // A NOTICE line explains a skip; it must not be counted as one, or the
        // verdict would name the same absence twice and inflate the count.
        let mut r = Report::new("r");
        r.raw("  NOTICE: something");
        assert_eq!(tally(&[r]), Tally::default());
    }
}
