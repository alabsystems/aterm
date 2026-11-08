// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's Rewind matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;
#[cfg(unix)]
#[path = "support/telemetry.rs"]
mod telemetry;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
    use aterm_conformance::measure::rewind::{RewindRow, RewindScene, measure};
    use aterm_conformance::measure::rewind_cost::{self, CostRow};
    use aterm_conformance::measure::starvation::Disposition;

    /// A full chain: the reader re-keyframes after a quarter of the 8 MiB budget of slices and witnesses, so 2 MiB of output is the longest a step folds through.
    const CHAIN: usize = 2_000_000;

    /// One row: a Rewind view's step or idle, or what recording costs.
    enum Row {
        View(RewindRow),
        Cost(CostRow),
    }

    const ROWS: &[(&str, Row)] = &[
        // Design §14.5 R1 exit row, declared before the build as key-to-present; a headless instance presents nothing, so it is booked at the fold shown (design §14.5 As built (R1)). PROVEN RED: with every step refolded from the keyframe it failed at p99 9.44 ms.
        (
            "a_rewind_step_is_shown_within_8ms_at_p99_over_a_2mib_chain",
            Row::View(RewindRow {
                scene: RewindScene::Steps {
                    steps: 100,
                    max_p99_us: 8_000,
                },
                chain_bytes: CHAIN,
            }),
        ),
        (
            "an_idle_open_rewind_view_makes_no_wake",
            Row::View(RewindRow {
                scene: RewindScene::Idle {
                    window: Duration::from_secs(10),
                },
                chain_bytes: CHAIN,
            }),
        ),
        // Design §14.5 R4 exit rows, declared before the candidate was measured: parse throughput with recording on at least 0.97x off, ABBA over fresh instances. Judged on the adjacent takes' off/on ratios: their median's 95% sign interval at or above 0.97 passes, wholly below fails, and one holding 0.97 is unproved (`rewind_cost::throughput_disposition`), since a load that comes and goes slows whole takes (`docs/measured/temporal-reader-rekey-2026-11-05.md`) and the arms' pooled medians followed it. Ten blocks give twenty ratios, so the interval is their 6th to 15th (95.9%).
        (
            "recording_keeps_parse_throughput_at_or_above_0_97x",
            Row::Cost(CostRow::Throughput {
                bytes: 64 << 20,
                pairs: 10,
                min_milli: 970,
            }),
        ),
        // One idle pane: recording adds at most `rewind_budget_mb`'s default (8 MiB, one pane's history cap) of resident size, the arms' medians over ABBA pairs of fresh instances. ATERM_DESIGN.md §7.2's 25 MB idle target is the whole app's, which misses it with recording off too, so the row judges it only on a bundle that records by default (the R4 flip it gates) and reports it on every other.
        (
            "an_idle_recording_pane_adds_at_most_one_panes_budget_rss",
            Row::Cost(CostRow::IdleRss {
                pairs: 3,
                max_extra_bytes: 8 << 20,
            }),
        ),
        // Twenty panes each draining 16 MiB at once: recording adds at most `rewind_total_mb`'s default (128 MiB) of resident size, and the pool holds at most its limit.
        (
            "twenty_busy_recording_panes_add_at_most_the_pool",
            Row::Cost(CostRow::BusyRss {
                panes: 20,
                bytes: 16 << 20,
                max_extra_bytes: 128 << 20,
            }),
        ),
    ];

    fn take(rig: &Rig, row: &Row) -> Outcome {
        match row {
            Row::View(row) => view(rig, row),
            Row::Cost(row) => cost(rig, row),
        }
    }

    fn cost(rig: &Rig, row: &CostRow) -> Outcome {
        match rewind_cost::measure(rig, row) {
            Ok(report) => match report.disposition {
                Disposition::Pass => Outcome::Pass(report.line),
                Disposition::Fail => Outcome::Fail(format!(
                    "REWIND COST FAILED: recording lost input or exceeded its cost bound, or a \
                     bundle that records by default is past the idle target.\n  {}",
                    report.line
                )),
                Disposition::Unproved => Outcome::Undecided(format!(
                    "REWIND COST UNPROVED: the adjacent takes' off/on ratios spread past the bound \
                     the row judges, so their median's 95% interval holds it and the takes are no \
                     evidence either way; take it on an unloaded machine.\n  {}",
                    report.line
                )),
            },
            Err(CouldNotRun::Unanswered(why)) => Outcome::Unanswered(why),
            Err(why) => Outcome::CouldNotRun(format!(
                "REWIND COST COULD NOT RUN: the probe decided nothing: {why}"
            )),
        }
    }

    fn view(rig: &Rig, row: &RewindRow) -> Outcome {
        let report = match measure(rig, row) {
            Ok(report) => report,
            Err(CouldNotRun::Unanswered(why)) => return Outcome::Unanswered(why),
            Err(why) => {
                return Outcome::CouldNotRun(format!(
                    "REWIND COULD NOT RUN: the probe decided nothing: {why}"
                ));
            }
        };
        match report.disposition {
            Disposition::Pass => Outcome::Pass(report.to_string()),
            Disposition::Fail => Outcome::Fail(format!(
                "REWIND CONFORMANCE FAILED: a step took past its bound to be shown, or an idle \
                 view woke the app.\n  {report}"
            )),
            Disposition::Unproved => Outcome::Undecided(format!(
                "REWIND UNPROVED: a step took past its bound to be shown while the driver was \
                 held off its core as long as the bound, so the take is not evidence about the \
                 step; take it on an unloaded machine.\n  {report}"
            )),
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_REWIND_BIN", "ATERM_PAINT_BIN"],
            aterm_tempfile::bin_exe("fake-tui"),
        );
        support::run_classified_with_telemetry(
            "rewind",
            ROWS,
            |row| match row {
                Row::View(RewindRow {
                    scene: RewindScene::Idle { .. },
                    ..
                })
                | Row::Cost(CostRow::IdleRss { .. } | CostRow::BusyRss { .. }) => {
                    support::Kind::Count { windowed: false }
                }
                _ => support::Kind::Timing,
            },
            |row| take(&rig, row),
            Some(support::Telemetry {
                cpu: super::telemetry::cpu,
                events: super::telemetry::events,
            }),
        )
    }

    /// A separate explicit collection mode: never mounted in the acceptance row list.
    pub fn diagnostic(args: &[String]) -> Option<std::process::ExitCode> {
        const FLAG: &str = "--recording-cost-diagnostic";
        const REFERENCE: &str = "--recording-reference-diagnostic";
        if !args.iter().any(|arg| arg == FLAG || arg == REFERENCE) {
            return None;
        }
        let (_, args) = match super::quiet::parse_args(args.iter().cloned()) {
            Ok(parsed) => parsed,
            Err(why) => {
                eprintln!("recording diagnostic: {why}");
                return Some(std::process::ExitCode::from(2));
            }
        };
        let [flag, subject, producer, output] = args.as_slice() else {
            eprintln!(
                "usage: rewind ({FLAG}|{REFERENCE}) SUBJECT PRODUCER EXISTING_PRIVATE_DIRECTORY [--admit now|wait]"
            );
            return Some(std::process::ExitCode::from(2));
        };
        if ![FLAG, REFERENCE].contains(&flag.as_str()) {
            return Some(std::process::ExitCode::from(2));
        }
        let rig = Rig {
            aterm: std::path::PathBuf::from(subject.as_str()),
            fake_tui: std::path::PathBuf::from(producer.as_str()),
        };
        Some(super::quiet::run(
            "rewind recording diagnostic",
            || match if flag.as_str() == REFERENCE {
                rewind_cost::diagnose_reference(&rig, std::path::Path::new(output))
            } else {
                rewind_cost::diagnose(&rig, std::path::Path::new(output))
            } {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("recording diagnostic: REFUSED — {error}; no acceptance verdict");
                    std::process::ExitCode::from(3)
                }
            },
        ))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        if let Some(code) = measuring::diagnostic(&std::env::args().skip(1).collect::<Vec<_>>()) {
            return code;
        }
        quiet::run("rewind", measuring::run)
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — rewind requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
