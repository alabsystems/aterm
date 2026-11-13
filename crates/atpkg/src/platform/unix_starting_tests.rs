// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::wait::MUST_HAPPEN;

use super::Starting;

/// Set, this process is the one [`a_watch_ended_inside_a_start_leaves_no_handler`] started to
/// run its body alone.
const ALONE: &str = "ATPKG_STARTING_WATCH_ALONE";

/// `sig`'s action now.
fn action(sig: libc::c_int) -> libc::sighandler_t {
    // SAFETY: all-zero is a valid `sigaction`; a null new action only reads the current one
    // into `now`, which outlives the call.
    unsafe {
        let mut now: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(sig, std::ptr::null(), &raw mut now), 0);
        now.sa_sigaction
    }
}

/// A WATCH THAT BEGAN BEFORE A START AND ENDED INSIDE IT LEAVES NO HANDLER (module docs): an
/// admission's watch catching SIGTERM, then a start setting it to its default, then the watch
/// ending, then the start: SIGTERM is at its default as both found it. It runs alone in a
/// process of its own, which sets SIGTERM's default first, so no other test of this binary
/// (an admission's watch) moves it meanwhile and no runner's ignored one hides the watch.
/// NEGATIVE CONTROL (by hand: a start with no watch of its own): the start puts back the ended
/// watch's handler, and SIGTERM stays caught.
#[test]
fn a_watch_ended_inside_a_start_leaves_no_handler() {
    if std::env::var_os(ALONE).is_none() {
        let mut alone =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        alone
            .args([
                "--exact",
                "platform::unix::starting::tests::a_watch_ended_inside_a_start_leaves_no_handler",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ALONE, "1");
        // Twice the hang detector: a body that hangs is named by this bound alone.
        let out = aterm_uds::deadline::output_within(&mut alone, MUST_HAPPEN * 2)
            .expect("hang detector: the body's own run never ended");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains("test result: ok. 1 passed"),
            "{}\n{said}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    // SAFETY: SIG_DFL is a disposition, no handler of ours; this process runs this test alone.
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_DFL) };
    let watch = aterm_toolchain::interrupt::Watch::enter();
    assert_ne!(
        action(libc::SIGTERM),
        libc::SIG_DFL,
        "the watch catches SIGTERM"
    );
    let start = Starting::at_defaults(&[libc::SIGTERM]);
    assert_eq!(action(libc::SIGTERM), libc::SIG_DFL, "the start state set");
    drop(watch);
    drop(start);
    assert_eq!(
        action(libc::SIGTERM),
        libc::SIG_DFL,
        "SIGTERM as the watch and the start found it"
    );
}
