// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor readiness without consuming bytes: indefinite reads or an absolute-deadline
//! read/write wait. Hangup also wakes the caller, which drains buffered data before EOF.

use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};

/// Run an operation with nonblocking status set on a descriptor, then restore its flags.
/// Duplicated descriptors share these flags; the caller must own their read/write use
/// during this scope. Restoration is also attempted during unwinding.
///
/// # Errors
/// Reading, setting or restoring descriptor flags fails, or the operation fails.
pub fn with_nonblocking<T>(
    fd: BorrowedFd<'_>,
    work: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    // SAFETY: F_GETFL reads the flags of the live borrowed descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    with_nonblocking_by(
        flags,
        |flags| {
            // SAFETY: fd remains borrowed for this scope; F_SETFL takes an integer flag word.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        },
        work,
    )
}

fn with_nonblocking_by<T>(
    flags: i32,
    set: impl Fn(i32) -> io::Result<()>,
    work: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    struct Restore<F: Fn(i32) -> io::Result<()>> {
        set: F,
        flags: i32,
        restored: bool,
    }
    impl<F: Fn(i32) -> io::Result<()>> Restore<F> {
        fn apply(&mut self) -> io::Result<()> {
            if !self.restored {
                (self.set)(self.flags)?;
                self.restored = true;
            }
            Ok(())
        }
    }
    impl<F: Fn(i32) -> io::Result<()>> Drop for Restore<F> {
        fn drop(&mut self) {
            let _ = self.apply();
        }
    }
    let mut restore = Restore {
        set,
        flags,
        restored: false,
    };
    (restore.set)(flags | libc::O_NONBLOCK)?;
    let result = work();
    let restored = restore.apply();
    result.and_then(|value| restored.map(|()| value))
}

/// What [`either`] found ready: at least one is `true`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ready {
    /// The first descriptor is readable, at its end (`POLLHUP`) or in error (`POLLERR`,
    /// `POLLNVAL`).
    pub first: bool,
    /// The second is.
    pub second: bool,
}

/// Blocks, with no timeout, until `first` or `second` is readable, at its end or in error, and
/// says which (both, when both were by the time `poll(2)` returned). Reads nothing, so a byte
/// either holds is still there for its reader. An interrupted `poll(2)` is asked again.
///
/// # Errors
/// `poll(2)`'s errno, other than `EINTR`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn either(first: BorrowedFd<'_>, second: BorrowedFd<'_>) -> io::Result<Ready> {
    pair(first, second, None)?.ok_or_else(|| io::Error::other("indefinite readiness expired"))
}

/// Wait on either descriptor until an absolute deadline. `None` means expiry;
/// bytes and kernel notifications remain for their actual readers. Interrupted
/// waits keep the same deadline. Readability, hangup and error all wake.
/// # Errors
/// A poll error other than interruption.
pub fn either_until(
    first: BorrowedFd<'_>,
    second: BorrowedFd<'_>,
    deadline: std::time::Instant,
) -> io::Result<Option<Ready>> {
    pair(first, second, Some(deadline))
}

#[cfg_attr(trust_verify, trust::skip)]
fn pair(
    first: BorrowedFd<'_>,
    second: BorrowedFd<'_>,
    deadline: Option<std::time::Instant>,
) -> io::Result<Option<Ready>> {
    let mut fds = [read_pollfd(first), read_pollfd(second)];
    let wait = deadline.map_or(Wait::Forever, Wait::Until);
    Ok(poll_set(&mut fds, wait)?.then(|| Ready {
        first: fds[0].revents != 0,
        second: fds[1].revents != 0,
    }))
}

/// Wait for any borrowed descriptor until an absolute deadline, without reading
/// bytes or notifications. Returns the ready indices; an empty vector means
/// expiry. Hangup/error also wake. The caller bounds the descriptor set.
/// # Errors
/// Empty or unrepresentable set, or poll error other than interruption.
pub fn any_until(fds: &[BorrowedFd<'_>], deadline: std::time::Instant) -> io::Result<Vec<usize>> {
    ready_set(fds, |fd| read_pollfd(*fd), Wait::Until(deadline))
}

/// Wait for mixed read/write interests until an absolute deadline, without
/// consuming bytes or notifications. Returns indices into the supplied set;
/// hangup/error also wake, and an empty result means expiry. An already expired
/// deadline asks nothing. The caller bounds the set and retains every descriptor.
/// # Errors
/// Empty or unrepresentable set, or poll error other than interruption.
pub fn any_interests_until(
    interests: &[Watch<'_>],
    deadline: std::time::Instant,
) -> io::Result<Vec<usize>> {
    ready_set(interests, interest_fd, Wait::Until(deadline))
}

/// Ask native readiness once with zero timeout. Like [`any_interests_until`],
/// this consumes no bytes or notifications. An interrupted poll is asked again.
/// Use this for already buffered work that must not hide pending host control.
/// # Errors
/// Empty or unrepresentable set, or poll error other than interruption.
pub fn any_interests_now(interests: &[Watch<'_>]) -> io::Result<Vec<usize>> {
    ready_set(interests, interest_fd, Wait::Now)
}

fn interest_fd(watch: &Watch<'_>) -> libc::pollfd {
    libc::pollfd {
        fd: watch.fd.as_raw_fd(),
        events: match watch.interest {
            Interest::Read => libc::POLLIN,
            Interest::Write => libc::POLLOUT,
        },
        revents: 0,
    }
}

fn ready_set<T>(
    watches: &[T],
    mut native: impl FnMut(&T) -> libc::pollfd,
    wait: Wait,
) -> io::Result<Vec<usize>> {
    if watches.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty readiness set",
        ));
    }
    // Most waits watch a socket and a bell. Keep their native descriptors on
    // the stack; larger dynamic sets retain the same heap-backed behavior.
    let mut stack = [libc::pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; 8];
    if watches.len() <= stack.len() {
        let fds = &mut stack[..watches.len()];
        for (fd, watch) in fds.iter_mut().zip(watches) {
            *fd = native(watch);
        }
        ready_indices(fds, wait)
    } else {
        let mut fds: Vec<_> = watches.iter().map(native).collect();
        ready_indices(&mut fds, wait)
    }
}

fn ready_indices(fds: &mut [libc::pollfd], wait: Wait) -> io::Result<Vec<usize>> {
    if !poll_set(fds, wait)? {
        return Ok(Vec::new());
    }
    Ok(fds
        .iter()
        .enumerate()
        .filter_map(|(i, fd)| (fd.revents != 0).then_some(i))
        .collect())
}

#[derive(Clone, Copy)]
enum Wait {
    Forever,
    Until(std::time::Instant),
    Now,
}

fn read_pollfd(fd: BorrowedFd<'_>) -> libc::pollfd {
    libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }
}

#[cfg_attr(trust_verify, trust::skip)]
fn poll_set(fds: &mut [libc::pollfd], wait: Wait) -> io::Result<bool> {
    let count = fds
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "readiness set too large"))?;
    loop {
        let timeout = match wait {
            Wait::Forever => -1,
            Wait::Now => 0,
            Wait::Until(deadline) => {
                let Some(remaining) = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|n| !n.is_zero())
                else {
                    return Ok(false);
                };
                let millis = remaining.as_millis()
                    + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
                millis.min(i32::MAX as u128) as i32
            }
        };
        for fd in &mut *fds {
            fd.revents = 0;
        }
        // SAFETY: initialized pollfds outlive the call and name descriptors kept
        // open by their callers' borrows. Poll retains no pointer; count matches.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds.iter().any(|fd| fd.revents != 0) {
            return Ok(true);
        }
        if matches!(wait, Wait::Now) {
            return Ok(false);
        }
    }
}

/// Blocks, with no timeout, until `fd` is readable, at its end or in error: [`either`] of it and
/// itself. A listener is readable once a connection waits to be accepted.
///
/// # Errors
/// `poll(2)`'s errno, other than `EINTR`.
pub fn one(fd: BorrowedFd<'_>) -> io::Result<()> {
    either(fd, fd).map(drop)
}

/// Which nonblocking operation a deadline wait precedes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interest {
    Read,
    Write,
}

/// Opaque borrowed readiness interest. A holder can wait for a descriptor but
/// cannot extract it to read, write, close or duplicate the original endpoint.
#[derive(Clone, Copy)]
pub struct Watch<'a> {
    fd: BorrowedFd<'a>,
    interest: Interest,
}

impl std::fmt::Debug for Watch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("interest", &self.interest)
            .finish_non_exhaustive()
    }
}

impl<'a> Watch<'a> {
    /// Observe readiness for one operation on a descriptor retained by its owner.
    #[must_use]
    pub const fn new(fd: BorrowedFd<'a>, interest: Interest) -> Self {
        Self { fd, interest }
    }
}

/// Wait for `interest`, hangup or error without changing socket timeout options.
/// Use a nonblocking operation after this call: readiness can be consumed by
/// another reader/writer. Interrupted waits retain the original deadline. A deadline already
/// past asks nothing and fails; one that passes during the wait still reports what `poll(2)` saw.
///
/// # Errors
/// Expiry, an invalid descriptor, or `poll(2)` errors other than interruption.
#[cfg_attr(trust_verify, trust::skip)]
pub fn until(
    fd: BorrowedFd<'_>,
    interest: Interest,
    deadline: std::time::Instant,
) -> io::Result<()> {
    if std::time::Instant::now() >= deadline {
        return Err(expired());
    }
    asked_until(fd, interest, deadline)
}

fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "readiness deadline")
}

/// [`until`], but `poll(2)` is asked at least once however late this is, and what it saw comes
/// before the deadline: the deadline only ends a wait that found nothing.
#[cfg_attr(trust_verify, trust::skip)]
fn asked_until(
    fd: BorrowedFd<'_>,
    interest: Interest,
    deadline: std::time::Instant,
) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let millis =
            remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        let timeout = millis.min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: match interest {
                Interest::Read => libc::POLLIN,
                Interest::Write => libc::POLLOUT,
            },
            revents: 0,
        };
        // SAFETY: the initialized pollfd remains live and its borrowed descriptor stays open.
        let result = unsafe { libc::poll(&raw mut descriptor, 1, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid readiness descriptor",
            ));
        }
        if descriptor.revents != 0 {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(expired());
        }
    }
}

/// Waits in `poll(2)` until a read of `stream` has something to return, bytes, its end or an
/// error, within the stream's `SO_RCVTIMEO`, or for as long as it takes with none. Takes nothing
/// from the stream. The read after it does not sleep, and that is the point: on Darwin a read
/// asleep in `recv(2)` on a Unix socket can miss a `shutdown(2)` of the connection and sleep on,
/// and past its own timeout when this end was the one shut, where a wait in `poll(2)` sees it
/// (`docs/measured/darwin-unix-shutdown-wakeup-2026-11-03.md`). A nonblocking stream waits here
/// too, so one that means to come back at once reads without this.
///
/// # Errors
/// `WouldBlock` once the timeout passes with nothing to read, as the read itself would say;
/// `InvalidInput` for a descriptor `poll(2)` calls invalid, on a stream with a timeout;
/// `poll(2)`'s errno; or the error of asking the stream for its timeout.
#[cfg_attr(trust_verify, trust::skip)]
pub fn for_read(stream: &crate::CtlStream) -> io::Result<()> {
    use std::os::fd::AsFd as _;
    let deadline = stream
        .read_timeout()?
        .and_then(|wait| std::time::Instant::now().checked_add(wait));
    let Some(deadline) = deadline else {
        return one(stream.as_fd());
    };
    asked_until(stream.as_fd(), Interest::Read, deadline).map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            io::ErrorKind::WouldBlock.into()
        } else {
            error
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn mixed_readiness_reports_writable_readable_and_hangup_without_consuming_bytes() {
        use super::*;
        use std::{
            io::{Read as _, Write as _},
            os::fd::AsFd as _,
            time::Instant,
        };
        let (quiet, _keep) = std::io::pipe().unwrap();
        let (mut sink, mut writable) = std::io::pipe().unwrap();
        let (mut readable, mut source) = std::io::pipe().unwrap();
        let (mut ended, close) = std::io::pipe().unwrap();
        source.write_all(b"r").unwrap();
        drop(close);
        ended_pipe(ended.as_fd());
        let interests = [
            Watch::new(quiet.as_fd(), Interest::Read),
            Watch::new(writable.as_fd(), Interest::Write),
            Watch::new(readable.as_fd(), Interest::Read),
            Watch::new(ended.as_fd(), Interest::Read),
        ];
        assert_eq!(any_interests_now(&interests).unwrap(), [1, 2, 3]);
        assert_eq!(
            any_interests_until(&interests, Instant::now() + HANG).unwrap(),
            [1, 2, 3]
        );
        assert!(
            any_interests_until(&interests, Instant::now())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            any_interests_until(&[], Instant::now() + HANG)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        let mut byte = [0];
        readable.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"r");
        writable.write_all(b"w").unwrap();
        sink.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"w");
        assert_eq!(ended.read(&mut byte).unwrap(), 0);
        assert!(
            any_interests_now(&[Watch::new(quiet.as_fd(), Interest::Read)])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            any_interests_now(&[]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn a_blocked_writer_is_not_reported_as_readiness_work() {
        use super::*;
        use std::{
            io::{Read as _, Write as _},
            os::fd::AsFd as _,
            time::Instant,
        };
        let (mut drain, mut writer) = std::io::pipe().unwrap();
        let flags = writer.try_clone().unwrap();
        with_nonblocking(flags.as_fd(), || {
            // Fill the actual pipe completely, including any partial final page.
            let mut written = 0;
            loop {
                match writer.write(b"x") {
                    Ok(1) => {
                        written += 1;
                        assert!(written <= 8 * 1024 * 1024);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    other => panic!("filling private pipe: {other:?}"),
                }
            }
            let (mut hint, mut sender) = std::io::pipe().unwrap();
            sender.write_all(b"h")?;
            assert_eq!(
                any_interests_until(
                    &[
                        Watch::new(hint.as_fd(), Interest::Read),
                        Watch::new(writer.as_fd(), Interest::Write),
                    ],
                    Instant::now() + HANG
                )?,
                [0]
            );
            let mut byte = [0];
            hint.read_exact(&mut byte)?;
            assert_eq!(byte, *b"h");
            let mut bytes = vec![0; written];
            drain.read_exact(&mut bytes)?;
            assert!(bytes.iter().all(|byte| *byte == b'x'));
            assert_eq!(
                any_interests_until(
                    &[Watch::new(writer.as_fd(), Interest::Write)],
                    Instant::now() + HANG
                )?,
                [0]
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn many_readiness_preserves_bytes_and_reports_all_ready_indices() {
        use super::*;
        use std::{
            io::{Read as _, Write as _},
            os::fd::AsFd as _,
        };
        let (mut first, mut writer) = std::io::pipe().unwrap();
        let (second, _quiet) = std::io::pipe().unwrap();
        let (third, closed) = std::io::pipe().unwrap();
        writer.write_all(b"x").unwrap();
        drop(closed);
        ended_pipe(third.as_fd());
        let found = any_until(
            &[first.as_fd(), second.as_fd(), third.as_fd()],
            std::time::Instant::now() + HANG,
        )
        .unwrap();
        assert_eq!(found, [0, 2]);
        let mut byte = [0];
        first.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
        assert!(
            any_until(&[second.as_fd()], std::time::Instant::now())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            any_until(&[], std::time::Instant::now() + HANG)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn readiness_stack_and_spill_preserve_indices_bytes_and_hangup() {
        use super::*;
        use std::io::{Read as _, Write as _};
        use std::os::fd::AsFd as _;
        use std::time::Instant;

        for size in [1, 8, 9, 32] {
            let (mut readers, mut writers): (Vec<_>, Vec<_>) =
                (0..size).map(|_| std::io::pipe().unwrap()).unzip();
            for writer in &mut writers {
                writer.write_all(b"x").unwrap();
            }
            {
                let fds: Vec<_> = readers.iter().map(|reader| reader.as_fd()).collect();
                let interests: Vec<_> = readers
                    .iter()
                    .map(|reader| Watch::new(reader.as_fd(), Interest::Read))
                    .collect();
                let expected: Vec<_> = (0..size).collect();
                assert_eq!(any_until(&fds, Instant::now() + HANG).unwrap(), expected);
                assert_eq!(any_interests_now(&interests).unwrap(), expected);
                assert!(any_until(&fds, Instant::now()).unwrap().is_empty());
                assert!(
                    any_interests_until(&interests, Instant::now())
                        .unwrap()
                        .is_empty()
                );
            }
            for reader in &mut readers {
                let mut byte = [0];
                reader.read_exact(&mut byte).unwrap();
                assert_eq!(byte, *b"x");
            }
            let interests: Vec<_> = readers
                .iter()
                .map(|reader| Watch::new(reader.as_fd(), Interest::Read))
                .collect();
            assert!(any_interests_now(&interests).unwrap().is_empty());
            drop(writers);
            for reader in &readers {
                ended_pipe(reader.as_fd());
            }
            assert_eq!(
                any_interests_now(&interests).unwrap(),
                (0..size).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn pair_deadline_preserves_bytes_and_hangup_still_wakes() {
        use super::*;
        use std::{
            io::{Read as _, Write as _},
            os::fd::AsFd as _,
            time::Instant,
        };
        let (mut first, mut writer) = std::io::pipe().unwrap();
        let (second, other) = std::io::pipe().unwrap();
        writer.write_all(b"x").unwrap();
        assert!(
            either_until(first.as_fd(), second.as_fd(), Instant::now())
                .unwrap()
                .is_none()
        );
        let found = either_until(first.as_fd(), second.as_fd(), Instant::now() + HANG)
            .unwrap()
            .unwrap();
        assert!(found.first);
        let mut byte = [0u8; 1];
        first.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
        drop(other);
        assert!(
            either_until(first.as_fd(), second.as_fd(), Instant::now() + HANG)
                .unwrap()
                .unwrap()
                .second
        );
    }
    #[test]
    fn scoped_nonblocking_restores_flags_after_an_operation_error() {
        use std::io::Read as _;
        use std::os::fd::{AsFd as _, AsRawFd as _};
        let (mut reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let observer = reader.try_clone().unwrap();
        // SAFETY: the stream retains its live descriptor throughout the test.
        let before = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
        assert!(before >= 0);
        let error = super::with_nonblocking(observer.as_fd(), || reader.read(&mut [0]).map(|_| ()))
            .expect_err("an empty open stream would block");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        // SAFETY: the same observer still retains the open descriptor.
        let after = unsafe { libc::fcntl(observer.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(after, before);
    }

    #[test]
    fn scoped_nonblocking_restores_once_after_success_or_operation_error() {
        use std::cell::Cell;
        for fails in [false, true] {
            let flags = Cell::new(0);
            let calls = Cell::new(0);
            let result = super::with_nonblocking_by(
                0,
                |value| {
                    calls.set(calls.get() + 1);
                    flags.set(value);
                    Ok(())
                },
                || {
                    assert_ne!(flags.get() & libc::O_NONBLOCK, 0);
                    if fails {
                        Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.is_err(), fails);
            assert_eq!(flags.get(), 0);
            assert_eq!(calls.get(), 2, "set nonblocking, then restore once");
        }
    }

    #[test]
    fn scoped_nonblocking_unwind_restores_and_failed_restoration_still_retries() {
        use std::cell::Cell;
        let flags = Cell::new(0);
        let calls = Cell::new(0);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: std::io::Result<()> = super::with_nonblocking_by(
                0,
                |value| {
                    calls.set(calls.get() + 1);
                    flags.set(value);
                    Ok(())
                },
                || panic!("operation unwinds"),
            );
        }));
        assert!(panic.is_err());
        assert_eq!(flags.get(), 0);
        assert_eq!(calls.get(), 2);

        calls.set(0);
        let error = super::with_nonblocking_by(
            0,
            |value| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                flags.set(value);
                Ok(())
            },
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            flags.get(),
            0,
            "Drop retries the failed explicit restoration"
        );
        assert_eq!(calls.get(), 3);

        calls.set(0);
        let error = super::with_nonblocking_by(
            0,
            |value| {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
                }
                flags.set(value);
                Ok(())
            },
            || -> std::io::Result<()> { panic!("failed setup cannot run the operation") },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(flags.get(), 0);
        assert_eq!(calls.get(), 2, "failed setup still attempts restoration");
    }

    #[test]
    fn scoped_nonblocking_restores_native_flags_during_unwind() {
        use std::os::fd::{AsFd as _, AsRawFd as _};
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: reader retains its live descriptor throughout this test.
        let before = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        assert!(before >= 0);
        let panic = std::panic::catch_unwind(|| {
            let _: std::io::Result<()> = super::with_nonblocking(reader.as_fd(), || {
                // SAFETY: the scope retains the same live descriptor.
                let during = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
                assert!(during >= 0);
                assert_ne!(during & libc::O_NONBLOCK, 0);
                panic!("operation unwinds");
            });
        });
        assert!(panic.is_err());
        // SAFETY: reader still retains that descriptor after unwinding.
        assert_eq!(
            unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
            before
        );
    }

    use super::{Ready, either, one};
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// The one-minute hang detector on a wait that must end.
    const HANG: Duration = Duration::from_secs(60);

    /// Waits under the hang detector for the pipe `reader` reads, with nothing left in it, to be
    /// at its end. Once a test drops its writer the end must come, but not always at once: a
    /// sibling test's child parked between fork and exec (`fork_window::Parked`, or a `pre_exec`
    /// spawn) holds a copy of every descriptor this process had open, that writer included, until
    /// it execs. A check that reads a set of ends at once comes after this, so it reads the set,
    /// not a race with a sibling's fork.
    fn ended_pipe(reader: std::os::fd::BorrowedFd<'_>) {
        assert_eq!(
            super::any_until(&[reader], std::time::Instant::now() + HANG).unwrap(),
            [0],
            "the pipe's end"
        );
    }

    #[test]
    fn deadline_readiness_keeps_buffered_bytes_and_eof_after_disconnect() {
        let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + HANG;
        super::until(near.as_fd(), super::Interest::Write, deadline).unwrap();
        far.write_all(b"kept").unwrap();
        drop(far);
        super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
        let mut bytes = [0; 4];
        near.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"kept");
        super::until(near.as_fd(), super::Interest::Read, deadline).unwrap();
        assert_eq!(near.read(&mut bytes).unwrap(), 0);
        for interest in [super::Interest::Read, super::Interest::Write] {
            assert_eq!(
                super::until(near.as_fd(), interest, std::time::Instant::now())
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::TimedOut
            );
        }
    }

    /// Bytes and then the connection's end each end the wait for a read, and neither is taken.
    #[test]
    fn a_wait_for_a_read_ends_at_bytes_and_at_the_end_and_takes_neither() {
        let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_read_timeout(Some(HANG)).unwrap();
        far.write_all(b"k").unwrap();
        super::for_read(&near).unwrap();
        let mut byte = [0];
        near.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"k");
        far.shutdown(std::net::Shutdown::Write).unwrap();
        super::for_read(&near).unwrap();
        assert_eq!(near.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn a_wait_for_a_read_past_its_timeout_says_what_the_read_would() {
        let (near, far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(super::for_read(&near));
        });
        let waited = ended.recv_timeout(HANG).expect("the timeout ends the wait");
        assert_eq!(waited.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        drop(far);
    }

    /// A byte already queued is the read's to return however short the timeout: `WouldBlock` for
    /// it would tell the caller nothing came.
    #[test]
    fn a_queued_byte_comes_before_a_wait_for_a_reads_timeout() {
        let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_read_timeout(Some(Duration::from_nanos(1)))
            .unwrap();
        far.write_all(b"k").unwrap();
        super::for_read(&near).expect("a byte waits to be read");
        near.read_exact(&mut [0]).unwrap();
    }

    /// `poll(2)` is asked even past the deadline, and only there does a past deadline not end
    /// the wait first: [`super::until`] says expired on the same stream.
    #[test]
    fn a_wait_already_past_its_deadline_still_asks() {
        use std::os::fd::AsFd as _;
        let (near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        let past = std::time::Instant::now();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let asked = super::asked_until(near.as_fd(), super::Interest::Read, past);
            let _ = done.send((asked, near));
        });
        let (asked, near) = ended
            .recv_timeout(HANG)
            .expect("the deadline ends the wait");
        assert_eq!(asked.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        far.write_all(b"k").unwrap();
        super::asked_until(near.as_fd(), super::Interest::Read, past).expect("a byte waits");
        let expired = super::until(near.as_fd(), super::Interest::Read, past);
        assert_eq!(expired.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }

    /// With no timeout the wait lasts until a byte comes: on a nonblocking stream a wait that
    /// gave up early would show as the read's `WouldBlock`.
    #[test]
    fn a_wait_for_a_read_with_no_timeout_lasts_until_a_byte_comes() {
        let (mut near, mut far) = crate::kept_out(UnixStream::pair).unwrap();
        near.set_nonblocking(true).unwrap();
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = super::for_read(&near).and_then(|()| near.read_exact(&mut [0]));
            let _ = done.send(read);
        });
        std::thread::sleep(Duration::from_millis(20));
        far.write_all(b"k").unwrap();
        ended.recv_timeout(HANG).expect("the wait ends").unwrap();
    }

    /// EACH SIDE WAKES THE WAIT, AND NEITHER IS READ: a byte on the first is
    /// still there after the wait said so; the second's writer closing (a
    /// pipe at its end) wakes it as the second; and a wait woken while the
    /// other side is still silent names only the side that woke it.
    #[test]
    fn either_side_ends_the_wait_and_nothing_is_read() {
        let (mut a_near, a_far) = UnixStream::pair().expect("a pair");
        let (b_read, b_write) = std::io::pipe().expect("a pipe");
        let (said, heard) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let found = either(a_far.as_fd(), b_read.as_fd()).expect("the first wait");
            said.send(found).expect("the test listens");
            let mut byte = [0_u8; 1];
            (&a_far)
                .read_exact(&mut byte)
                .expect("the byte is still there");
            assert_eq!(&byte, b"x");
            let found = either(a_far.as_fd(), b_read.as_fd()).expect("the second wait");
            said.send(found).expect("the test listens");
        });
        a_near.write_all(b"x").expect("write");
        assert_eq!(
            heard.recv_timeout(HANG).expect("the first wait ended"),
            Ready {
                first: true,
                second: false
            }
        );
        drop(b_write);
        assert_eq!(
            heard.recv_timeout(HANG).expect("the second wait ended"),
            Ready {
                first: false,
                second: true
            }
        );
        waiter.join().expect("the waiter");
    }

    /// A LISTENER IS READY WITH A CONNECTION WAITING: [`one`] ends once a connect is queued, and
    /// a `Ready` accept then finds it.
    #[test]
    fn one_ends_when_a_listener_has_a_connection_to_accept() {
        let root = aterm_tempfile::tempdir().expect("a folder");
        let path = root.path().join("ready.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let (said, heard) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            one(listener.as_fd()).expect("the wait");
            said.send(
                crate::accept(
                    &listener,
                    crate::AcceptWait::Ready,
                    crate::AcceptedMode::Blocking,
                )
                .is_ok(),
            )
            .expect("the test listens");
        });
        let _client = UnixStream::connect(&path).expect("connect");
        assert!(heard.recv_timeout(HANG).expect("the wait ended"));
        waiter.join().expect("the waiter");
    }

    /// A PEER'S CLOSE IS READY: a socket whose peer is gone ends the wait
    /// as its side, with nothing to read but its end.
    #[test]
    fn a_closed_peer_ends_the_wait() {
        let (near, far) = UnixStream::pair().expect("a pair");
        let (b_read, _b_write) = std::io::pipe().expect("a pipe");
        drop(far);
        let found = either(b_read.as_fd(), near.as_fd()).expect("the wait");
        assert_eq!(
            found,
            Ready {
                first: false,
                second: true
            }
        );
        assert_eq!((&near).read(&mut [0_u8; 1]).expect("its end"), 0);
    }
}
