// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Command, command};
use std::io;

/// The test's own name, for its re-execs.
const NAME: &str = "spawnfd::tests::a_file_that_is_no_program_is_a_start_error_and_raises_no_box";
/// Which part of the test a re-exec plays (`starts` or `mode`).
const ROLE: &str = "ATERM_SPAWNFD_NO_PROGRAM_ROLE";
/// `CREATE_DEFAULT_ERROR_MODE` (`WinBase.h`): the child gets the default error mode, not this
/// process's.
const CREATE_DEFAULT_ERROR_MODE: u32 = 0x0400_0000;
/// `SEM_FAILCRITICALERRORS` (`WinBase.h`): the critical-error boxes are off.
const SEM_FAILCRITICALERRORS: u32 = 0x0001;
/// `ERROR_BAD_EXE_FORMAT` (`winerror.h`): "%1 is not a valid Win32 application".
const ERROR_BAD_EXE_FORMAT: i32 = 193;
/// `ERROR_EXE_MACHINE_TYPE_MISMATCH` (`winerror.h`): "This version of %1 is not compatible".
const ERROR_EXE_MACHINE_TYPE_MISMATCH: i32 = 216;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetErrorMode() -> u32;
}

/// This process's error mode, the one a child inherits.
fn process_error_mode() -> u32 {
    // SAFETY: no arguments; it reads this process's error mode.
    unsafe { GetErrorMode() }
}

/// The test binary, re-run as this test alone in `role`.
fn this_test(role: &str) -> Command {
    let mut again = command(std::env::current_exe().expect("test binary"));
    again
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(ROLE, role);
    again
}

/// A start that must fail with the loader's answer, not wait on a box.
fn refused(started: io::Result<impl std::fmt::Debug>, how: &str) {
    let error = started.expect_err(how);
    assert!(
        matches!(
            error.raw_os_error(),
            Some(ERROR_BAD_EXE_FORMAT | ERROR_EXE_MACHINE_TYPE_MISMATCH)
        ),
        "{how}: {error:?}"
    );
}

/// A file that is no program for this machine, started through the seam, is the start's error,
/// answered at once and with no system box: `spawn`, `output` and `status` start with this
/// thread's error boxes off, where Windows' CreateProcess would otherwise raise "This version of
/// %1 is not compatible" and wait for a person to answer it, with std's process-wide spawn lock
/// held over every other start. The starts run in a re-exec given the default error mode
/// (`CREATE_DEFAULT_ERROR_MODE`), where the box is on whatever mode this run inherited, and a
/// child the seam starts there reads that mode, not the starting thread's: the guard leaves the
/// child's own error mode alone. NEGATIVE CONTROL: drop `quiet()` from `Command::spawn` and, in a
/// desktop session (`lab windows exec --desktop`), the re-exec waits on the box until the hang
/// detector. In session 0, where an SSH session's processes run, a box has no desktop to wait on
/// and answers its default at once, so the start errs with or without the guard: the test ends
/// COULD NOT RUN there rather than pass on nothing.
#[test]
fn a_file_that_is_no_program_is_a_start_error_and_raises_no_box() {
    match std::env::var(ROLE).as_deref() {
        Ok("mode") => {
            println!("process error mode {:#x}", process_error_mode());
            return;
        }
        Ok("starts") => {
            let mode = process_error_mode();
            assert_eq!(
                mode & SEM_FAILCRITICALERRORS,
                0,
                "the box is on here: {mode:#x}"
            );
            let dir = aterm_tempfile::tempdir().expect("scratch directory");
            let script = dir.path().join("not-a-program.exe");
            std::fs::write(&script, "#!/bin/sh\necho hi\n").expect("write the script");
            refused(command(&script).spawn(), "spawn");
            refused(command(&script).output(), "output");
            refused(command(&script).status(), "status");
            let child = this_test("mode").output().expect("the mode child");
            let stdout = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success(),
                "the mode child: {stdout}\n{}",
                String::from_utf8_lossy(&child.stderr)
            );
            // Under `--nocapture` the harness writes `test <name> ... ` before the test runs, so
            // the child's line follows that on the same line.
            let line = stdout
                .lines()
                .find_map(|line| line.split_once("process error mode 0x"))
                .map(|(_, mode)| mode)
                .expect("the mode child's line");
            let inherited = u32::from_str_radix(line.trim(), 16).expect("a hex mode");
            assert_eq!(
                inherited & SEM_FAILCRITICALERRORS,
                0,
                "the child kept the starting thread's quiet mode: {inherited:#x}"
            );
            return;
        }
        _ => {}
    }
    crate::host::could_not_run_in_session_zero("a desktop for the box the guard keeps off");
    let mut isolated = this_test("starts");
    crate::spawn::set_flags(&mut isolated, CREATE_DEFAULT_ERROR_MODE);
    let output = crate::deadline::output_within(&mut isolated, aterm_spec::wait::MUST_HAPPEN)
        .expect("the starts answer within the hang detector");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the starts in a process whose box is on:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
