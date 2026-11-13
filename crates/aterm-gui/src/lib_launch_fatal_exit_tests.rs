// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::process::ExitStatusExt as _;

const ARM: &str = "aterm-launch-fatal-child";

const HANDLER_RAN: i32 = 86;

unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
}

extern "C" fn the_driver_destructors_ran() {
    // `_exit`, through the crate's one spelling of it: no further handler
    // or destructor runs, and nothing returns.
    crate::crash_signal::clean_exit_now(HANDLER_RAN)
}

fn launch_fatal_child_if_asked() {
    let argv: Vec<String> = std::env::args().collect();
    if !argv.iter().any(|a| a == ARM) {
        return;
    }
    // SAFETY: registering a C exit handler in a single-threaded `--test-threads=1` child.
    let armed = unsafe { atexit(the_driver_destructors_ran) };
    assert_eq!(armed, 0, "atexit accepted the handler");
    super::fatal_launch_error(
        true,
        super::LaunchFatal::Spawn,
        "spawn failed: child could not exec `/opt/homebrew/bin/fish`",
    );
}

fn run_child() -> crate::test_alone::Ran {
    let mut child = crate::test_alone::StandIn::helper(
        module_path!(),
        "a_launch_fatal_exits_1_and_runs_no_atexit_handler",
    );
    child.command().arg(ARM);
    child.spawn().selected()
}

#[test]
fn a_launch_fatal_exits_1_and_runs_no_atexit_handler() {
    launch_fatal_child_if_asked();
    let out = run_child();
    let (stdout, stderr) = (&out.stdout, &out.stderr);
    let status = out.status.expect("the stand-in exited");
    assert_eq!(
        status.signal(),
        None,
        "a handled configuration error must not die on a signal — stderr: {stderr}"
    );
    assert_eq!(
        status.code(),
        Some(1),
        "status {HANDLER_RAN} is the armed `atexit` handler run, as exit(3) runs it \
         — stderr: {stderr}"
    );
    assert!(
        stderr.contains("aterm-gui: spawn failed: child could not exec"),
        "the one true sentence still reaches stderr — stderr: {stderr}"
    );
    assert!(
        !stdout.contains("test result:"),
        "the process left INSIDE the fatal, not through a failing harness — stdout: {stdout}"
    );
}
