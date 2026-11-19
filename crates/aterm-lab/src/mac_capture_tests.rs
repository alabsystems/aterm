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

#[test]
fn retirement_transport_requires_complete_streams_and_the_actual_failed_status() {
    let world = aterm_tempfile::tempdir().unwrap();
    let stdout_file = world.path().join("stdout");
    let stderr_file = world.path().join("stderr");
    let stdout = binary_bytes(4097, b"observer\0\xff");
    let stderr = binary_bytes(8193, b"diagnostic\0\xfe");
    std::fs::write(&stdout_file, &stdout).unwrap();
    std::fs::write(&stderr_file, &stderr).unwrap();
    let output = super::capture_retirement_output(
        &mut emitter(&stdout_file, &stderr_file),
        8193,
        |_| {},
        || false,
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, stdout);
    assert_eq!(output.stderr, stderr);
    for (out, err) in [(&stdout_file, &stderr_file), (&stderr_file, &stdout_file)] {
        assert!(
            super::capture_retirement_output(&mut emitter(out, err), 4096, |_| {}, || false)
                .is_err()
        );
    }
}

#[test]
fn cancelled_retirement_transport_reaps_its_actual_local_child_without_claiming_guest_closure() {
    let world = aterm_tempfile::tempdir().unwrap();
    let marker = world.path().join("observer-started");
    let mut command = aterm_uds::spawnfd::command(stand_in(Path::new("/bin/sh")));
    command
        .args(["-c", "printf '%s\\n' $$ > \"$1\"; \"$2\" 1000", "observer"])
        .arg(&marker)
        .arg(stand_in(Path::new("/bin/sleep")));
    let actual_pid = std::cell::Cell::new(None);
    let mut original = None;
    let mut probe = aterm_uds::deadline::ProcessProgressProbe::default();
    let mut progress =
        aterm_uds::deadline::ProgressWindow::new(aterm_spec::wait::MUST_HAPPEN, Instant::now());
    let result = super::capture_retirement_output(
        &mut command,
        4096,
        |pid| actual_pid.set(Some(pid)),
        || {
            let Some(pid) = actual_pid.get() else {
                // The canonical collector also asks about cancellation before spawning.
                return false;
            };
            if std::fs::read(&marker).ok().as_deref() == Some(format!("{pid}\n").as_bytes()) {
                original = aterm_uds::process::read_identity(pid as i32);
                if original.is_some() {
                    return true;
                }
            }
            let work = probe.read(pid);
            assert!(
                !progress.observe(Instant::now(), 0, || work).is_zero(),
                "the actual observer made no native progress before its complete self-write"
            );
            false
        },
    );
    assert!(result.unwrap_err().contains("cancelled"));
    let original = original.expect("cancellation follows an observed actual child");
    let mut status = 0;
    // SAFETY: this known child has already been retired by the owning collector. WNOHANG
    // cannot wait; ECHILD proves no unreaped original remains even if its PID was reused.
    #[expect(
        unsafe_code,
        reason = "nonblocking wait proves the actual owned collector already reaped its child"
    )]
    let waited = unsafe { libc::waitpid(original.pid, &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_ne!(
        aterm_uds::process::read_identity(original.pid),
        Some(original)
    );
}
