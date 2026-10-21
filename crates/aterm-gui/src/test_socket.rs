// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: hang a socket client up the way its process exit does, and join
//! what served it with a hang detector.
//!
//! `drop` closes a descriptor, and a socket closes only with its LAST one. In
//! this lib-test process sibling tests fork all the time (forkpty, `pre_exec`
//! commands, `ParkedChild`, raw `fork`), and each child holds a copy of every
//! descriptor until it execs: a window measured past ten seconds at load ~150
//! for a child demoted to the Darwin background band before its exec (macOS
//! 26.6 does not honor `FD_CLOFORK`). Worse, std on macOS marks a socketpair,
//! a `socket` and an `accept`ed stream close-on-exec only AFTER creating it, so
//! a fork in that instant hands over a copy that outlives the exec too. While a
//! copy lives, the server end of a dropped client reads no EOF and its writes
//! still succeed — into the 8 KiB buffer, then they block (measured on macOS
//! 26.6). A push loop that waited for a write to fail therefore pushed every
//! teardown frame into a dead socket, went quiet, and parked on its 250 ms
//! tick for good: a 25-minute hang of a `sessions`-stream subscription test on
//! 2026-09-30. A relay's client-to-child pump, waiting for that EOF, parks the
//! same way.
//!
//! [`hang_up`] acts on the socket instead, standing in for a real `aterm ctl`
//! client's exit. It does not stand in for a probe that needs the last close
//! itself (the lanes' `POLLHUP` check, `aterm_uds::hangup`); a test waits for
//! that as a fact (`control_hangup_tests`).

use std::thread::JoinHandle;

use aterm_uds::CtlStream;

/// Hang up a test's client for the whole socket by the production rule
/// ([`aterm_uds::hangup::hang_up`]), then close this descriptor: the server end
/// reads EOF at once whatever copies of the client end survive, and its writes
/// are discarded without blocking.
pub(crate) fn hang_up(client: CtlStream) {
    aterm_uds::hangup::hang_up(&client);
}

/// Join a thread that must end once its client hung up (a push loop, a relay,
/// a parked verb) with a hang detector of [`aterm_spec::wait::MUST_HAPPEN`],
/// not an unbounded `join`. Its value comes back; a panic inside it is
/// re-raised as itself.
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

/// Everything `stream` reads up to its end-of-file, read in
/// [`crate::control::HANGUP_POLL`] slices under a hang detector
/// ([`aterm_spec::wait::MUST_HAPPEN`]): the wait for a peer's end-of-file
/// re-reads after every slice instead of parking one read on the wake. Darwin
/// can leave a read that has just taken the peer's bytes asleep through the
/// peer's `shutdown(Write)` that follows them, though the end-of-file is there
/// for the next read: the relay had copied the answer and shut its end within
/// 0.2 ms, and the read slept out its whole minute (1 run in about 3,300 at
/// load ~20, measured 2026-10-17); a bare std socketpair does the same (7 of
/// 320,000 reads at load ~17), and reads with no timeout at all stayed parked
/// (three of eight workers, still asleep ten minutes on when killed), while a
/// `poll` for the same end-of-file lost none (0 of 480,000 beside the read's 8
/// of 480,000, load 13-15, measured 2026-10-17). The relay's own pumps poll
/// ([`crate::proxy`]'s `readable_unless_stopped`).
///
/// Each slice is a `poll`, then a read only once the end is readable: no
/// socket option is set on `stream`, because Darwin refuses one (`setsockopt`
/// `EINVAL`) on a socket whose peer is already gone — the end this wait is
/// for, there to be read. A sibling that accepted a forward which had
/// already closed failed there under load, before it read a byte.
///
/// When the hang detector fires, `report` (given what was read) says how far
/// the other side got.
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
            // Readable (bytes, the end, or an error): the read returns at once.
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

/// [`read_to_end_of_file`] where there is no `poll`: the stream's read
/// timeout makes the slices.
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

    /// A reader may start after its peer is gone: a forward that found no
    /// token closed before the sibling's thread accepted it. Darwin then
    /// refuses any socket option on the reading end (`EINVAL`), yet what the
    /// peer wrote and its end are there to be read.
    #[test]
    fn an_end_whose_peer_already_closed_reads_to_its_end_of_file() {
        let (near, far) = CtlStream::pair().expect("pair");
        (&far).write_all(b"said\n").expect("the peer writes");
        drop(far);
        // The close is the last one once no fork elsewhere holds a copy of
        // `far`: a fact that must happen, waited for with a hang detector.
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
