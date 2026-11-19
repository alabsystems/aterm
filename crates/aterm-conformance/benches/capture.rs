// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The production Mac capture row. Its caller-owned functional claim supplies no Timing.

#[cfg(target_os = "macos")]
#[path = "support/functional.rs"]
mod functional;
#[cfg(target_os = "macos")]
#[path = "support/quiet.rs"]
#[allow(
    dead_code,
    reason = "this functional matrix reuses the shared admission option parser without invoking Timing admission"
)]
mod quiet;
mod support;

const ROW: &str = "ordinary_production_capture_survives_an_unavailable_presentation_lease";

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    {
        use aterm_conformance::measure::starvation::ShapeVerdict;
        use aterm_conformance::measure::{Rig, capture};
        let (mode, _) = match quiet::parse_args(std::env::args().skip(1)) {
            Ok(parsed) => parsed,
            Err(why) => {
                eprintln!("measure capture: {why}");
                return std::process::ExitCode::from(2);
            }
        };
        let run = || {
            let rig = Rig::release(
                &["ATERM_CAPTURE_BIN", "ATERM_PAINT_BIN"],
                aterm_tempfile::bin_exe("fake-tui"),
            );
            support::run("capture", &[(ROW, ())], |_| match capture::measure(&rig) {
                Ok(report) if report.verdict == ShapeVerdict::Pass => {
                    support::Outcome::Pass(report.line)
                }
                Ok(report) => support::Outcome::Fail(report.line),
                Err(aterm_conformance::measure::instance::CouldNotRun::Unanswered(why)) => {
                    support::Outcome::Unanswered(why)
                }
                Err(why) => support::Outcome::CouldNotRun(why.to_string()),
            })
        };
        if std::env::args().any(|arg| arg == "--list") {
            return run();
        }
        if std::env::var_os("ATERM_MEASURE_NO_WINDOWS").is_some_and(|value| value == "1") {
            return support::run("capture", &[(ROW, ())], |_| {
                support::Outcome::CouldNotRun("production capture opens a native window; the verifier omits this matrix under --no-windows".into())
            });
        }
        let _claim = match functional::acquire("capture", 4 << 30, mode) {
            Ok(claim) => claim,
            Err(why) => {
                eprintln!("measure capture: COULD NOT RUN — {why}; no row was taken");
                return std::process::ExitCode::from(3);
            }
        };
        run()
    }
    #[cfg(not(target_os = "macos"))]
    {
        support::run("capture", &[(ROW, ())], |_| {
            support::Outcome::CouldNotRun(
                "production Mac capture capability coverage requires macOS".into(),
            )
        })
    }
}
