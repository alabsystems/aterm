// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run prints: one line per check as it finishes, each failure followed
//! by its findings and the command that reruns just it, then one verdict line.

use std::fmt::Write as _;
use std::time::Duration;

use crate::checks::Check;
use crate::ladder::{Finding, Outcome, Report, Severity};

/// How many findings a failed check lists before pointing at the log.
pub const FINDINGS_SHOWN: usize = 10;

/// One check's result: what its stages' rows add up to.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckResult {
    pub check: Check,
    pub outcome: Outcome,
    pub seconds: f64,
    /// One line: the scope it ran over, or why it failed, skipped or could
    /// not run.
    pub detail: String,
    /// The failures, itemized; for a skip or a could-not-run, every reason.
    pub findings: Vec<Finding>,
    /// The command that reruns just this check.
    pub rerun: String,
}

/// The word a check's line opens with.
#[must_use]
pub const fn word(o: Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Skip => "skip",
        Outcome::Fail(Severity::GateFailed) => "FAIL",
        Outcome::Fail(Severity::CouldNotRun) => "NOT RUN",
    }
}

/// Fold a check's stage reports into its result. The worst row decides: a
/// finding, then a could-not-run, then a skip (the check did not fully run),
/// else ok. `scope` is the selection it ran over, `""` for a whole-tree check.
#[must_use]
pub fn judge(
    check: Check,
    reports: &[&Report],
    seconds: f64,
    scope: &str,
    rerun: String,
) -> CheckResult {
    let rows: Vec<(Outcome, &str)> = reports.iter().flat_map(|r| r.outcomes()).collect();
    let worst = [
        Outcome::Fail(Severity::GateFailed),
        Outcome::Fail(Severity::CouldNotRun),
        Outcome::Skip,
    ]
    .into_iter()
    .find(|o| rows.iter().any(|(r, _)| r == o))
    .unwrap_or(Outcome::Ok);
    let labels: Vec<&str> = rows
        .iter()
        .filter(|(o, _)| *o == worst)
        .map(|(_, l)| *l)
        .collect();
    let (detail, findings) = match worst {
        Outcome::Ok => (scope.to_string(), Vec::new()),
        Outcome::Fail(Severity::GateFailed) => {
            let mut findings: Vec<Finding> = reports.iter().flat_map(|r| r.findings()).collect();
            let detail = match findings.iter().filter(|f| f.rerun.is_some()).count() {
                0 => labels.first().copied().unwrap_or_default().to_string(),
                1 => "1 test failed".to_string(),
                n => format!("{n} tests failed"),
            };
            // What could not run beside the failures follows them under its own
            // word, counted in none of them: a host with no fish is no failure.
            let unrun = Outcome::Fail(Severity::CouldNotRun);
            findings.extend(
                rows.iter()
                    .filter(|(o, _)| *o == unrun)
                    .map(|(_, l)| Finding {
                        id: format!("{} {l}", word(unrun)),
                        rerun: None,
                        message: Vec::new(),
                    }),
            );
            (detail, findings)
        }
        _ => {
            let detail = match (labels.as_slice(), worst) {
                ([], _) => String::new(),
                ([one], _) => (*one).to_string(),
                (all, Outcome::Skip) => format!("{} of its rows skipped:", all.len()),
                (all, _) => format!("{} of its rows could not run:", all.len()),
            };
            let reasons = if labels.len() > 1 {
                labels
                    .iter()
                    .map(|l| Finding {
                        id: (*l).to_string(),
                        rerun: None,
                        message: Vec::new(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (detail, reasons)
        }
    };
    CheckResult {
        check,
        outcome: worst,
        seconds,
        detail,
        findings,
        rerun,
    }
}

/// `1.2s`, `47s`, `3m05s`.
#[must_use]
pub fn duration(secs: f64) -> String {
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{secs:.0}s")
    } else {
        let whole = Duration::from_secs_f64(secs).as_secs();
        format!("{}m{:02}s", whole / 60, whole % 60)
    }
}

/// A check's lines: `ok    lint    40s  -p aterm-grid`, and under a failure
/// its findings, each with its message and rerun, then the check's own rerun.
/// `width` pads the check names into one column.
#[must_use]
pub fn render(r: &CheckResult, width: usize) -> String {
    let mut s = format!(
        "{:<7} {:<width$} {:>6}",
        word(r.outcome),
        r.check.name(),
        duration(r.seconds),
    );
    if !r.detail.is_empty() {
        let _ = write!(s, "  {}", r.detail);
    }
    s.push('\n');
    let pad = "        ";
    let shown = r.findings.iter().take(FINDINGS_SHOWN);
    for f in shown {
        // A finding that is the row the detail already names adds only its message.
        if f.id != r.detail {
            let _ = writeln!(s, "{pad}{}", f.id);
        }
        for line in &f.message {
            let _ = writeln!(s, "{pad}  {line}");
        }
        if let Some(cmd) = &f.rerun {
            let _ = writeln!(s, "{pad}  rerun: {cmd}");
        }
    }
    if r.findings.len() > FINDINGS_SHOWN {
        let _ = writeln!(
            s,
            "{pad}… {} more in the log",
            r.findings.len() - FINDINGS_SHOWN
        );
    }
    if matches!(r.outcome, Outcome::Fail(_)) {
        let _ = writeln!(s, "{pad}rerun: {}", r.rerun);
    }
    s
}

/// The verdict line and the exit code, from every check's result. Under
/// `release` a skip is exit 3: publication needs every check to have run.
#[must_use]
pub fn verdict(results: &[CheckResult], release: bool, seconds: f64) -> (String, i32) {
    let named = |o: Outcome| -> Vec<&str> {
        results
            .iter()
            .filter(|r| r.outcome == o)
            .map(|r| r.check.name())
            .collect()
    };
    let failed = named(Outcome::Fail(Severity::GateFailed));
    let unrun = named(Outcome::Fail(Severity::CouldNotRun));
    let skipped = named(Outcome::Skip);
    let ok = named(Outcome::Ok).len();
    let mut tail = vec![format!("{ok} ok")];
    if !skipped.is_empty() {
        tail.push(format!(
            "{} skipped ({})",
            skipped.len(),
            skipped.join(", ")
        ));
    }
    let tail = format!("{} in {}", tail.join(", "), duration(seconds));
    if !failed.is_empty() {
        let mut head = format!("{} failed", failed.join(", "));
        if !unrun.is_empty() {
            let _ = write!(head, "; {} could not run", unrun.join(", "));
        }
        return (
            format!("verify: FAIL — {head}; {tail}"),
            crate::exit::FAILED,
        );
    }
    if !unrun.is_empty() {
        return (
            format!(
                "verify: COULD NOT RUN — {} could not run; {tail}",
                unrun.join(", ")
            ),
            crate::exit::COULD_NOT_RUN,
        );
    }
    if release && !skipped.is_empty() {
        return (
            format!(
                "verify: COULD NOT RUN — `release` needs every check, and {} skipped; {tail}",
                skipped.join(", ")
            ),
            crate::exit::COULD_NOT_RUN,
        );
    }
    (format!("verify: PASS — {tail}"), crate::exit::PASS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(check: Check, outcome: Outcome) -> CheckResult {
        CheckResult {
            check,
            outcome,
            seconds: 1.0,
            detail: String::new(),
            findings: Vec::new(),
            rerun: format!("targo --unverified verify {}", check.name()),
        }
    }

    #[test]
    fn the_worst_row_decides_and_a_failed_test_is_its_own_finding() {
        let mut lint = Report::new("tippy");
        lint.pass("tippy -p a");
        let r = judge(Check::Lint, &[&lint], 2.0, "-p a", "rerun".into());
        assert_eq!(r.outcome, Outcome::Ok);

        let mut skipped = Report::new("doc");
        skipped.skip("targo test --doc (no library)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into());
        assert_eq!(r.outcome, Outcome::Skip);
        skipped.skip("another (no tool)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into());
        assert_eq!(r.findings.len(), 2, "each reason listed under it");

        let mut test = Report::new("test");
        test.pass("compile");
        test.fail("a row");
        test.cannot_run("no disk");
        let r = judge(Check::Test, &[&test], 0.0, "", "rerun".into());
        assert_eq!(r.outcome, Outcome::Fail(Severity::GateFailed));
        let ids: Vec<&str> = r.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a row", "NOT RUN no disk"],
            "the failure, then what could not run beside it under its own word"
        );
        assert_eq!(r.detail, "a row");
    }

    #[test]
    fn the_exit_code_is_the_worst_check_and_a_release_skip_is_could_not_run() {
        let ok = result(Check::Fmt, Outcome::Ok);
        let skip = result(Check::Doc, Outcome::Skip);
        let fail = result(Check::Lint, Outcome::Fail(Severity::GateFailed));
        let unrun = result(Check::Test, Outcome::Fail(Severity::CouldNotRun));
        let code = |results: &[CheckResult], release: bool| verdict(results, release, 1.0).1;
        assert_eq!(code(&[ok.clone(), skip.clone()], false), crate::exit::PASS);
        assert_eq!(
            code(&[ok.clone(), skip], true),
            crate::exit::COULD_NOT_RUN,
            "`release` needs every check"
        );
        assert_eq!(
            code(&[ok.clone(), fail, unrun.clone()], false),
            crate::exit::FAILED
        );
        assert_eq!(code(&[ok, unrun], false), crate::exit::COULD_NOT_RUN);
    }
}
