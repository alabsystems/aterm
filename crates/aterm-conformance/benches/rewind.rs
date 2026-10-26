// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's Rewind matrix, run explicitly by `verify measure`.

#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::instance::CouldNotRun;
    use aterm_conformance::measure::rewind::{RewindRow, RewindScene, measure};
    use aterm_conformance::measure::starvation::Disposition;

    /// A full chain: the recorder re-keyframes at a quarter of its 8 MiB budget, so 2 MiB of output is the longest a step folds through.
    const CHAIN: usize = 2_000_000;

    const ROWS: &[(&str, RewindRow)] = &[
        // Design §14.5 R1 exit row, declared before the build as key-to-present; a headless instance presents nothing, so it is booked at the fold shown (design §14.5 As built (R1)). PROVEN RED: with every step refolded from the keyframe it failed at p99 9.44 ms.
        (
            "a_rewind_step_is_shown_within_8ms_at_p99_over_a_2mib_chain",
            RewindRow {
                scene: RewindScene::Steps {
                    steps: 100,
                    max_p99_us: 8_000,
                },
                chain_bytes: CHAIN,
            },
        ),
        (
            "an_idle_open_rewind_view_makes_no_wake",
            RewindRow {
                scene: RewindScene::Idle {
                    window: Duration::from_secs(10),
                },
                chain_bytes: CHAIN,
            },
        ),
    ];

    fn take(rig: &Rig, row: &RewindRow) -> Outcome {
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
        measuring::run()
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — rewind requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
