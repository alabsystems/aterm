// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's Rewind matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
    use aterm_conformance::measure::rewind::{RewindRow, RewindScene, measure};
    use aterm_conformance::measure::rewind_cost::{self, CostRow};
    use aterm_conformance::measure::starvation::{Disposition, ShapeVerdict};

    /// A full chain: the recorder re-keyframes at a quarter of its 8 MiB budget, so 2 MiB of output is the longest a step folds through.
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
        // Design §14.5 R4 exit rows, declared before the candidate was measured: parse throughput with recording on at least 0.97x off, ABBA over fresh instances, the arms' medians.
        (
            "recording_keeps_parse_throughput_at_or_above_0_97x",
            Row::Cost(CostRow::Throughput {
                bytes: 64 << 20,
                pairs: 5,
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
            Ok(report) if report.verdict == ShapeVerdict::Pass => Outcome::Pass(report.line),
            Ok(report) => Outcome::Fail(format!(
                "REWIND COST FAILED: recording on cost past its bound, or a bundle that records \
                 by default is past the idle target.\n  {}",
                report.line
            )),
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
            Disposition::Unproved => Outcome::CouldNotRun(format!(
                "REWIND UNPROVED: a step took past its bound to be shown while the driver was \
                 held off its core as long as the bound, so the take is not evidence about the \
                 step; take it on an unloaded machine.\n  {report}"
            )),
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_REWIND_BIN", "ATERM_PAINT_BIN"],
            env!("CARGO_BIN_EXE_fake-tui"),
        );
        support::run("rewind", ROWS, |row| take(&rig, row))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        quiet::run("rewind", measuring::run)
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — rewind requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
