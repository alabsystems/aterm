// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's idle-wake acceptance matrix, run explicitly by `verify measure`.
//!
//! Every row measures one private instance, sequentially, without retries.
//! `ATERM_SPIN_BIN` (or `ATERM_PAINT_BIN`) names the release executable.
//! An instrument refusal is never a pass.

#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::spin::{SpinRow, SpinScene, measure};
    use aterm_conformance::measure::starvation::ShapeVerdict;

    /// The ceiling: far above a healthy zero, far below a spin.
    const MAX_ARMS: u64 = 100;

    const ROWS: &[(&str, SpinRow)] = &[
        // Row 1, claude-shaped: the steady repainter arrives inside the window.
        // The window is 10 s because the movement clock goes stale `quiet_after`
        // (5 s) after the last real write. Timer wakes are bounded too: 12 is 2.4x
        // a healthy window and under the clamped poll's 21.
        (
            "an_idle_claude_shaped_client_banks_no_past_deadline_arms",
            SpinRow {
                scene: SpinScene::ClaudeShaped,
                window: Duration::from_secs(10),
                max_arms: MAX_ARMS,
                max_timer_wakes: Some(12),
            },
        ),
        // Row 2, the floor: a bare `/bin/sh` prompt with nothing running. Red
        // here is a spin that is not client-shaped, a different finding.
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
