// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` serves the tether's verb (`aterm_uds::tether::VERB`) as its `main`'s first
//! call, so a child it spawns tethered can start: `<aterm-verify> __tether <parent pid> <program>
//! <argv0> [args…]` ties itself to that parent, then execs the program in its own place. Told a
//! pid that is not its parent, it starts nothing.

#![cfg(unix)]

use std::time::Instant;

use aterm_spec::wait::MUST_HAPPEN;

/// `aterm-verify __tether <parent> /bin/sh /bin/sh -c 'echo ran'` under the `argv[0]` a tethered
/// spawn gives it (`aterm_uds::tether::NAME`), run as this test's own child.
fn tether_through_verify(parent: &str) -> std::process::Output {
    let verify = aterm_tempfile::bin_exe("aterm-verify");
    aterm_spec::wait::first_exec(&verify, &["--help"]);
    aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(&verify)
            .arg0(aterm_uds::tether::NAME)
            .args([aterm_uds::tether::VERB, parent])
            .args(["/bin/sh", "/bin/sh", "-c", "echo ran"]),
        Instant::now() + MUST_HAPPEN,
    )
    .expect("run verify's tether verb")
}

/// Told this test's pid, its parent's, the tether runs the program; told pid 1, which is no
/// parent it may be tied to, it exits `UNTETHERED` and the program never runs.
#[test]
fn verify_serves_the_tether_verb() {
    let out = tether_through_verify(&std::process::id().to_string());
    assert_eq!(
        out.status.code(),
        Some(0),
        "told its parent's pid, the tether exits as its program does: {out:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ran\n",
        "told its parent's pid, the tether runs its program: {out:?}"
    );
    let stranger = tether_through_verify("1");
    assert_eq!(
        stranger.status.code(),
        Some(i32::from(aterm_uds::tether::UNTETHERED)),
        "told pid 1: {stranger:?}"
    );
    assert!(
        stranger.stdout.is_empty(),
        "told pid 1, the program never ran: {stranger:?}"
    );
}
