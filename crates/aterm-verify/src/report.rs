// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run prints: one line per check as it finishes, each failure followed
//! by its findings and the command that reruns just it, then one verdict line.
//! `--json` prints the same as JSON lines.

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
pub fn word(o: Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Skip => "skip",
        Outcome::Fail(Severity::GateFailed) => "FAIL",
        Outcome::Fail(Severity::CouldNotRun) => "NOT RUN",
    }
}

/// The outcome as `--json` spells it.
#[must_use]
pub fn json_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Skip => "skip",
        Outcome::Fail(Severity::GateFailed) => "fail",
        Outcome::Fail(Severity::CouldNotRun) => "not-run",
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
            let findings: Vec<Finding> = reports.iter().flat_map(|r| r.findings()).collect();
            let detail = match findings.iter().filter(|f| f.rerun.is_some()).count() {
                0 => labels.first().copied().unwrap_or_default().to_string(),
                1 => "1 test failed".to_string(),
                n => format!("{n} tests failed"),
            };
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

/// A JSON string literal.
#[must_use]
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_list(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

/// One check as a JSON object on one line.
#[must_use]
pub fn json_check(r: &CheckResult) -> String {
    let findings = json_list(r.findings.iter().map(|f| {
        format!(
            "{{\"id\":{},\"rerun\":{},\"message\":{}}}",
            json_str(&f.id),
            f.rerun
                .as_deref()
                .map_or_else(|| "null".to_string(), json_str),
            json_list(f.message.iter().map(|m| json_str(m)))
        )
    }));
    format!(
        "{{\"type\":\"check\",\"name\":{},\"outcome\":\"{}\",\"seconds\":{:.1},\"detail\":{},\"findings\":{findings},\"rerun\":{}}}",
        json_str(r.check.name()),
        json_word(r.outcome),
        r.seconds,
        json_str(&r.detail),
        json_str(&r.rerun),
    )
}

/// The run's summary as a JSON object on one line.
#[must_use]
pub fn json_summary(
    results: &[CheckResult],
    exit: i32,
    seconds: f64,
    selection: &str,
    log: Option<&str>,
) -> String {
    let count = |o: Outcome| results.iter().filter(|r| r.outcome == o).count();
    let outcome = match exit {
        crate::exit::PASS => "pass",
        crate::exit::FAILED => "fail",
        _ => "could-not-run",
    };
    format!(
        "{{\"type\":\"summary\",\"outcome\":\"{outcome}\",\"exit\":{exit},\"checks\":{},\"ok\":{},\"failed\":{},\"not_run\":{},\"skipped\":{},\"seconds\":{seconds:.1},\"selection\":{},\"log\":{}}}",
        results.len(),
        count(Outcome::Ok),
        count(Outcome::Fail(Severity::GateFailed)),
        count(Outcome::Fail(Severity::CouldNotRun)),
        count(Outcome::Skip),
        json_str(selection),
        log.map_or_else(|| "null".to_string(), json_str),
    )
}

/// The summary of a run that stopped before any check ran (a usage error, no
/// checkout, no pin, no toolchain): its exit code and the line it printed.
#[must_use]
pub fn json_refusal(exit: i32, why: &str) -> String {
    let outcome = if exit == crate::exit::USAGE {
        "usage"
    } else {
        "could-not-run"
    };
    format!(
        "{{\"type\":\"summary\",\"outcome\":\"{outcome}\",\"exit\":{exit},\"checks\":0,\"why\":{}}}",
        json_str(why)
    )
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
        assert_eq!((r.outcome, r.detail.as_str()), (Outcome::Ok, "-p a"));

        let mut skipped = Report::new("doc");
        skipped.skip("targo test --doc (no library)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into());
        assert_eq!(r.outcome, Outcome::Skip);
        assert_eq!(r.detail, "targo test --doc (no library)");
        assert!(r.findings.is_empty(), "one reason is the detail itself");
        skipped.skip("another (no tool)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into());
        assert_eq!(r.detail, "2 of its rows skipped:");
        assert_eq!(r.findings.len(), 2, "each reason listed under it");

        let mut test = Report::new("test");
        test.pass("compile");
        test.fail("a row");
        test.cannot_run("no disk");
        let r = judge(Check::Test, &[&test], 0.0, "", "rerun".into());
        assert_eq!(r.outcome, Outcome::Fail(Severity::GateFailed));
        assert_eq!(r.detail, "a row");
        assert_eq!(r.findings.len(), 1);
    }

    #[test]
    fn a_failure_prints_its_findings_and_both_reruns() {
        let r = CheckResult {
            check: Check::Test,
            outcome: Outcome::Fail(Severity::GateFailed),
            seconds: 83.9,
            detail: "1 test failed".into(),
            findings: vec![Finding {
                id: "-p a --lib -- t".into(),
                rerun: Some("targo --unverified test -p a --lib -- t --exact".into()),
                message: vec!["assertion failed".into()],
            }],
            rerun: "targo --unverified verify test -p a".into(),
        };
        assert_eq!(
            render(&r, 5),
            "FAIL    test   1m23s  1 test failed\n\
             \x20       -p a --lib -- t\n\
             \x20         assertion failed\n\
             \x20         rerun: targo --unverified test -p a --lib -- t --exact\n\
             \x20       rerun: targo --unverified verify test -p a\n"
        );
        assert_eq!(
            render(&result(Check::Fmt, Outcome::Ok), 5),
            "ok      fmt     1.0s\n"
        );
    }

    #[test]
    fn the_exit_code_is_the_worst_check_and_a_release_skip_is_could_not_run() {
        let ok = result(Check::Fmt, Outcome::Ok);
        let skip = result(Check::Doc, Outcome::Skip);
        let fail = result(Check::Lint, Outcome::Fail(Severity::GateFailed));
        let unrun = result(Check::Test, Outcome::Fail(Severity::CouldNotRun));
        let (line, code) = verdict(&[ok.clone(), skip.clone()], false, 5.0);
        assert_eq!(code, crate::exit::PASS);
        assert_eq!(line, "verify: PASS — 1 ok, 1 skipped (doc) in 5.0s");
        let (line, code) = verdict(&[ok.clone(), skip.clone()], true, 5.0);
        assert_eq!(code, crate::exit::COULD_NOT_RUN);
        assert!(
            line.contains("`release` needs every check, and doc skipped"),
            "{line}"
        );
        let (line, code) = verdict(&[ok.clone(), fail, unrun.clone()], false, 75.0);
        assert_eq!(code, crate::exit::FAILED);
        assert_eq!(
            line,
            "verify: FAIL — lint failed; test could not run; 1 ok in 1m15s"
        );
        let (_, code) = verdict(&[ok, unrun], false, 1.0);
        assert_eq!(code, crate::exit::COULD_NOT_RUN);
    }

    #[test]
    fn json_lines_are_one_object_each_and_escape_what_they_carry() {
        let mut r = result(Check::Test, Outcome::Fail(Severity::GateFailed));
        r.findings.push(Finding {
            id: "-p a --lib -- t".into(),
            rerun: None,
            message: vec!["said \"no\"\tthen\nstopped".into()],
        });
        let line = json_check(&r);
        assert!(!line.contains('\n'));
        assert_eq!(
            line,
            "{\"type\":\"check\",\"name\":\"test\",\"outcome\":\"fail\",\"seconds\":1.0,\
             \"detail\":\"\",\"findings\":[{\"id\":\"-p a --lib -- t\",\"rerun\":null,\
             \"message\":[\"said \\\"no\\\"\\tthen\\nstopped\"]}],\
             \"rerun\":\"targo --unverified verify test\"}"
        );
        assert_eq!(
            json_summary(&[r], 1, 2.0, "-p a", None),
            "{\"type\":\"summary\",\"outcome\":\"fail\",\"exit\":1,\"checks\":1,\"ok\":0,\
             \"failed\":1,\"not_run\":0,\"skipped\":0,\"seconds\":2.0,\
             \"selection\":\"-p a\",\"log\":null}"
        );
        assert_eq!(json_str("\u{1}"), "\"\\u0001\"");
        assert_eq!(
            json_refusal(3, "verify: COULD NOT RUN — no pin"),
            "{\"type\":\"summary\",\"outcome\":\"could-not-run\",\"exit\":3,\"checks\":0,\
             \"why\":\"verify: COULD NOT RUN — no pin\"}"
        );
    }
}
