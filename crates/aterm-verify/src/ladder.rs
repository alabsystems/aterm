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

    /// Verbatim output (child logs, NOTICE lines). Trailing newlines are trimmed;
    /// the renderer supplies exactly one.
    pub fn raw(&mut self, text: impl Into<String>) {
        let text = text.into();
        let trimmed = text.trim_end_matches('\n');
        if !trimmed.is_empty() {
            self.entries.push(Entry::Raw(trimmed.to_string()));
        }
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

/// The accounting the verdict is computed from: how many findings, how many
/// could-not-runs, and the NAME of every skip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub gate_failures: usize,
    pub could_not_run: usize,
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
        self.gate_failures > 0 || self.could_not_run > 0
    }

    /// Add one decision.
    pub fn record(&mut self, outcome: Outcome, label: &str) {
        match outcome {
            Outcome::Ok => {}
            Outcome::Skip => self.skips.push(label.to_string()),
            Outcome::Fail(Severity::GateFailed) => self.gate_failures += 1,
            Outcome::Fail(Severity::CouldNotRun) => self.could_not_run += 1,
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
        assert_eq!(t.gate_failures, 1);
        assert_eq!(t.could_not_run, 1);
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

    #[test]
    fn raw_text_never_becomes_a_skip() {
        // A NOTICE line explains a skip; it must not be counted as one, or the
        // verdict would name the same absence twice and inflate the count.
        let mut r = Report::new("r");
        r.raw("  NOTICE: something");
        assert_eq!(tally(&[r]), Tally::default());
    }
}
