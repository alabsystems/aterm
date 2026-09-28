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

/// One failure, itemized so another run's can be compared with it
/// ([`crate::differential`]): the unit a base receipt lists and a branch's run
/// is judged by, one `fail` line in the receipt each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// What failed: a test (`-p atpkg --test index_probe -- <name>`, from
    /// cargo's re-run spec and libtest's name) or, when the row cannot be
    /// itemized, the ladder row's own label. Never normalized: two rows that
    /// differ only in a number are two findings.
    pub id: String,
    /// [`crate::differential::fingerprint`] of how it failed — everything the
    /// failure printed, whitespace and line numbers included, with only its
    /// run-to-run noise masked (where the run happened, thread ids, pids,
    /// measured durations, build hashes, frame addresses, commit ids:
    /// [`crate::differential::normalize`]); every count and value it printed
    /// is kept. "The same failure" is the same id AND this.
    pub hash: String,
    /// Why this failure can never be matched against main's, when it cannot:
    /// the machine refused the test ([`crate::libtest::COULD_NOT_RUN_SENTINEL`]),
    /// or the row printed nothing to compare. It then always counts as NEW.
    pub opaque: Option<&'static str>,
    /// The words that make this failure read as a CLOCK running out — a
    /// [`crate::differential::TIMING_MARKERS`] entry in its message, or
    /// [`crate::differential::MEASURED_DURATION`] for one that printed a clock
    /// it read — when it does. The verdict names such a failure when its stage
    /// ran on a loaded machine ([`StageLoad::loaded`]), as a label; and it is
    /// never inherited from main, whose same words cannot be told from a new
    /// hang or a slower run's ([`crate::differential::judge`]).
    pub timing: Option<&'static str>,
}

/// THE MACHINE'S LOAD AROUND ONE STAGE (2026-09-26): the one-minute load
/// average when the stage started and when it ended, in hundredths (as
/// `sysctl vm.loadavg` prints it), and how many cores it is read against.
///
/// WHY. A red on a machine at load 78 on 14 cores (measured on the audit's
/// runs, 2026-09-24) and the same red on an idle one are different evidence,
/// and until then only the opt-in `--timings` TSV (retired 2026-09-27) knew
/// which it was. The
/// ladder's `time` line, the receipt (`load` lines) and the verdict's
/// under-load label read this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageLoad {
    pub start: u32,
    pub end: u32,
    pub cores: u32,
}

impl StageLoad {
    /// From two readings ([`crate::exec::load_average`]); `None` unless both
    /// were read.
    #[must_use]
    pub fn of(start: Option<f64>, end: Option<f64>, cores: u32) -> Option<Self> {
        // Through the two-decimal text the reading came from, so no float is
        // ever cast: a load is a small non-negative number, and anything else
        // is no reading.
        let centi = |l: f64| -> Option<u32> {
            if !(l.is_finite() && (0.0..1e7).contains(&l)) {
                return None;
            }
            let text = format!("{l:.2}");
            let (whole, frac) = text.split_once('.')?;
            whole
                .parse::<u32>()
                .ok()?
                .checked_mul(100)?
                .checked_add(frac.parse().ok()?)
        };
        Some(Self {
            start: centi(start?)?,
            end: centi(end?)?,
            cores,
        })
    }

    /// Hundredths as the decimal they came from: `7482` is `74.82`.
    #[must_use]
    pub fn text(centi: u32) -> String {
        format!("{}.{:02}", centi / 100, centi % 100)
    }

    /// `74.82 -> 80.10 on 14 cores`.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} -> {} on {} cores",
            Self::text(self.start),
            Self::text(self.end),
            self.cores
        )
    }

    /// Was the machine BUSY around this stage: the one-minute load, at either
    /// end, above half its cores — past that, a correct test waiting on a clock
    /// competes for a core with work that is not its own.
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.start.max(self.end) > self.cores.saturating_mul(50)
    }
}

/// Why a stage failed — the distinction `tools/verify.sh`'s exit codes draw
/// (`1` FAILED, `3` COULD NOT RUN; [`crate::exit`]).
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
    Ladder {
        outcome: Outcome,
        label: String,
        /// A finding's itemized failures ([`Report::fail_child`],
        /// [`Report::fail_test_child`], [`Report::fail_and_continue`]); empty
        /// for every other row, and for a finding decided without a child,
        /// which the tally itemizes from its label or its log, never to be
        /// inherited ([`tally`]).
        findings: Vec<Finding>,
    },
    Raw(String),
}

/// One stage's complete output, buffered so that concurrently-run stages still
/// print in the ladder's declared order.
#[derive(Clone, Debug)]
pub struct Report {
    pub title: String,
    pub entries: Vec<Entry>,
    /// The machine's load around the stage ([`StageLoad`]), set by the run
    /// once the stage has finished; `None` when it could not be read.
    pub load: Option<StageLoad>,
}

impl Report {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            entries: Vec::new(),
            load: None,
        }
    }

    pub fn pass(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Ok,
            label: label.into(),
            findings: Vec::new(),
        });
    }

    pub fn skip(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Skip,
            label: label.into(),
            findings: Vec::new(),
        });
    }

    /// A gate decided against the tree, by the stage itself, with no child
    /// whose output is the finding — or with a log under it (a smoke's child
    /// log tail). NEVER INHERITED (2026-09-27, fourth review): nothing shows
    /// the stage went on to decide what came after it, and most such rows end
    /// their stage — a smoke returns at its first bad reply, so main's red on
    /// one hid every check behind it ([`crate::differential::UNFINISHED`],
    /// applied by [`tally`]). A row after which the stage goes on is
    /// [`Report::fail_and_continue`].
    pub fn fail(&mut self, label: impl Into<String>) {
        self.fail_with(label, Vec::new());
    }

    /// A gate the stage decided by itself against the tree, AFTER WHICH IT
    /// GOES ON to decide everything it would have decided had this passed: a
    /// reply that is wrong and stopped nothing, or the stage's last check. Its
    /// label is its whole message ([`crate::differential::label_finding`]),
    /// and it can be inherited — a row that ends its stage is [`Report::fail`].
    /// Nothing is read from a log under it.
    pub fn fail_and_continue(&mut self, label: impl Into<String>) {
        let label = label.into();
        let finding = crate::differential::label_finding(&label);
        self.fail_with(label, vec![finding]);
    }

    /// A gate decided against the tree, and these are its itemized failures.
    fn fail_with(&mut self, label: impl Into<String>, findings: Vec<Finding>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Fail(Severity::GateFailed),
            label: label.into(),
            findings,
        });
    }

    /// The stage could not execute. Still a `FAIL` line: fail-closed is the rule,
    /// and a missing tool that a stage NEEDED is not an honest skip.
    pub fn cannot_run(&mut self, label: impl Into<String>) {
        self.entries.push(Entry::Ladder {
            outcome: Outcome::Fail(Severity::CouldNotRun),
            label: label.into(),
            findings: Vec::new(),
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
    ///
    /// The row's finding is the row itself, fingerprinted from what the child
    /// printed ([`crate::differential::row_finding`]) — and NEVER INHERITED
    /// (2026-09-27, fourth review): nothing shows the child ran to its end,
    /// and one that stops at its first failure prints the same whatever it
    /// did not reach ([`crate::differential::UNFINISHED`]). A checker whose
    /// output can show it is [`Report::decide_checker_child`]'s.
    pub fn fail_child(&mut self, run: &Run, label: impl Into<String>) {
        self.fail_checker_child(run, label, |_| Err(crate::differential::UNFINISHED));
    }

    /// [`Report::decide_child`] for a CHECKER whose failing output can show it
    /// decided everything it covers: its finding can be inherited only when
    /// `ran_to_end` says so, and is never inherited otherwise
    /// ([`crate::differential::RanToEnd`]).
    pub fn decide_checker_child(
        &mut self,
        run: &Run,
        label: impl Into<String>,
        ran_to_end: crate::differential::RanToEnd,
    ) {
        if run.ok {
            self.pass(label);
        } else {
            self.fail_checker_child(run, label, ran_to_end);
        }
    }

    /// [`Report::fail_child`], inheritable when `ran_to_end` says the checker
    /// reached its end.
    pub fn fail_checker_child(
        &mut self,
        run: &Run,
        label: impl Into<String>,
        ran_to_end: crate::differential::RanToEnd,
    ) {
        let label = label.into();
        if !self.child_could_not_run(run, &label) {
            let mut finding = crate::differential::row_finding(&label, &run.output);
            if let Err(why) = ran_to_end(&run.output) {
                finding = crate::differential::never_inherited(finding, why);
            }
            self.fail_with(label, vec![finding]);
        }
    }

    /// [`Report::fail_child`] for a `targo test` child: its findings are the
    /// failed TESTS, each with the binary that failed it, when the log
    /// accounts for every one ([`crate::differential::test_findings`]) — so a
    /// red test main already has is one inherited finding, not a whole red row.
    pub fn fail_test_child(&mut self, run: &Run, label: impl Into<String>) {
        let label = label.into();
        if !self.child_could_not_run(run, &label) {
            let findings = crate::differential::test_findings(&label, &run.output);
            self.fail_with(label, findings);
        }
    }

    /// [`Report::decide_child`] for a `targo test` child
    /// ([`Report::fail_test_child`]).
    ///
    /// A PASS THAT RAN NO TEST IS NO PASS (2026-09-27, third review). cargo
    /// starts each test binary through a runner when a cargo config or the
    /// environment names one for the host (`target.<triple>.runner`,
    /// `CARGO_TARGET_<TRIPLE>_RUNNER`) — and that OUTRANKS the gate's own
    /// recording runner, which is a `cfg(all())` one, so the run falls back to
    /// cargo's serial child, which starts every binary through it too. A runner
    /// that runs nothing (`true`, a wrapper that lost its argument) exited 0 for
    /// every binary, printed no test, and the row was `ok`: the merge contract,
    /// with no test run. A child that announced test binaries and printed not
    /// one libtest `test result:` decided nothing about them — COULD NOT RUN,
    /// naming why ([`crate::libtest::ran_no_test`]).
    pub fn decide_test_child(&mut self, run: &Run, label: impl Into<String>) {
        let label = label.into();
        if run.ok && crate::libtest::ran_no_test(&run.output) {
            self.cannot_run(format!(
                "{label} — could not run: {}",
                crate::libtest::RAN_NO_TEST
            ));
        } else if run.ok {
            self.pass(label);
        } else {
            self.fail_test_child(run, label);
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
            findings: Vec::new(),
        });
    }

    /// [`Report::record`] for an outcome computed from `run`'s exit code: a
    /// finding is fingerprinted from what the child printed, as
    /// [`Report::fail_child`]'s is, so two runs whose driver failed with the
    /// same code but said different things are two different failures — and,
    /// as that one is, never inherited: one exit code answers for all of a
    /// driver's checks, and nothing shows it reached the last
    /// ([`crate::differential::UNFINISHED`]).
    pub fn record_child(&mut self, run: &Run, outcome: Outcome, label: impl Into<String>) {
        let label = label.into();
        if outcome == Outcome::Fail(Severity::GateFailed) {
            let finding = crate::differential::never_inherited(
                crate::differential::row_finding(&label, &run.output),
                crate::differential::UNFINISHED,
            );
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

    /// Ladder outcomes only, for tests and accounting.
    pub fn outcomes(&self) -> impl Iterator<Item = (Outcome, &str)> {
        self.decisions().map(|(outcome, label, _)| (outcome, label))
    }

    /// Ladder outcomes with each row's itemized findings.
    pub fn decisions(&self) -> impl Iterator<Item = (Outcome, &str, &[Finding])> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Ladder {
                outcome,
                label,
                findings,
            } => Some((*outcome, label.as_str(), findings.as_slice())),
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
    /// The itemized failures of each finding, parallel to `gate_failures`
    /// ([`Tally::findings_of`]): what a receipt lists and what the
    /// differential verdict judges row by row.
    pub findings: Vec<Vec<Finding>>,
    /// The load around the stage each finding came from, parallel to
    /// `gate_failures` ([`Tally::load_of`]) — what the verdict's under-load
    /// label reads.
    pub loads: Vec<Option<StageLoad>>,
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
        self.record_with(outcome, label, &[]);
    }

    /// Add one decision with its itemized findings. A finding recorded without
    /// any — a row a stage decided without a child, and with no log under it
    /// ([`tally`]) — is itemized from its label
    /// ([`crate::differential::label_finding`]), so every row the tally counts
    /// as a finding has at least one.
    pub fn record_with(&mut self, outcome: Outcome, label: &str, findings: &[Finding]) {
        self.record_under(outcome, label, findings, None);
    }

    /// [`Tally::record_with`] for a decision of a stage that ran under `load`.
    pub fn record_under(
        &mut self,
        outcome: Outcome,
        label: &str,
        findings: &[Finding],
        load: Option<StageLoad>,
    ) {
        match outcome {
            Outcome::Ok => {}
            Outcome::Skip => self.skips.push(label.to_string()),
            Outcome::Fail(Severity::GateFailed) => {
                self.gate_failures.push(label.to_string());
                self.loads.push(load);
                self.findings.push(if findings.is_empty() {
                    vec![crate::differential::label_finding(label)]
                } else {
                    findings.to_vec()
                });
            }
            Outcome::Fail(Severity::CouldNotRun) => self.could_not_run.push(label.to_string()),
        }
    }

    /// The itemized failures of the `row`-th finding — empty for a row a
    /// caller put in `gate_failures` by hand, which the differential verdict
    /// then can never excuse.
    #[must_use]
    pub fn findings_of(&self, row: usize) -> &[Finding] {
        self.findings.get(row).map_or(&[], Vec::as_slice)
    }

    /// Every itemized failure, in ladder order.
    pub fn all_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().flatten()
    }

    /// The load around the stage the `row`-th finding came from, when known.
    #[must_use]
    pub fn load_of(&self, row: usize) -> Option<StageLoad> {
        self.loads.get(row).copied().flatten()
    }
}

/// Fold every stage report into the run's accounting.
///
/// A FINDING DECIDED WITHOUT A CHILD BUT WITH A LOG UNDER IT (2026-09-27) —
/// `r.fail("smoke: aterm-gui exited early")` and then the child's log tail as
/// raw text — is fingerprinted from that log, up to the stage's next row
/// ([`crate::differential::row_finding`]). Until then it was itemized from its
/// label alone, so a new crash under the same label read as main's.
///
/// AND IT IS NEVER INHERITED (2026-09-27, fourth review), with or without a
/// log: a row the stage decided by itself ([`Report::fail`], a
/// [`Report::record`]ed finding) most often ends the stage, and nothing shows
/// it did not ([`crate::differential::UNFINISHED`]). The stage says so when it
/// goes on ([`Report::fail_and_continue`], which itemizes the row itself).
#[must_use]
pub fn tally(reports: &[Report]) -> Tally {
    let mut t = Tally::default();
    for r in reports {
        for (at, entry) in r.entries.iter().enumerate() {
            let Entry::Ladder {
                outcome,
                label,
                findings,
            } = entry
            else {
                continue;
            };
            let logged: Vec<&str> = r.entries[at + 1..]
                .iter()
                .map_while(|e| match e {
                    Entry::Raw(text) => Some(text.as_str()),
                    Entry::Ladder { .. } => None,
                })
                .collect();
            if *outcome == Outcome::Fail(Severity::GateFailed) && findings.is_empty() {
                let finding = if logged.is_empty() {
                    crate::differential::label_finding(label)
                } else {
                    crate::differential::row_finding(label, &logged.join("\n"))
                };
                let finding =
                    crate::differential::never_inherited(finding, crate::differential::UNFINISHED);
                t.record_under(*outcome, label, &[finding], r.load);
            } else {
                t.record_under(*outcome, label, findings, r.load);
            }
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_columns_match_the_script_byte_for_byte() {
        let mut r = Report::new("test compile (--workspace)");
        r.pass("targo test --workspace --no-run");
        r.skip("tippy lint (no tippy)");
        r.fail("license_check.sh");
        r.cannot_run("targo not found");
        assert_eq!(
            r.render(),
            "\n=== test compile (--workspace) ===\n\
             \x20 ok    targo test --workspace --no-run\n\
             \x20 skip  tippy lint (no tippy)\n\
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

    /// THE MACHINE'S REFUSAL, THROUGH THE STAGE'S OWN PATH (2026-09-23). A
    /// test child whose every failure carries the COULD NOT RUN sentinel is
    /// the COULD NOT RUN severity — the receipt says `COULD-NOT-RUN`, not
    /// `FAIL` — and the same child with one plain failure beside it stays a
    /// finding.
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
        r.decide_child(&run(log("assertion failed: a finding")), "found");
        let t = tally(&[r]);
        assert_eq!(t.gate_failures, ["found"]);
        assert_eq!(t.could_not_run.len(), 1, "{t:?}");
        assert!(
            t.could_not_run[0].starts_with("refused — could not run: every failing test refused")
                && t.could_not_run[0].contains("(a, b)"),
            "{t:?}"
        );
    }

    /// A LOAD IS TWO READINGS OR NONE: both ends read, in hundredths from the
    /// two-decimal text, never a cast of a float; a missing, negative or
    /// non-finite reading is no load at all; and the tally keeps each
    /// finding's stage load beside it, row for row.
    #[test]
    fn a_stage_load_is_two_readings_or_none_and_rides_with_its_findings() {
        let l = StageLoad::of(Some(74.82), Some(80.1), 14).expect("both read");
        assert_eq!((l.start, l.end, l.cores), (7482, 8010, 14));
        assert_eq!(l.describe(), "74.82 -> 80.10 on 14 cores");
        assert_eq!(StageLoad::text(5), "0.05");
        assert_eq!(StageLoad::of(Some(1.0), None, 14), None);
        assert_eq!(StageLoad::of(None, Some(1.0), 14), None);
        assert_eq!(StageLoad::of(Some(-1.0), Some(1.0), 14), None);
        assert_eq!(StageLoad::of(Some(f64::NAN), Some(1.0), 14), None);

        let mut a = Report::new("a");
        a.fail("first");
        a.load = Some(l);
        let mut b = Report::new("b");
        b.fail("second");
        let t = tally(&[a, b]);
        assert_eq!(t.load_of(0), Some(l));
        assert_eq!(t.load_of(1), None);
        assert_eq!(t.load_of(2), None);
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
