// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's idle-wake acceptance matrix, run explicitly by `verify measure`.

#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::spin::{SpinRow, SpinScene, measure};
    use aterm_conformance::measure::starvation::ShapeVerdict;

    const MAX_ARMS: u64 = 100;

    const ROWS: &[(&str, SpinRow)] = &[
        (
            "an_idle_claude_shaped_client_banks_no_past_deadline_arms",
            SpinRow {
                scene: SpinScene::ClaudeShaped,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: Some(12),
            },
        ),
        (
            "a_bare_idle_instance_banks_no_past_deadline_arms",
            SpinRow {
                scene: SpinScene::Idle,
                window: Duration::from_secs(6),
                max_arms: MAX_ARMS,
                max_timer_wakes: None,
            },
        ),
    ];

    fn take(rig: &Rig, row: &SpinRow) -> Outcome {
        let report = match measure(rig, row) {
            Ok(report) => report,
            Err(why) => {
                return Outcome::CouldNotRun(format!(
                    "SPIN COULD NOT RUN [{:?}]: the probe decided nothing: {why}",
                    row.scene
                ));
            }
        };
        if report.verdict == ShapeVerdict::Pass {
            Outcome::Pass(report.to_string())
        } else {
            Outcome::Fail(format!(
                "SPIN CONFORMANCE FAILED [{:?}]: an idle instance armed wakes past its own \
                 observation gate, or woke for timers far past its quiet rate.\n  {report}",
                row.scene
            ))
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"],
            env!("CARGO_BIN_EXE_fake-tui"),
        );
        support::run("spin", ROWS, |row| take(&rig, row))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        measuring::run()
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — spin requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
