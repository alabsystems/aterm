// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A libtest binary's every kind of case, run by the tests of libtest's JSON format against the
//! real toolchain (`libtest_json_tests`, `live_halt_tests`): this test binary, filtered to
//! `libtest_fixture::`, with [`FIXTURE`] set. Unset, as in any ordinary run, every case passes
//! (a `should_panic` one by panicking as it expects) and none waits.

use std::io::Write as _;
use std::path::PathBuf;

/// Set (to a directory), the cases act: they fail, refuse, run out of disk, and the napper waits.
pub(crate) const FIXTURE: &str = "ATERM_VERIFY_LIBTEST_FIXTURE";

/// The words the plain red case's panic carries, so a test can find its block.
pub(crate) const NONCE: &str = "fixture-nonce-5b1d9c";

fn acting() -> Option<PathBuf> {
    std::env::var_os(FIXTURE).map(PathBuf::from)
}

#[test]
fn b_pass() {}

#[test]
fn c_plain() {
    if acting().is_some() {
        panic!("the planted red {NONCE}");
    }
}

#[test]
fn c_refused() {
    if acting().is_some() {
        aterm_uds::host::could_not_run("the fixture refuses its machine");
    }
}

#[test]
fn d_enospc() {
    if acting().is_some() {
        panic!("writing the fixture: No space left on device (os error 28)");
    }
}

#[test]
#[should_panic(expected = "the wanted words")]
fn e_should_panic_mismatch() {
    if acting().is_some() {
        panic!("other words");
    }
    panic!("the wanted words");
}

#[test]
#[should_panic]
fn e_should_panic_did_not() {
    if acting().is_none() {
        panic!("as expected");
    }
}

#[test]
fn f_returns_err() -> Result<(), String> {
    if acting().is_some() {
        return Err("the fixture's error".into());
    }
    Ok(())
}

#[test]
#[ignore = "the fixture's reason"]
fn g_ignored() {}

#[test]
fn h_ansi_and_raw() {
    if acting().is_some() {
        println!("\x1b[31mcoloured\x1b[0m captured \u{fffd} text");
        // Written past libtest's capture, to fd 1, ending its line.
        let _ = std::io::stdout().write_all(b"uncaptured raw \xff\xfe bytes\n");
        panic!("after ANSI and raw bytes");
    }
}

/// Says it started (`napper.started`, its pid), then waits for `napper.release` in the fixture's
/// directory, failing at the hang detector if it never comes: only a halt ends it sooner.
#[test]
fn z_napper() {
    let Some(dir) = acting() else {
        return;
    };
    std::fs::write(dir.join("napper.started"), std::process::id().to_string())
        .expect("said it started");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the napper's release",
        || dir.join("napper.release").exists().then_some(()),
    );
}
