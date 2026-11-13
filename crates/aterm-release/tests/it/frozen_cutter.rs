// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linked cutter transport dispatch.

#[test]
fn linked_remote_transport_runs_before_cutter_argument_dispatch() {
    let binary = aterm_tempfile::bin_exe("aterm-release");
    let world = aterm_tempfile::tempdir().unwrap();
    let output = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(binary)
            .args([
                "--remote-agh",
                &std::process::id().to_string(),
                "help",
                "repo",
                "refs",
            ])
            .env("PATH", world.path()),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("agh repo refs"));
}
