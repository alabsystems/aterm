// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix: an idle release instance must stay idle. What each row
//! measures is `aterm_conformance::measure::spin`.
//!
//! The rows are ignored in an ordinary test run; the `measure` check runs them
//! with `-- --ignored` and the release binary in `ATERM_SPIN_BIN` (or
//! `ATERM_PAINT_BIN`). Without it a row is red, never "not measured".
#![cfg(unix)]

mod measuring {
    use std::time::Duration;

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::spin::{SpinRow, SpinScene, measure};
    use aterm_conformance::measure::starvation::ShapeVerdict;

    /// The ceiling: far above a healthy zero, far below a spin.
    const MAX_ARMS: u64 = 100;

    fn run(row: SpinRow) {
        let rig = Rig::release(
            &["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"],
            env!("CARGO_BIN_EXE_fake-tui"),
        );
        let report = match measure(&rig, &row) {
            Ok(report) => report,
            // A probe that decided nothing ends COULD NOT RUN, so the verify gate
            // records the machine, not a spin finding.
            Err(why) => aterm_spec::host::could_not_run(format!(
                "SPIN COULD NOT RUN [{:?}]: the probe decided nothing, which is not a pass: {why}",
                row.scene
            )),
        };
        assert_eq!(
            report.verdict,
            ShapeVerdict::Pass,
            "SPIN CONFORMANCE FAILED [{:?}]: an idle instance armed wakes past its own \
             observation gate, or woke for timers far past its quiet rate.\n  {report}",
            row.scene
        );
        eprintln!("{report}");
    }

    /// Row 1, claude-shaped: the steady repainter arrives inside the window.
    /// The window is 10 s because the movement clock goes stale `quiet_after`
    /// (5 s) after the last real write. Timer wakes are bounded too: 12 is 2.4x
    /// a healthy window and under the clamped poll's 21.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_SPIN_BIN"]
    fn an_idle_claude_shaped_client_banks_no_past_deadline_arms() {
        run(SpinRow {
            scene: SpinScene::ClaudeShaped,
            window: Duration::from_secs(10),
            max_arms: MAX_ARMS,
            max_timer_wakes: Some(12),
        });
    }

    /// Row 2, the floor: a bare `/bin/sh` prompt with nothing running. Red
    /// here is a spin that is not client-shaped, a different finding.
    #[test]
    #[ignore = "measures a release aterm: --ignored with ATERM_SPIN_BIN"]
    fn a_bare_idle_instance_banks_no_past_deadline_arms() {
        run(SpinRow {
            scene: SpinScene::Idle,
            window: Duration::from_secs(6),
            max_arms: MAX_ARMS,
            max_timer_wakes: None,
        });
    }
}
