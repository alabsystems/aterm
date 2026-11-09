// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;
use std::{
    io::Read as _,
    os::fd::AsRawFd as _,
    time::{Duration, Instant},
};

#[test]
fn original_query_pipe_requires_physical_eof_and_preserves_binary_prefix() {
    let (mut capture, mut writer) = capture::Capture::pipe().unwrap();
    let bytes = [0, 255, b'\n', b'\r', 0, b'x'];
    writer.write_all(&bytes).unwrap();
    capture.poll().unwrap();
    assert_eq!(capture.prefix(), bytes);
    assert!(!capture.complete(), "available bytes do not imply pipe EOF");
    drop(writer);
    capture.poll().unwrap();
    assert!(capture.complete());
    assert_eq!(capture.prefix(), bytes);
}

#[test]
fn overflow_remains_incomplete_after_eof_and_keeps_original_bounded_prefix() {
    let (mut capture, mut writer) = capture::Capture::pipe().unwrap();
    let chunk = [0xa5; wire::MAX_CHUNK];
    // Bound the real pipe population: no large synthetic unrelated FD reserve
    // or blocking writer is needed to exercise the native capture boundary.
    for _ in 0..wire::MAX_OUTPUT as usize / chunk.len() {
        writer.write_all(&chunk).unwrap();
        capture.poll().unwrap();
    }
    writer.write_all(&[0x77]).unwrap();
    assert!(capture.poll().is_err());
    assert_eq!(capture.prefix(), vec![0xa5; wire::MAX_OUTPUT as usize]);
    drop(writer);
    capture.poll().unwrap();
    assert!(
        !capture.complete(),
        "late EOF cannot certify a truncated query"
    );
}

#[test]
fn stdout_and_stderr_are_separate_original_pipes() {
    let (mut stdout, mut out) = capture::Capture::pipe().unwrap();
    let (mut stderr, mut err) = capture::Capture::pipe().unwrap();
    out.write_all(b"out\0").unwrap();
    err.write_all(b"err\xff").unwrap();
    drop(out);
    stdout.poll().unwrap();
    stderr.poll().unwrap();
    assert!(stdout.complete());
    assert!(!stderr.complete());
    assert_eq!(stdout.prefix(), b"out\0");
    assert_eq!(stderr.prefix(), b"err\xff");
    drop(err);
    stderr.poll().unwrap();
    assert!(stderr.complete());
}

#[test]
fn all_128_completed_transport_snapshots_keep_ack_data_without_socket_descriptors() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_query::tests::all_128_completed_transport_snapshots_keep_ack_data_without_socket_descriptors",
    ) {
        return;
    }
    fn descriptors() -> usize {
        (0..256)
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
            .count()
    }
    fn complete(index: u8) -> delivery::DeliveryObservation {
        let (mut client, server) = aterm_uds::CtlStream::pair().unwrap();
        let fd = server.as_raw_fd();
        assert!(fd < 256);
        let mut delivery =
            delivery::Delivery::frames_for_test(server, vec![vec![index, 0, 255, b'\n']]).unwrap();
        let reader = std::thread::spawn(move || {
            let mut actual = Vec::new();
            client.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, [0, 0, 0, 4, index, 0, 255, b'\n']);
            client.write_all(&[0, 0, 0, 1, 1]).unwrap();
            // This exact peer drops only after real write EOF, raw bytes and
            // framed ACK. No status/Exit/Ready supplies transport completion.
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while !delivery.poll(deadline).unwrap() {
            std::thread::sleep(Duration::from_millis(1));
        }
        reader.join().unwrap();
        assert!(delivery.control_closed(deadline).unwrap());
        let observation = delivery.observation();
        drop(delivery);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        observation
    }
    // Initialize the actual native socket path before measuring its live
    // descriptor population. Hold ALL128 observations; reduce no denominator.
    drop(complete(255));
    let before = descriptors();
    let mut observations = Vec::new();
    for index in 0..128_u8 {
        observations.push(complete(index));
        assert_eq!(descriptors(), before);
    }
    assert_eq!(observations.len(), 128);
    for observation in observations {
        assert!(observation.write_closed && observation.acknowledged);
        assert_eq!(observation.acknowledgement_prefix, [0, 0, 0, 1, 1]);
        assert!(!observation.failed);
        assert_eq!(observation.remaining_frames, 0);
        assert_eq!(observation.current_frame_sent, 0);
    }
    assert_eq!(descriptors(), before);
    // This is actual transport disposal only. It constructs no QueryOperation,
    // NativeClosed, Completed, SDK Keeper or paid GNU/native-scope authority.
}
