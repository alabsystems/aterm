// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test helpers for hanging up control sockets and reading to EOF under a hang detector.

use std::thread::JoinHandle;

use aterm_uds::CtlStream;

/// Hangs up `client` and closes its descriptor.
pub(crate) fn hang_up(client: CtlStream) {
    aterm_uds::hangup::hang_up(&client);
}

/// Joins `join` under [`aterm_spec::wait::MUST_HAPPEN`] after a client hang-up.
#[track_caller]
pub(crate) fn join_after_hang_up<T>(join: JoinHandle<T>, what: &str) -> T {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        join.is_finished().then_some(())
    });
    match join.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Reads `stream` to EOF in polled slices under [`aterm_spec::wait::MUST_HAPPEN`].
#[cfg(unix)]
#[track_caller]
pub(crate) fn read_to_end_of_file(
    stream: &CtlStream,
    report: impl FnOnce(&[u8]) -> String,
) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (mut read, mut chunk) = (Vec::new(), [0u8; 4096]);
    loop {
        let readable =
            crate::control_listener::poll_readable(stream.as_raw_fd(), crate::control::HANGUP_POLL);
        match readable {
            Ok(true) => match (&*stream).read(&mut chunk) {
                Ok(0) => return read,
                Ok(n) => read.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => panic!("read to end-of-file: {e}: {}", report(&read)),
            },
            Ok(false) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("poll for end-of-file: {e}: {}", report(&read)),
        }
        if std::time::Instant::now() >= until {
            panic!(
                "hang detector: no end-of-file within {:?}: {}",
                aterm_spec::wait::MUST_HAPPEN,
                report(&read)
            );
        }
    }
}

/// Reads `stream` to EOF using read timeouts under [`aterm_spec::wait::MUST_HAPPEN`].
#[cfg(not(unix))]
#[track_caller]
pub(crate) fn read_to_end_of_file(
    stream: &CtlStream,
    report: impl FnOnce(&[u8]) -> String,
) -> Vec<u8> {
    use std::io::Read as _;
    stream
        .set_read_timeout(Some(crate::control::HANGUP_POLL))
        .expect("a read slice");
    let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (mut read, mut chunk) = (Vec::new(), [0u8; 4096]);
    loop {
        match (&*stream).read(&mut chunk) {
            Ok(0) => return read,
            Ok(n) => read.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                if std::time::Instant::now() >= until {
                    panic!(
                        "hang detector: no end-of-file within {:?}: {}",
                        aterm_spec::wait::MUST_HAPPEN,
                        report(&read)
                    );
                }
            }
            Err(e) => panic!("read to end-of-file: {e}: {}", report(&read)),
        }
    }
}

#[cfg(unix)]
mod tests {
    use super::read_to_end_of_file;
    use aterm_uds::CtlStream;
    use std::io::Write as _;

    /// peer wrote and its end are there to be read.
    #[test]
    fn an_end_whose_peer_already_closed_reads_to_its_end_of_file() {
        let (near, far) = CtlStream::pair().expect("pair");
        (&far).write_all(b"said\n").expect("the peer writes");
        drop(far);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the peer's last close",
            || aterm_uds::hangup::peer_closed(&near).then_some(()),
        );
        let read = read_to_end_of_file(&near, |read| {
            format!("after {:?}", String::from_utf8_lossy(read))
        });
        assert_eq!(read, b"said\n");
    }
}
