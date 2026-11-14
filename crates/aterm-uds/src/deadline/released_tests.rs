// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_pending_release_waits_and_the_detector_bounds_a_permanent_holder() {
    let start = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let attempts = std::cell::Cell::new(0);
    let answer = released::<()>(
        || {
            attempts.set(attempts.get() + 1);
            Err(io::ErrorKind::WouldBlock.into())
        },
        || start + elapsed.get(),
        || elapsed.set(elapsed.get() + Duration::from_secs(1)),
    );
    assert_eq!(answer.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(elapsed.get(), Duration::from_secs(60));
    assert_eq!(attempts.get(), 61);
    let error = released::<()>(
        || Err(io::ErrorKind::PermissionDenied.into()),
        Instant::now,
        || panic!("a non-contention error must not wait"),
    );
    assert_eq!(error.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[cfg(unix)]
#[test]
fn a_forked_copy_outlives_the_local_drop_until_exec() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    let owner = std::fs::File::create(&path).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "close-only lock reproduces inherited custody"
    )]
    owner.try_lock().unwrap();
    let parked = crate::fork_window::Parked::park();
    drop(owner);
    let probe = std::fs::File::open(&path).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "probe explicitly unlocks after successful take"
    )]
    let attempt = || probe.try_lock().map_err(io::Error::from);
    assert_eq!(attempt().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    let mut child = Some(parked);
    let mut observed = 0;
    once_released(|| {
        observed += 1;
        let answer = attempt();
        if let Some(child) = child.take() {
            assert!(
                answer.is_err(),
                "negative control: the child still owns the copy"
            );
            assert!(child.release().success());
        }
        answer
    });
    probe.unlock().unwrap();
    assert!(observed >= 2, "a single-sample implementation must fail");
}

#[cfg(unix)]
#[test]
fn a_never_released_native_lock_fails_at_the_detector() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("held");
    let owner = std::fs::File::create(&path).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "negative control explicitly unlocks below"
    )]
    owner.try_lock().unwrap();
    let probe = std::fs::File::open(&path).unwrap();
    let start = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let answer = released(
        || {
            #[expect(
                clippy::disallowed_methods,
                reason = "negative control probes an actually held native lock"
            )]
            probe.try_lock().map_err(io::Error::from)
        },
        || start + elapsed.get(),
        || elapsed.set(Duration::from_secs(60)),
    );
    owner.unlock().unwrap();
    assert_eq!(answer.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(elapsed.get(), Duration::from_secs(60));
}

#[cfg(unix)]
#[test]
fn a_pipe_waits_for_the_inherited_writer_without_discarding_bytes() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd as _;
    #[expect(
        clippy::disallowed_methods,
        reason = "the parked fork below must inherit the writer: a kept-out one reaches no fork, \
                  and the read would end at once"
    )]
    let (mut reader, mut writer) = io::pipe().unwrap();
    let fd = reader.as_raw_fd();
    // SAFETY: the test owns this reader; its description has not been shared.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: the same owned descriptor remains live.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    writer.write_all(b"kept").unwrap();
    let parked = crate::fork_window::Parked::park();
    drop(writer);
    let mut bytes = [0; 4];
    assert_eq!(reader.read(&mut bytes).unwrap(), 4);
    assert_eq!(&bytes, b"kept");
    assert_eq!(
        reader.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let mut parked = Some(parked);
    let eof = once_released(|| {
        let answer = reader.read(&mut bytes);
        if let Some(child) = parked.take() {
            assert!(answer.is_err());
            assert!(child.release().success());
        }
        answer
    });
    assert_eq!(eof, 0);
}
