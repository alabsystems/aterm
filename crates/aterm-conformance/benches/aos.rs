// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's aos repair matrix, run explicitly by `verify measure`.

#[cfg(unix)]
#[path = "support/quiet.rs"]
mod quiet;
#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::aos::{AosRow, measure};
    use aterm_conformance::measure::starvation::ShapeVerdict;

    const ROWS: &[(&str, AosRow)] = &[(
        "a_repair_takes_at_most_8ms_of_main_thread_time_at_p99",
        AosRow {
            repairs: 40,
            keys: 24,
            max_p99_us: 8_000,
        },
    )];

    fn take(rig: &Rig, row: &AosRow) -> Outcome {
        let report = match measure(rig, row) {
            Ok(report) => report,
            Err(why) => {
                return Outcome::CouldNotRun(format!(
                    "AOS COULD NOT RUN: the probe decided nothing: {why}"
                ));
            }
        };
        if report.verdict == ShapeVerdict::Pass {
            Outcome::Pass(report.to_string())
        } else {
            Outcome::Fail(format!(
                "AOS CONFORMANCE FAILED: a repair held the main thread past its bound, or \
                 prepared on it.\n  {report}"
            ))
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_AOS_BIN", "ATERM_PAINT_BIN"],
            env!("CARGO_BIN_EXE_fake-tui"),
        );
        support::run("aos", ROWS, |row| take(&rig, row))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        quiet::run("aos", measuring::run)
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — aos requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
