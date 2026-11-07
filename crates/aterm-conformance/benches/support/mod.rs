// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Acceptance measurement runner with an explicit concurrent count-row experiment.

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

#[allow(dead_code, reason = "count matrices use run_classified instead")]
pub fn run<R>(target: &str, rows: &[(&str, R)], mut take: impl FnMut(&R) -> Outcome) -> ExitCode {
    run_selected(target, rows, |selected, out| {
        run_into(target, selected, |row| take(*row), out)
    })
}

/// A count observes an isolated subject over a window. Timing includes any row whose values
/// depend on another row's CPU, display or throughput work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the shared runner also serves matrices with only timing rows"
)]
pub enum Kind {
    Count { windowed: bool },
    Timing,
}

#[allow(
    dead_code,
    reason = "the shared runner also serves matrices with only timing rows"
)]
pub fn run_classified<R: Sync>(
    target: &str,
    rows: &[(&str, R)],
    kind: impl Fn(&R) -> Kind + Sync,
    take: impl Fn(&R) -> Outcome + Sync,
) -> ExitCode {
    let together = match std::env::var("ATERM_MEASURE_COUNT_MODE").as_deref() {
        Ok("sequential") | Err(std::env::VarError::NotPresent) => false,
        Ok("together") => true,
        _ => {
            eprintln!("measure {target}: ATERM_MEASURE_COUNT_MODE takes sequential or together");
            return ExitCode::from(2);
        }
    };
    run_selected(target, rows, |selected, out| {
        run_scheduled_into(
            target,
            selected,
            |row| kind(*row),
            |row| take(*row),
            together,
            out,
        )
    })
}

/// A single worker runs windows in order, with one worker per headless count. The caller
/// reports each completion immediately, then joins every count before starting timing work.
#[allow(
    dead_code,
    reason = "the shared runner also serves matrices with only timing rows"
)]
fn run_scheduled_into<R: Sync>(
    target: &str,
    rows: &[(&str, R)],
    kind: impl Fn(&R) -> Kind + Sync,
    take: impl Fn(&R) -> Outcome + Sync,
    together: bool,
    out: &mut dyn Write,
) -> io::Result<Counts> {
    if !together {
        return run_into(target, rows, take, out);
    }
    let (mut headless, mut windows, mut timing) = (Vec::new(), Vec::new(), Vec::new());
    for (i, (_, row)) in rows.iter().enumerate() {
        match kind(row) {
            Kind::Count { windowed: false } => headless.push((i, row)),
            Kind::Count { windowed: true } => windows.push((i, row)),
            Kind::Timing => timing.push((i, row)),
        }
    }
    let count_len = headless.len() + windows.len();
    let mut counts = Counts {
        planned: rows.len(),
        ..Counts::default()
    };
    std::thread::scope(|scope| -> io::Result<()> {
        let (send, receive) = std::sync::mpsc::channel();
        let take = &take;
        for (i, row) in headless {
            let send = send.clone();
            scope.spawn(move || {
                let _ = send.send((i, measured(|| take(row))));
            });
        }
        if !windows.is_empty() {
            let send = send.clone();
            scope.spawn(move || {
                for (i, row) in windows {
                    let _ = send.send((i, measured(|| take(row))));
                }
            });
        }
        drop(send);
        for _ in 0..count_len {
            let (i, result) = receive
                .recv()
                .map_err(|_| io::Error::other("count worker ended without an outcome"))?;
            report_row(target, rows[i].0, result, &mut counts, out)?;
        }
        Ok(())
    })?;
    for (i, row) in timing {
        report_row(target, rows[i].0, measured(|| take(row)), &mut counts, out)?;
    }
    report_summary(target, &counts, out)?;
    Ok(counts)
}

fn measured(take: impl FnOnce() -> Outcome) -> Outcome {
    catch_unwind(AssertUnwindSafe(take)).unwrap_or_else(|panic| {
        let why = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        Outcome::Fail(format!("measurement panicked: {why}"))
    })
}

fn run_selected<R>(
    target: &str,
    rows: &[(&str, R)],
    execute: impl FnOnce(&[(&str, &R)], &mut dyn Write) -> io::Result<Counts>,
) -> ExitCode {
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
    // A worker may print diagnostics of its own; do not hold stdout's lock while awaiting it.
    match execute(&selected, &mut io::stdout()) {
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
        report_row(target, name, measured(|| take(row)), &mut counts, out)?;
    }
    report_summary(target, &counts, out)?;
    Ok(counts)
}

fn report_row(
    target: &str,
    name: &str,
    result: Outcome,
    counts: &mut Counts,
    out: &mut dyn Write,
) -> io::Result<()> {
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
    out.flush()
}

fn report_summary(target: &str, counts: &Counts, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "measure result: {target} planned={} passed={} failed={} refused={} unanswered={} undecided={}",
        counts.planned,
        counts.passed,
        counts.failed,
        counts.refused,
        counts.unanswered,
        counts.undecided
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn findings_flush_before_the_next_row_or_the_count_phase_ends() {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;

        struct Tape {
            bytes: Arc<Mutex<Vec<u8>>>,
            release: Option<mpsc::Sender<()>>,
        }
        impl Write for Tape {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                if String::from_utf8_lossy(&self.bytes.lock().unwrap())
                    .contains("fixture::fast FAIL")
                    && let Some(release) = self.release.take()
                {
                    let _ = release.send(());
                }
                Ok(())
            }
        }
        for together in [false, true] {
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let (release, wait) = mpsc::channel();
            let wait = Mutex::new(wait);
            let done = AtomicBool::new(false);
            let mut tape = Tape {
                bytes: Arc::clone(&bytes),
                release: Some(release),
            };
            let rows = [("fast", 0), ("window", 1), ("timing", 2)];
            let counts = run_scheduled_into(
                "fixture",
                &rows,
                |row| {
                    if *row == 2 {
                        Kind::Timing
                    } else {
                        Kind::Count {
                            windowed: *row == 1,
                        }
                    }
                },
                |row| match *row {
                    0 => Outcome::Fail("early finding".into()),
                    1 => {
                        // Buffering until the phase ends cannot release this row. This is
                        // a hang detector, not an assertion about the reporter's latency.
                        wait.lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(60))
                            .unwrap();
                        done.store(true, Ordering::SeqCst);
                        Outcome::Pass("window".into())
                    }
                    2 => {
                        assert!(done.load(Ordering::SeqCst));
                        assert!(
                            String::from_utf8_lossy(&bytes.lock().unwrap())
                                .contains("fixture::window PASS")
                        );
                        Outcome::Pass("timing".into())
                    }
                    _ => unreachable!(),
                },
                together,
                &mut tape,
            )
            .unwrap();
            assert_eq!(counts.planned, 3);
            assert_eq!(counts.passed, 2);
            assert_eq!(counts.failed, 1);
            assert_eq!(counts.exit_code(), 1);
        }
    }

    #[test]
    fn counts_overlap_one_window_at_a_time_and_timing_waits_for_every_count() {
        use super::*;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Mutex, mpsc};
        use std::time::Duration;

        let rows = [
            ("headless-a", 0),
            ("headless-b", 1),
            ("window-a", 2),
            ("window-b", 3),
            ("timing", 4),
        ];
        let (ready_tx, ready_rx) = mpsc::channel();
        let ready_rx = Mutex::new(ready_rx);
        let (release_a, wait_a) = mpsc::channel();
        let (release_b, wait_b) = mpsc::channel();
        let waits = [Mutex::new(wait_a), Mutex::new(wait_b)];
        let window = AtomicBool::new(false);
        let finished = AtomicUsize::new(0);
        let mut output = Vec::new();
        let counts = run_scheduled_into(
            "fixture",
            &rows,
            |row| {
                if *row == 4 {
                    Kind::Timing
                } else {
                    Kind::Count {
                        windowed: *row >= 2,
                    }
                }
            },
            |row| {
                match *row {
                    0 | 1 => {
                        ready_tx.send(*row).unwrap();
                        waits[*row]
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(60))
                            .unwrap();
                        finished.fetch_add(1, Ordering::SeqCst);
                    }
                    2 | 3 => {
                        assert!(!window.swap(true, Ordering::SeqCst));
                        if *row == 2 {
                            // A sequential runner cannot reach both sends while this window is open.
                            let rx = ready_rx.lock().unwrap();
                            let mut ids = [
                                rx.recv_timeout(Duration::from_secs(60)).unwrap(),
                                rx.recv_timeout(Duration::from_secs(60)).unwrap(),
                            ];
                            ids.sort_unstable();
                            assert_eq!(ids, [0, 1]);
                            release_a.send(()).unwrap();
                            release_b.send(()).unwrap();
                        }
                        window.store(false, Ordering::SeqCst);
                        finished.fetch_add(1, Ordering::SeqCst);
                    }
                    4 => {
                        assert_eq!(finished.load(Ordering::SeqCst), 4);
                        assert!(!window.load(Ordering::SeqCst));
                    }
                    _ => unreachable!(),
                }
                Outcome::Pass(row.to_string())
            },
            true,
            &mut output,
        )
        .unwrap();
        assert_eq!(counts.passed, 5);
        let output = String::from_utf8(output).unwrap();
        for (name, i) in rows {
            assert!(output.contains(&format!("fixture::{name} PASS\n{i}\n")));
        }
        let seen = Mutex::new(Vec::new());
        let mut output = Vec::new();
        let counts = run_scheduled_into(
            "fixture",
            &rows,
            |_| Kind::Count { windowed: false },
            |row| {
                seen.lock().unwrap().push(*row);
                if *row == 1 {
                    panic!("negative control");
                }
                Outcome::Pass(row.to_string())
            },
            false,
            &mut output,
        )
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), [0, 1, 2, 3, 4]);
        assert_eq!(counts.failed, 1);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("negative control")
        );
    }

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
