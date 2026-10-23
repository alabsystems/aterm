// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ladder: `ok` / `FAIL` / `NOT RUN` / `skip`, one row per decision, under a
//! `=== stage ===` header. A skip names what was absent and is never a silent
//! pass; a stage that could not run is never mistaken for a finding.

use std::ffi::OsString;

use crate::exec::Run;
use crate::findings;

/// One failure: what failed, the command that reruns just it, and the lines of
/// its output that say what went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// A failed test (`-p atpkg --test atpkg_suite -- <name>`) or, when the row
    /// cannot be itemized, the row's own label.
    pub id: String,
    /// The exact command that reruns this finding alone, when there is one
    /// narrower than rerunning its check.
    pub rerun: Option<String>,
    /// What it printed that matters, a few lines at most.
    pub message: Vec<String>,
}

/// Why a stage failed: exit `1` against exit `3` ([`crate::exit`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// A real finding about the tree: a lint, a test, a guard, a proof.
    GateFailed,
    /// Nothing was decided: no driver, no helper script, no disk. Never green,
    /// and never a finding either.
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
    /// The row's prefix.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Outcome::Ok => "  ok    ",
            Outcome::Skip => "  skip  ",
            Outcome::Fail(Severity::GateFailed) => "  FAIL  ",
            Outcome::Fail(Severity::CouldNotRun) => "  NOT RUN ",
        }
    }
}

/// A line of stage output: a ladder decision or verbatim text (a child's log),
/// in the order the stage produced them, so a build's output sits above the
/// `FAIL` it explains.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Entry {
    Ladder {
        outcome: Outcome,
        label: String,
        /// A finding's itemized failures; empty for every other row, and for a
        /// finding decided without a child ([`Report::findings`] itemizes it).
        findings: Vec<Finding>,
    },
    Raw(String),
}

/// One stage's complete output, buffered so concurrently run stages still
/// print in declared order.
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
        self.record(Outcome::Ok, label);
    }

    pub fn skip(&mut self, label: impl Into<String>) {
        self.record(Outcome::Skip, label);
    }

    /// A finding the stage decided by itself; a log written under it with
    /// [`Report::raw`] is its message.
    pub fn fail(&mut self, label: impl Into<String>) {
        self.record(Outcome::Fail(Severity::GateFailed), label);
    }

    /// The stage could not execute: a `NOT RUN` row and exit 3.
    pub fn cannot_run(&mut self, label: impl Into<String>) {
        self.record(Outcome::Fail(Severity::CouldNotRun), label);
    }

    /// A finding the stage decided by itself, itemized: each of `findings`
    /// is printed under the check's line.
    pub fn fail_with_findings(&mut self, label: impl Into<String>, findings: Vec<Finding>) {
        self.fail_with(label.into(), findings);
    }

    fn fail_with(&mut self, label: String, findings: Vec<Finding>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Fail(Severity::GateFailed),
            label,
            findings,
        });
    }

    /// A child that did not pass: COULD NOT RUN when it never spawned or died of
    /// the machine ([`Run::environment_failure`]), a finding read from what it
    /// printed otherwise.
    pub fn fail_child(&mut self, run: &Run, label: impl Into<String>) {
        let label = label.into();
        if !self.child_could_not_run(run, &label) {
            let finding = findings::row_finding(&label, &run.output);
            self.fail_with(label, vec![finding]);
        }
    }

    /// [`Report::fail_child`] for a `targo test` child that ran under `env`:
    /// one finding per failed test when the log accounts for every one
    /// ([`findings::test_findings`]), and a COULD NOT RUN row for the tests
    /// beside them that refused the machine ([`findings::split_test_findings`]):
    /// a host with no fish never reads as a failure of the tree.
    pub fn fail_test_child(
        &mut self,
        run: &Run,
        label: impl Into<String>,
        env: &[(OsString, OsString)],
    ) {
        let label = label.into();
        if !self.child_could_not_run(run, &label) {
            let (found, refused) = findings::split_test_findings(&label, &run.output, env);
            if !refused.is_empty() {
                let who = format!("{} of its failing tests", refused.len());
                self.cannot_run(format!(
                    "{label} — {}",
                    crate::libtest::refusal_reason(&who, &refused, &run.output)
                ));
            }
            self.fail_with(label, found);
        }
    }

    /// [`Report::decide_child`] for a `targo test` child that ran under `env`
    /// ([`findings::rerun_env`]). A pass that ran no test is no pass: a runner
    /// from the caller's cargo config can outrank the recording one and run
    /// nothing, exiting 0 ([`crate::libtest::ran_no_test`]).
    pub fn decide_test_child(
        &mut self,
        run: &Run,
        label: impl Into<String>,
        env: &[(OsString, OsString)],
    ) {
        let label = label.into();
        if run.ok && crate::libtest::ran_no_test(&run.output) {
            self.cannot_run(format!("{label} — {}", crate::libtest::RAN_NO_TEST));
        } else if run.ok {
            self.pass(label);
        } else {
            self.fail_test_child(run, label, env);
        }
    }

    /// `ok` on success, else [`Report::fail_child`].
    pub fn decide_child(&mut self, run: &Run, label: impl Into<String>) {
        if run.ok {
            self.pass(label);
        } else {
            self.fail_child(run, label);
        }
    }

    /// The environment's failure, if this child had one, recorded as a COULD
    /// NOT RUN row under `label`: `true` when it was.
    pub fn child_could_not_run(&mut self, run: &Run, label: &str) -> bool {
        match run.environment_failure() {
            Some(why) => {
                self.cannot_run(format!("{label} — {why}"));
                true
            }
            None => false,
        }
    }

    /// Verbatim output. Trailing newlines are trimmed; the renderer supplies one.
    pub fn raw(&mut self, text: impl Into<String>) {
        let text = text.into();
        let trimmed = text.trim_end_matches('\n');
        if !trimmed.is_empty() {
            self.entries.push(Entry::Raw(trimmed.to_string()));
        }
    }

    /// Record an already-computed outcome, for the stages whose child reports
    /// more than pass/fail.
    pub fn record(&mut self, outcome: Outcome, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome,
            label: label.into(),
            findings: Vec::new(),
        });
    }

    /// [`Report::record`] for an outcome read from `run`'s exit code: a finding
    /// carries what the child printed.
    pub fn record_child(&mut self, run: &Run, outcome: Outcome, label: impl Into<String>) {
        let label = label.into();
        if outcome == Outcome::Fail(Severity::GateFailed) {
            let finding = findings::row_finding(&label, &run.output);
            self.fail_with(label, vec![finding]);
        } else {
            self.record(outcome, label);
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut s = format!("\n=== {} ===\n", self.title);
        for e in &self.entries {
            match e {
                Entry::Ladder { outcome, label, .. } => {
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

    /// Ladder outcomes only.
    pub fn outcomes(&self) -> impl Iterator<Item = (Outcome, &str)> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Ladder { outcome, label, .. } => Some((*outcome, label.as_str())),
            Entry::Raw(_) => None,
        })
    }

    /// Every failing row's findings, in ladder order. A row decided without a
    /// child is itemized from the log written under it, else from its label.
    #[must_use]
    pub fn findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        for (at, entry) in self.entries.iter().enumerate() {
            let Entry::Ladder {
                outcome: Outcome::Fail(Severity::GateFailed),
                label,
                findings,
            } = entry
            else {
                continue;
            };
            if !findings.is_empty() {
                out.extend(findings.iter().cloned());
                continue;
            }
            let logged: Vec<&str> = self.entries[at + 1..]
                .iter()
                .map_while(|e| match e {
                    Entry::Raw(text) => Some(text.as_str()),
                    Entry::Ladder { .. } => None,
                })
                .collect();
            out.push(if logged.is_empty() {
                findings::label_finding(label)
            } else {
                findings::row_finding(label, &logged.join("\n"))
            });
        }
        out
    }
}

/// The accounting a run's exit code is decided from: every finding, every
/// could-not-run and every skip, named in ladder order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub gate_failures: Vec<String>,
    pub could_not_run: Vec<String>,
    pub skips: Vec<String>,
}

impl Tally {
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skips.len()
    }

    #[must_use]
    pub const fn failed(&self) -> bool {
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

/// Fold every stage report into one tally.
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
    fn every_skip_is_counted_and_named() {
        let mut a = Report::new("a");
        a.skip("targo test (no targo)");
        a.pass("something real");
        let mut b = Report::new("b");
        b.skip("gui smoke (macOS only)");
        b.fail("a finding");
        b.cannot_run("no driver");
        b.raw("  NOTICE: raw text is never a decision");

        let t = tally(&[a, b]);
        assert_eq!(t.skipped(), 2);
        assert_eq!(t.skips, ["targo test (no targo)", "gui smoke (macOS only)"]);
        assert_eq!(t.gate_failures, ["a finding"]);
        assert_eq!(t.could_not_run, ["no driver"]);
        assert!(t.failed());
    }

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
        r.decide_child(
            &run(false, "error[E0308]: mismatched types", None),
            "a finding",
        );
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
        let t = tally(std::slice::from_ref(&r));
        assert_eq!(t.gate_failures, ["a finding"]);
        assert_eq!(t.could_not_run.len(), 2, "never spawned, starved: {t:?}");
        let found = r.findings();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "a finding");
        assert_eq!(found[0].message, ["error[E0308]: mismatched types"]);
    }

    /// One plain failure beside the refusals makes the test child a finding:
    /// that failure alone, and the refusals beside it a COULD NOT RUN row.
    #[test]
    fn a_test_child_whose_every_failure_is_a_refusal_is_could_not_run() {
        let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
        let log = |second: &str| {
            format!(
                "     Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-1)\n\n\
                 running 2 tests\ntest a ... FAILED\ntest b ... FAILED\n\nfailures:\n\n\
                 ---- a stdout ----\n{sentinel} — strays alive\n\n\
                 ---- b stdout ----\n{second}\n\nfailures:\n    a\n    b\n\n\
                 test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered \
                 out; finished in 1.00s\n\nerror: test failed, to rerun pass `-p x --test \
                 bridge_e2e`\n"
            )
        };
        let run = |output: String| Run {
            ok: false,
            output,
            code: Some(101),
            spawn_error: None,
        };
        let mut r = Report::new("test (--workspace)");
        r.decide_child(&run(log(&format!("{sentinel} — strays alive"))), "refused");
        r.decide_test_child(&run(log("assertion failed: a finding")), "found", &[]);
        let t = tally(std::slice::from_ref(&r));
        assert_eq!(t.gate_failures, ["found"]);
        assert_eq!(t.could_not_run.len(), 2, "{t:?}");
        assert_eq!(
            t.could_not_run[1],
            "found — 1 of its failing tests refused the machine, not the tree (a) — strays alive",
            "the refusal named with what it said: {t:?}"
        );
        let ids: Vec<String> = r.findings().into_iter().map(|f| f.id).collect();
        assert_eq!(
            ids,
            ["-p x --test bridge_e2e -- b"],
            "the refusal is no finding"
        );
    }
}
