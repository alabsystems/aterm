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

/// [`any_until`] with the deadline optional: `None` waits until a descriptor is ready, with no
/// timeout. Returns the ready indices; an empty vector means `deadline` came.
/// # Errors
/// Empty or unrepresentable set, or poll error other than interruption.
pub fn any(fds: &[BorrowedFd<'_>], deadline: Option<std::time::Instant>) -> io::Result<Vec<usize>> {
    ready_set(
        fds,
        |fd| read_pollfd(*fd),
        deadline.map_or(Wait::Forever, Wait::Until),
    )
}

/// Which of `fds` are readable, at their end or in error now, asked once with zero timeout and
/// reading nothing.
/// # Errors
/// Empty or unrepresentable set, or poll error other than interruption.
pub fn any_now(fds: &[BorrowedFd<'_>]) -> io::Result<Vec<usize>> {
    ready_set(fds, |fd| read_pollfd(*fd), Wait::Now)
}

/// A wake any thread rings, as often as it likes, for a loop that polls it beside its own
/// descriptors ([`Doorbell::fd`]): a nonblocking pipe that [`Doorbell::ring`] writes a byte into
/// and [`Doorbell::clear`] empties. Unlike a stop's bell ([`crate::stop::Stop::bell`]), which
/// rings once for good, it rings again after each clear. The loop clears it before it reads the
/// state a ring announces, so a ring that comes during that read polls ready at its next wait.
#[derive(Debug)]
pub struct Doorbell {
    reader: std::io::PipeReader,
    ringer: std::io::PipeWriter,
}

impl Doorbell {
    /// A doorbell not rung: a pipe (two descriptors, kept out of children), both ends nonblocking.
    ///
    /// # Errors
    /// The pipe could not be made (this process is out of descriptors), or its ends could not be
    /// made nonblocking.
    pub fn new() -> io::Result<Self> {
        use std::os::fd::AsFd as _;
        let (reader, ringer) = crate::pipe()?;
        for fd in [reader.as_fd(), ringer.as_fd()] {
            // SAFETY: F_GETFL reads the flags of a descriptor this function owns.
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            // SAFETY: F_SETFL takes an integer flag word for the same owned descriptor.
            if flags < 0
                || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { reader, ringer })
    }

    /// Ring: the next poll of [`Self::fd`] reads ready until [`Self::clear`]. A pipe already full
    /// of rings is rung already, so a write it refuses changes nothing.
    pub fn ring(&self) {
        use std::io::Write as _;
        let _ = (&self.ringer).write(&[1]);
    }

    /// Take every ring so far: `true` when it was rung since the last clear.
    pub fn clear(&self) -> bool {
        use std::io::Read as _;
        let mut rung = false;
        let mut bytes = [0; 64];
        while let Ok(read) = (&self.reader).read(&mut bytes) {
            if read == 0 {
                break;
            }
            rung = true;
        }
        rung
    }

    /// The descriptor that polls readable while the doorbell is rung: poll it, never read it.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.reader.as_fd()
    }
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

/// The same absolute wait, interrupted by a borrowed readable control source. Nothing is
/// consumed; a Stop bell remains latched and a closed broker request remains observable.
pub(crate) fn until_cancelled(
    fd: BorrowedFd<'_>,
    interest: Interest,
    deadline: std::time::Instant,
    cancel: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    let Some(cancel) = cancel else {
        return until(fd, interest, deadline);
    };
    let ready = any_interests_until(
        &[Watch::new(fd, interest), Watch::new(cancel, Interest::Read)],
        deadline,
    )?;
    if ready.contains(&1) {
        return Err(cancelled());
    }
    if ready.contains(&0) {
        Ok(())
    } else {
        Err(expired())
    }
}

pub(crate) fn check_cancelled(cancel: Option<BorrowedFd<'_>>) -> io::Result<()> {
    if let Some(cancel) = cancel
        && !any_now(&[cancel])?.is_empty()
    {
        return Err(cancelled());
    }
    Ok(())
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "socket preparation cancelled")
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
#[path = "ready_tests.rs"]
mod tests;
