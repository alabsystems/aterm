// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipped account host's hold on the signals that end a window
//! (`aterm_agent::harness::cli::WindowEnds`). This binary links the library as the CLI ships
//! it, with no `cfg(test)`, so [`LiveAccounts`]'s `hold_window_ends` here is the one that
//! `accounts add`, `accounts adopt-default`, `harness agy login`, `signin --cli grok` and
//! `accounts browser` take from the host the CLI's `run` passes. The unit-test build's
//! `LiveAccounts` holds none, and the trait's default holds none too.

use aterm_agent::harness::cli::{AccountsHost as _, LiveAccounts};

/// Set, [`the_shipped_account_host_holds_the_end_signals_until_its_hold_drops`] is the child
/// its own run started. The child reads the hold instead of starting another child.
const CHILD: &str = "ATERM_TEST_LIVE_WINDOW_ENDS";

/// What SIGTERM does in this process now: `default`, `ignored` or `handled`.
fn sigterm_action() -> &'static str {
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask, no flags).
    let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action changes nothing; SIGTERM's current one is read into `now`.
    if unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut now) } != 0 {
        return "unread";
    }
    match now.sa_sigaction {
        libc::SIG_DFL => "default",
        libc::SIG_IGN => "ignored",
        _ => "handled",
    }
}

/// THE SHIPPED ACCOUNT HOST HOLDS THE END SIGNALS UNTIL ITS HOLD DROPS. Under
/// `LiveAccounts::default().hold_window_ends()`, SIGTERM has a handler, which only notes the
/// end, so the verb cancels instead of the process ending mid-step. Once the hold drops,
/// SIGTERM is back at its default action. The suite binary runs again as its own child with
/// this test alone, and every signal starts at its default action there ([`CHILD`]). No other
/// test of the suite can change what the child reads. CONTROL: before the hold, the same read
/// finds the default action. NEGATIVE CONTROL: with `LiveAccounts`'s `hold_window_ends`
/// removed (the trait's default), the read under the hold finds `default`.
#[test]
fn the_shipped_account_host_holds_the_end_signals_until_its_hold_drops() {
    if std::env::var_os(CHILD).is_some() {
        let before = sigterm_action();
        let held = LiveAccounts::default().hold_window_ends();
        let under = sigterm_action();
        drop(held);
        let after = sigterm_action();
        assert_eq!(
            [before, under, after],
            ["default", "handled", "default"],
            "SIGTERM before, under and after the shipped host's hold"
        );
        return;
    }
    let name = format!(
        "{}::the_shipped_account_host_holds_the_end_signals_until_its_hold_drops",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let exe = std::env::current_exe().expect("test exe");
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["--exact", &name, "--test-threads=1"])
        .env(CHILD, "1")
        // Every signal starts at its default action, whatever this binary's other tests hold now.
        .default_signals()
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let ran = aterm_uds::deadline::output_while_process_progressing(
        &mut cmd,
        aterm_spec::wait::MUST_HAPPEN,
        "the live hold's child",
    )
    .expect("the child ran");
    let told = format!(
        "{}{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(
        ran.status.success(),
        "the child failed ({:?}):\n{told}",
        ran.status
    );
    assert!(told.contains("1 passed"), "the child ran no test:\n{told}");
}
