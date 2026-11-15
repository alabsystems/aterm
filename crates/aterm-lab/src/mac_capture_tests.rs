// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::Path;
use std::time::Instant;

use aterm_uds::deadline::{MAX_COMPLETE_STDOUT_BYTES, OUTPUT_KEEP, output_until, stand_in};
use aterm_uds::spawnfd::Command;

fn binary_bytes(length: usize, first: &[u8]) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0u8..=255).cycle().take(length).collect();
    bytes[..first.len()].copy_from_slice(first);
    bytes
}

fn emitter(stdout: &Path, stderr: &Path) -> Command {
    let mut command = aterm_uds::spawnfd::command(stand_in(Path::new("/bin/sh")));
    command
        .args([
            "-c",
            r#""$1" "$2"; "$1" "$3" >&2; exit 1"#,
            "runtime-capture",
        ])
        .arg(stand_in(Path::new("/bin/cat")))
        .arg(stdout)
        .arg(stderr);
    command
}

#[test]
fn complete_runtime_capture_keeps_both_large_binary_streams_and_the_failed_exit() {
    let world = aterm_tempfile::tempdir().unwrap();
    let stdout_file = world.path().join("stdout");
    let stderr_file = world.path().join("stderr");
    let stdout = binary_bytes(OUTPUT_KEEP + 1025, b"stdout-start\0\xff");
    let stderr = binary_bytes(OUTPUT_KEEP + 2047, b"stderr-start\0\xfe");
    std::fs::write(&stdout_file, &stdout).unwrap();
    std::fs::write(&stderr_file, &stderr).unwrap();

    let out = super::capture_output(
        &mut emitter(&stdout_file, &stderr_file),
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, stdout);
    assert_eq!(out.stderr, stderr);

    // Negative control: the former transport accepts these same streams but loses their starts.
    let tailed = output_until(
        &mut emitter(&stdout_file, &stderr_file),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(tailed.status.code(), Some(1));
    assert_eq!(
        tailed.stdout.as_slice(),
        &stdout[stdout.len() - OUTPUT_KEEP..]
    );
    assert_eq!(
        tailed.stderr.as_slice(),
        &stderr[stderr.len() - OUTPUT_KEEP..]
    );
    assert_ne!(tailed.stdout, out.stdout);
    assert_ne!(tailed.stderr, out.stderr);
}

#[test]
fn runtime_capture_refuses_either_oversized_stream_instead_of_returning_a_tail() {
    let world = aterm_tempfile::tempdir().unwrap();
    let oversized_file = world.path().join("oversized");
    let empty_file = world.path().join("empty");
    std::fs::write(
        &oversized_file,
        binary_bytes(MAX_COMPLETE_STDOUT_BYTES + 1, b"oversized-start\0\xff"),
    )
    .unwrap();
    std::fs::write(&empty_file, b"").unwrap();

    for (stream, stdout, stderr) in [
        ("stdout", &oversized_file, &empty_file),
        ("stderr", &empty_file, &oversized_file),
    ] {
        let why =
            super::capture_output(&mut emitter(stdout, stderr), aterm_spec::wait::MUST_HAPPEN)
                .expect_err("an oversized stream supplies no complete output");
        assert!(why.contains(stream), "{why}");
        assert!(
            why.contains(&MAX_COMPLETE_STDOUT_BYTES.to_string()),
            "{why}"
        );
    }
}
