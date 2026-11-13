// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;

fn assert_closed(fd: libc::c_int) {
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}

fn low_descriptors() -> usize {
    (0..256)
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .count()
}

#[test]
fn closed_capture_observations_keep_all_128_prefixes_without_reader_descriptors() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_query::capture::tests::closed_capture_observations_keep_all_128_prefixes_without_reader_descriptors",
    ) {
        return;
    }
    // A complete actual pipe initializes the same descriptor path before
    // measurement. No OS limit, concurrency or selected-input owner changes.
    let (mut warm, writer) = Capture::pipe().unwrap();
    drop(writer);
    warm.poll().unwrap();
    drop(warm.into_observation());
    let before = low_descriptors();
    let mut observations = Vec::new();
    for index in 0..128_u8 {
        let (mut capture, mut writer) = Capture::pipe().unwrap();
        let fd = capture.reader.as_raw_fd();
        assert!(
            fd < 256,
            "this bounded native control measures the unchanged low table"
        );
        let bytes = [index, 0, 255, b'\n'];
        writer.write_all(&bytes).unwrap();
        drop(writer);
        capture.poll().unwrap();
        assert!(capture.complete());
        let observation = capture.into_observation();
        assert_closed(fd);
        assert_eq!(observation.prefix, bytes);
        assert_eq!(observation.bytes_seen, Some(bytes.len() as u64));
        assert!(observation.eof && !observation.failed);
        observations.push(observation);
        assert_eq!(low_descriptors(), before);
    }
    assert_eq!(observations.len(), 128);
    assert_eq!(low_descriptors(), before);
    for (index, observation) in observations.into_iter().enumerate() {
        assert_eq!(observation.prefix, [index as u8, 0, 255, b'\n']);
    }
}

#[test]
fn actual_child_owning_wait_and_pipe_eof_preserve_bytes_after_reader_close() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_query::capture::tests::actual_child_owning_wait_and_pipe_eof_preserve_bytes_after_reader_close",
    ) {
        return;
    }
    // This is a real native owning-wait/output control, not an SDK Keeper,
    // NoForkTarget, paid Epoch borrower, Host completion or Ready fixture.
    let (mut capture, writer) = Capture::pipe().unwrap();
    let fd = capture.reader.as_raw_fd();
    let mut command = aterm_uds::spawnfd::command("/usr/bin/printf");
    command
        .env_clear()
        .arg(r"native\000\377")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::from(writer))
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut child = command.spawn().unwrap();
    drop(command);
    let actual = child.wait_with_rusage().unwrap();
    assert!(actual.status.success() && child.is_reaped());
    capture.poll().unwrap();
    assert!(capture.complete());
    let observation = capture.into_observation();
    assert_closed(fd);
    assert_eq!(observation.prefix, b"native\0\xff");
    assert_eq!(observation.bytes_seen, Some(8));
    assert!(observation.eof && !observation.failed);
    assert!(
        actual.status.success(),
        "actual wait status remains separate from FD cleanup"
    );
}

#[test]
fn overflow_snapshot_preserves_seen_total_eof_and_failed_prefix() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_query::capture::tests::overflow_snapshot_preserves_seen_total_eof_and_failed_prefix",
    ) {
        return;
    }
    // Physical EOF requires the last native writer to close. Create this
    // pipe only in the owned process, where a parallel test cannot fork
    // while its writer is open and retain another copy before exec.
    let (mut capture, mut writer) = Capture::pipe().unwrap();
    let chunk = [0x67; wire::MAX_CHUNK];
    for _ in 0..wire::MAX_OUTPUT as usize / chunk.len() {
        writer.write_all(&chunk).unwrap();
        capture.poll().unwrap();
    }
    writer.write_all(b"x").unwrap();
    assert!(capture.poll().is_err());
    drop(writer);
    capture.poll().unwrap();
    let observation = capture.into_observation();
    assert_eq!(observation.bytes_seen, Some(wire::MAX_OUTPUT + 1));
    assert_eq!(observation.prefix, vec![0x67; wire::MAX_OUTPUT as usize]);
    assert!(observation.eof && observation.failed);
}
