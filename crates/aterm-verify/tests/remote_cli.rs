// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Remote transport dispatch must work without the installed agh on PATH.

#[test]
fn linked_transport_recognizes_current_protocol_without_path_tools() {
    let verify = aterm_tempfile::bin_exe("aterm-verify");
    let world = aterm_tempfile::tempdir().unwrap();
    for args in [
        vec!["help", "repo", "refs"],
        vec!["help", "auth", "git-credential"],
    ] {
        let output = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&verify)
                .args(["--remote-agh", &std::process::id().to_string()])
                .args(&args)
                .env("PATH", world.path()),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(&args[1..].join(" ")));
    }
}

#[test]
fn credential_callback_is_separate_from_transport_parent_metadata() {
    let world = aterm_tempfile::tempdir().unwrap();
    for operation in ["erase", "capability"] {
        let output = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-verify"))
                .args([
                    "--remote-agh",
                    "credential",
                    "auth",
                    "git-credential",
                    operation,
                ])
                .env("HOME", world.path())
                .env("XDG_CONFIG_HOME", world.path().join("config"))
                .env("XDG_STATE_HOME", world.path().join("state")),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"quit=1\n");
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_live_process_other_than_the_captured_parent_cannot_own_the_transport() {
    let wrong = aterm_uds::process::read_process_parent(std::process::id() as i32).unwrap();
    assert!(wrong > 1 && wrong != std::process::id());
    let output = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-verify")).args([
            "--remote-agh",
            &wrong.to_string(),
            "version",
        ]),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(aterm_verify::exit::COULD_NOT_RUN)
    );
    assert!(output.stdout.is_empty(), "agh must not execute: {output:?}");
}
