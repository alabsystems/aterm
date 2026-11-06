// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab` serves the tether's verb (`aterm_uds::tether::VERB`) ahead of its `argv[0]`
//! dispatch, so a child it spawns tethered can start: `<aterm-lab> __tether <parent pid> <program>
//! <argv0> [args…]`, under any `argv[0]`, ties itself to that parent, then execs the program in its
//! own place. Told a pid that is not its parent, it starts nothing.

#![cfg(target_os = "macos")]

use std::time::Instant;

use aterm_spec::wait::MUST_HAPPEN;

/// `aterm-lab __tether <parent> /bin/sh /bin/sh -c 'echo ran'` under the `argv[0]` `name`, run
/// as this test's own child.
fn tether_through_the_lab(name: &str, parent: &str) -> std::process::Output {
    let lab = aterm_tempfile::bin_exe("aterm-lab");
    aterm_spec::wait::first_exec(&lab, &["--help"]);
    aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(&lab)
            .arg0(name)
            .args([aterm_uds::tether::VERB, parent])
            .args(["/bin/sh", "/bin/sh", "-c", "echo ran"]),
        Instant::now() + MUST_HAPPEN,
    )
    .expect("run the lab's tether verb")
}

/// Told this test's pid, its parent's, the tether runs the program, under the `argv[0]` a tethered
/// spawn gives it (`aterm_uds::tether::NAME`) and under one the lab's `argv[0]` dispatch takes as
/// a shell (`zsh`); told pid 1, which is no parent it may be tied to, it exits `UNTETHERED` and the
/// program never runs.
#[test]
fn the_lab_serves_the_tether_verb() {
    for name in [aterm_uds::tether::NAME, "zsh"] {
        let out = tether_through_the_lab(name, &std::process::id().to_string());
        assert_eq!(
            out.status.code(),
            Some(0),
            "told its parent's pid under argv[0] {name}, the tether exits as its program does: \
             {out:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "ran\n",
            "told its parent's pid under argv[0] {name}, the tether runs its program: {out:?}"
        );
    }
    let stranger = tether_through_the_lab(aterm_uds::tether::NAME, "1");
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
