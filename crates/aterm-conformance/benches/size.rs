// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release binary's size row, run explicitly by `verify measure`.

#[cfg(unix)]
mod support;

#[cfg(unix)]
mod measuring {
    use super::support::{self, Outcome};

    use aterm_conformance::measure::Rig;
    use aterm_conformance::measure::size::{SizeRow, measure};
    use aterm_conformance::measure::starvation::ShapeVerdict;

    // Binary growth is measured on every bundle: the row reads the executable's Mach-O header and
    // judges the slice for the Mac it runs on (arm64 on the release Mac), so a universal bundle
    // and a one-slice dev build are held alike. The x86_64 slice of a universal bundle is
    // reported and not bounded.
    //
    // The rule: the ceiling is the measured slice for this Mac plus 10%, the rule the wasm module
    // ceilings follow. A take past it is a finding, answered by shrinking the executable or by
    // raising the ceiling with what grew recorded beside it here, never by a silent re-record.
    //
    // History. Each `targo --unverified build -p aterm --release` (arm64, unsigned) measured
    // 2026-10-27: 73,663,576 B before Rewind (77e2bd9c3b) and 82,295,592 B on 5bf00deb90. The
    // first ceiling, never landed, was the second plus 10%, 90,525,151 B. The installed 0.147.0
    // bundle's arm64 slice read 93,113,344 B on 2026-11-01, past it.
    //
    // Raised 2026-11-13 by the same rule: the same build of main c3682d2db0d reads 145,674,616 B,
    // so the ceiling is 160,242,077 B. The growth from 82.3 MB is the 48,589 commits of main
    // between the two builds; fat LTO leaves no per-crate map, so it is not attributed here. On
    // that day the row passed the installed 0.200.0 bundle (its signed arm64 slice 142,725,872 B,
    // its x86_64 slice of 164,670,816 B reported) and failed a copy of the c3682d2db0d build
    // padded by 15 MiB to 161,403,256 B.
    const MAX_HOST_SLICE_BYTES: u64 = 160_242_077;

    const ROWS: &[(&str, SizeRow)] = &[(
        "the_release_executables_slice_stays_within_its_ceiling",
        SizeRow {
            max_bytes: MAX_HOST_SLICE_BYTES,
        },
    )];

    fn take(rig: &Rig, row: &SizeRow) -> Outcome {
        match measure(&rig.aterm, row) {
            Ok(report) if report.verdict == ShapeVerdict::Pass => Outcome::Pass(report.to_string()),
            Ok(report) => Outcome::Fail(format!(
                "SIZE FAILED: the release executable's slice for this Mac is past its ceiling, \
                 or it carries no such slice.\n  {report}"
            )),
            Err(why) => Outcome::CouldNotRun(format!(
                "SIZE COULD NOT RUN: the row read no executable: {why}"
            )),
        }
    }

    pub fn run() -> std::process::ExitCode {
        let rig = Rig::release(
            &["ATERM_SIZE_BIN", "ATERM_PAINT_BIN"],
            aterm_tempfile::bin_exe("fake-tui"),
        );
        support::run("size", ROWS, |row| take(&rig, row))
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    {
        measuring::run()
    }
    #[cfg(not(unix))]
    {
        eprintln!("measure: COULD NOT RUN — size requires a Unix host");
        std::process::ExitCode::from(3)
    }
}
