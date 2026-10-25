// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sequential acceptance measurement runner.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;

pub enum Outcome {
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "macOS-only acceptance targets share this outcome type on other hosts, where their rows must refuse instead of constructing Pass"
        )
    )]
    Pass(String),
    Fail(String),
    CouldNotRun(String),
    /// The subject has no answer to what the row asks: it does not know a verb, or its reply
    /// lacks a field. Whoever knows which build the subject is decides what that says.
    #[allow(
        dead_code,
        reason = "only the conformance matrices ask a subject that may predate a row; the other benches that mount this runner build what they measure"
    )]
    Unanswered(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    planned: usize,
    passed: usize,
    failed: usize,
    refused: usize,
    unanswered: usize,
}

impl Counts {
    fn exit_code(&self) -> u8 {
        if self.failed != 0 {
            1
        } else if self.refused != 0 || self.unanswered != 0 || self.planned == 0 {
            3
        } else {
            0
        }
    }
}

pub fn run<R>(target: &str, rows: &[(&str, R)], take: impl FnMut(&R) -> Outcome) -> ExitCode {
    match run_into(target, rows, take, &mut io::stdout().lock()) {
        Ok(counts) => ExitCode::from(counts.exit_code()),
        Err(why) => {
            eprintln!("measure {target}: cannot write the measurement report: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run_into<R>(
    target: &str,
    rows: &[(&str, R)],
    mut take: impl FnMut(&R) -> Outcome,
    out: &mut dyn Write,
) -> io::Result<Counts> {
    let mut counts = Counts {
        planned: rows.len(),
        ..Counts::default()
    };
    for (name, row) in rows {
        let result = catch_unwind(AssertUnwindSafe(|| take(row))).unwrap_or_else(|panic| {
            let why = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            Outcome::Fail(format!("measurement panicked: {why}"))
        });
        let (status, detail) = match result {
            Outcome::Pass(detail) => {
                counts.passed += 1;
                ("PASS", detail)
            }
            Outcome::Fail(detail) => {
                counts.failed += 1;
                ("FAIL", detail)
            }
            Outcome::CouldNotRun(detail) => {
                counts.refused += 1;
                ("COULD NOT RUN", detail)
            }
            Outcome::Unanswered(detail) => {
                counts.unanswered += 1;
                ("UNANSWERED", detail)
            }
        };
        writeln!(out, "measure row: {target}::{name} {status}\n{detail}")?;
        out.flush()?;
    }
    writeln!(
        out,
        "measure result: {target} planned={} passed={} failed={} refused={} unanswered={}",
        counts.planned, counts.passed, counts.failed, counts.refused, counts.unanswered
    )?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_row_runs_once_in_order_and_a_finding_outweighs_a_refusal() {
        use super::*;

        let rows = [
            ("pass", 0),
            ("refuse", 1),
            ("panic", 2),
            ("unasked", 3),
            ("last", 4),
        ];
        let mut seen = Vec::new();
        let mut output = Vec::new();
        let counts = run_into(
            "fixture",
            &rows,
            |row| {
                seen.push(*row);
                match row {
                    0 | 4 => Outcome::Pass("measured".into()),
                    1 => Outcome::CouldNotRun("no adapter".into()),
                    2 => panic!("broken instrument"),
                    3 => Outcome::Unanswered("no such verb".into()),
                    _ => unreachable!(),
                }
            },
            &mut output,
        )
        .unwrap();
        assert_eq!(seen, [0, 1, 2, 3, 4]);
        assert_eq!(
            counts,
            Counts {
                planned: 5,
                passed: 2,
                failed: 1,
                refused: 1,
                unanswered: 1,
            }
        );
        assert_eq!(counts.exit_code(), 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("measure row: fixture::panic FAIL\nmeasurement panicked:"));
        assert!(output.contains("measure row: fixture::unasked UNANSWERED\nno such verb"));
        assert!(output.ends_with("fixture planned=5 passed=2 failed=1 refused=1 unanswered=1\n"));
    }

    #[test]
    fn only_complete_decided_measurements_pass() {
        use super::*;

        let mut output = Vec::new();
        let empty = run_into(
            "empty",
            &[] as &[(&str, ())],
            |_| unreachable!(),
            &mut output,
        )
        .unwrap();
        assert_eq!(empty.exit_code(), 3);
        let rows = [("one", ()), ("two", ())];
        let refused = run_into(
            "refused",
            &rows,
            |_| Outcome::CouldNotRun("no display".into()),
            &mut output,
        )
        .unwrap();
        assert_eq!(refused.refused, rows.len());
        assert_eq!(refused.exit_code(), 3);
        let passed = run_into(
            "passed",
            &rows,
            |_| Outcome::Pass("measured".into()),
            &mut output,
        )
        .unwrap();
        assert_eq!(passed.passed, rows.len());
        assert_eq!(passed.exit_code(), 0);
        let unanswered = run_into(
            "unanswered",
            &rows,
            |_| Outcome::Unanswered("no such field".into()),
            &mut output,
        )
        .unwrap();
        assert_eq!(unanswered.unanswered, rows.len());
        assert_eq!(unanswered.exit_code(), 3);
    }
}
