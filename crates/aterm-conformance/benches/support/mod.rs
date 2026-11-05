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
    /// The row measured, and its takes are no evidence either way on this machine's load (an
    /// interval too wide, a starved driver). It is stated beside the rows that passed, never a
    /// finding and never a refusal (owner ruling 2026-11-05: a busy machine never blocks a
    /// release). A row that could not measure at all is `CouldNotRun`.
    #[allow(
        dead_code,
        reason = "only paint and rewind judge takes a machine's load can leave undecided; spin and aos mount the same runner"
    )]
    Undecided(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    planned: usize,
    passed: usize,
    failed: usize,
    refused: usize,
    unanswered: usize,
    undecided: usize,
}

impl Counts {
    /// 1 for a finding; 3 where a row could not run or went unanswered, or the matrix is empty;
    /// else 0, undecided rows included, a wholly undecided matrix too: a busy machine is stated,
    /// never a refusal (owner ruling 2026-11-05).
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

pub fn run<R>(target: &str, rows: &[(&str, R)], mut take: impl FnMut(&R) -> Outcome) -> ExitCode {
    let options = match selection(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("measure {target}: {why}");
            return ExitCode::from(2);
        }
    };
    let selected: Vec<_> = rows
        .iter()
        .filter(|(name, _)| {
            options
                .0
                .as_ref()
                .is_none_or(|filter| name.contains(filter))
        })
        .map(|(name, row)| (*name, row))
        .collect();
    if options.1 {
        for (name, _) in &selected {
            println!("{target}::{name}");
        }
        return ExitCode::SUCCESS;
    }
    match run_into(
        target,
        &selected,
        |row| take(*row),
        &mut io::stdout().lock(),
    ) {
        Ok(counts) => ExitCode::from(counts.exit_code()),
        Err(why) => {
            eprintln!("measure {target}: cannot write the measurement report: {why}");
            ExitCode::FAILURE
        }
    }
}

fn selection(args: impl Iterator<Item = String>) -> Result<(Option<String>, bool), String> {
    let mut args = args;
    let (mut filter, mut list) = (None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => {}
            "--admit" => {
                args.next().ok_or("--admit needs now or wait")?;
            }
            "--list" => list = true,
            "--filter" => {
                if filter.is_some() {
                    return Err("--filter given twice".into());
                }
                filter = Some(
                    args.next()
                        .filter(|word| !word.is_empty() && !word.starts_with('-'))
                        .ok_or("--filter needs a row-name substring")?,
                );
            }
            _ => {
                return Err(format!(
                    "unknown argument {arg}; use --filter ROW or --list"
                ));
            }
        }
    }
    Ok((filter, list))
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
            Outcome::Undecided(detail) => {
                counts.undecided += 1;
                ("UNDECIDED", detail)
            }
        };
        writeln!(out, "measure row: {target}::{name} {status}\n{detail}")?;
        out.flush()?;
    }
    writeln!(
        out,
        "measure result: {target} planned={} passed={} failed={} refused={} unanswered={} undecided={}",
        counts.planned,
        counts.passed,
        counts.failed,
        counts.refused,
        counts.unanswered,
        counts.undecided
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
                undecided: 0,
            }
        );
        assert_eq!(counts.exit_code(), 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("measure row: fixture::panic FAIL\nmeasurement panicked:"));
        assert!(output.contains("measure row: fixture::unasked UNANSWERED\nno such verb"));
        assert!(
            output.ends_with(
                "fixture planned=5 passed=2 failed=1 refused=1 unanswered=1 undecided=0\n"
            )
        );
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
        let undecided = run_into(
            "undecided",
            &rows,
            |_| Outcome::Undecided("interval too wide".into()),
            &mut output,
        )
        .unwrap();
        assert_eq!(undecided.undecided, rows.len());
        assert_eq!(
            undecided.exit_code(),
            0,
            "a wholly undecided matrix is stated, never a refusal"
        );
        let mut first = true;
        let beside = run_into(
            "beside",
            &rows,
            |_| {
                if std::mem::take(&mut first) {
                    Outcome::Pass("measured".into())
                } else {
                    Outcome::Undecided("interval too wide".into())
                }
            },
            &mut output,
        )
        .unwrap();
        assert_eq!((beside.passed, beside.undecided), (1, 1));
        assert_eq!(
            beside.exit_code(),
            0,
            "an undecided row beside a pass is stated"
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("measure row: beside::two UNDECIDED\ninterval too wide"));
    }
}

#[cfg(test)]
mod selection_tests {
    #[test]
    fn selecting_rows_is_explicit_and_rejects_misspelled_options() {
        let args = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).into())
                .collect::<Vec<_>>()
                .into_iter()
        };
        assert_eq!(
            super::selection(args(&["--bench", "--filter", "pet", "--list"])).unwrap(),
            (Some("pet".into()), true)
        );
        assert!(super::selection(args(&["--filter"])).is_err());
        assert!(super::selection(args(&["--fliter", "pet"])).is_err());
    }
}
