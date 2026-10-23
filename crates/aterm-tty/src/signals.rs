// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal signal handling (`SIGWINCH`, `SIGTSTP`, `SIGCONT`) routed through a
//! process-wide self-pipe.

/// A set of the signals a terminal program waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Signals(u8);

impl Signals {
    pub const NONE: Self = Self(0);
    /// SIGWINCH: the window changed size.
    pub const WINCH: Self = Self(1);
    /// SIGTSTP from outside: the program is asked to stop ([`suspend`]).
    pub const TSTP: Self = Self(1 << 1);
    /// SIGCONT: the process was continued, and its terminal may have been reset while it
    /// was stopped.
    pub const CONT: Self = Self(1 << 2);

    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[cfg(unix)]
pub use unix::{SignalWake, suspend};

#[cfg(unix)]
pub(crate) use unix::errno;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::fd::RawFd;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

    use super::Signals;
    use crate::raw::RawGuard;

    /// The signals caught since the last [`SignalWake::take`].
    static PENDING: AtomicU8 = AtomicU8::new(0);
    /// The write end of the wake pipe; -1 until it exists.
    static WAKE_WR: AtomicI32 = AtomicI32::new(-1);
    /// A [`SignalWake`] is installed.
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    /// SIGTSTP was ignored when the wake was installed (a parent with no job control
    /// arranged it): it stays ignored, and [`suspend`] does not stop.
    static TSTP_IGNORED: AtomicBool = AtomicBool::new(false);

    /// The calling thread's `errno`.
    pub(crate) fn errno() -> libc::c_int {
        // SAFETY: the calling thread's own errno slot, valid for its lifetime.
        unsafe { *errno_slot() }
    }

    fn errno_slot() -> *mut libc::c_int {
        // SAFETY: returns the calling thread's errno address; takes no arguments.
        #[cfg(target_os = "macos")]
        let slot = unsafe { libc::__error() };
        // SAFETY: as above.
        #[cfg(not(target_os = "macos"))]
        let slot = unsafe { libc::__errno_location() };
        slot
    }

    /// The handler of the three signals: the flag, then one byte into the wake pipe,
    /// both async-signal-safe, with the interrupted code's `errno` put back.
    extern "C" fn on_signal(sig: libc::c_int) {
        let slot = errno_slot();
        // SAFETY: the calling thread's own errno slot.
        let saved = unsafe { *slot };
        let bit = match sig {
            libc::SIGWINCH => Signals::WINCH.0,
            libc::SIGTSTP => Signals::TSTP.0,
            libc::SIGCONT => Signals::CONT.0,
            _ => 0,
        };
        PENDING.fetch_or(bit, Ordering::AcqRel);
        let wr = WAKE_WR.load(Ordering::Acquire);
        if wr >= 0 {
            let byte = 1u8;
            // SAFETY: a one-byte write(2) from a local; the pipe is non-blocking and never
            // closed, so a full one answers EAGAIN and the bytes in it wake the loop.
            unsafe { libc::write(wr, (&raw const byte).cast(), 1) };
        }
        // SAFETY: as above.
        unsafe { *slot = saved };
    }

    /// The read end of the process's wake pipe, made once (non-blocking, close-on-exec)
    /// and never closed.
    fn wake_pipe() -> io::Result<RawFd> {
        static PIPE: OnceLock<Option<(RawFd, RawFd)>> = OnceLock::new();
        let (rd, wr) = (*PIPE.get_or_init(make_pipe))
            .ok_or_else(|| io::Error::other("the signal wake pipe could not be made"))?;
        WAKE_WR.store(wr, Ordering::Release);
        Ok(rd)
    }

    fn make_pipe() -> Option<(RawFd, RawFd)> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a writable array of two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        for fd in fds {
            // SAFETY: `fd` is a descriptor `pipe` just returned; these only set its flags.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Some((fds[0], fds[1]))
    }

    /// The disposition `sig` has now.
    fn disposition(sig: libc::c_int) -> libc::sigaction {
        // SAFETY: `sigaction` is plain data for which all-zero is a valid value.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action only reads the current one into `was`.
        unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut was) };
        was
    }

    /// Route `sig` to [`on_signal`] with `SA_RESTART`, returning the previous disposition.
    fn catch(sig: libc::c_int) -> libc::sigaction {
        // SAFETY: an all-zero `sigaction` is a valid value; filled in below.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: `sa.sa_mask` is a valid out-parameter owned by this frame.
        unsafe { libc::sigemptyset(&raw mut sa.sa_mask) };
        sa.sa_flags = libc::SA_RESTART;
        // SAFETY: as above.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: installs `on_signal`, which is async-signal-safe; `was` receives the
        // disposition it replaces.
        unsafe { libc::sigaction(sig, &raw const sa, &raw mut was) };
        was
    }

    /// Routes `SIGWINCH`, `SIGTSTP`, and `SIGCONT` to the wake pipe until dropped.
    #[derive(Debug)]
    pub struct SignalWake {
        rd: RawFd,
        replaced: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl SignalWake {
        /// Catch the three signals.
        ///
        /// # Errors
        /// Another wake is installed in this process, or the pipe could not be made.
        pub fn install() -> io::Result<Self> {
            if INSTALLED.swap(true, Ordering::AcqRel) {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    "the signal wake is already installed in this process",
                ));
            }
            let rd = match wake_pipe() {
                Ok(rd) => rd,
                Err(error) => {
                    INSTALLED.store(false, Ordering::Release);
                    return Err(error);
                }
            };
            drain(rd);
            PENDING.store(0, Ordering::Release);
            let tstp_ignored = disposition(libc::SIGTSTP).sa_sigaction == libc::SIG_IGN;
            TSTP_IGNORED.store(tstp_ignored, Ordering::Release);
            let mut replaced = vec![
                (libc::SIGWINCH, catch(libc::SIGWINCH)),
                (libc::SIGCONT, catch(libc::SIGCONT)),
            ];
            if !tstp_ignored {
                replaced.push((libc::SIGTSTP, catch(libc::SIGTSTP)));
            }
            Ok(Self { rd, replaced })
        }

        /// The descriptor a `poll` watches beside the terminal: readable when a signal
        /// arrived.
        #[inline]
        #[must_use]
        pub const fn fd(&self) -> RawFd {
            self.rd
        }

        /// The signals caught and not yet taken.
        #[inline]
        #[must_use]
        pub fn pending(&self) -> Signals {
            Signals(PENDING.load(Ordering::Acquire))
        }

        /// Empty the wake pipe.
        #[inline]
        pub fn drain(&self) {
            drain(self.rd);
        }

        /// Drain the wake pipe and return the signals caught since the last take.
        #[inline]
        pub fn take(&self) -> Signals {
            drain(self.rd);
            Signals(PENDING.swap(0, Ordering::AcqRel))
        }
    }

    impl Drop for SignalWake {
        fn drop(&mut self) {
            for (sig, was) in &self.replaced {
                // SAFETY: puts back the disposition `install` replaced.
                unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
            }
            INSTALLED.store(false, Ordering::Release);
        }
    }

    fn drain(rd: RawFd) {
        let mut sink = [0u8; 64];
        // SAFETY: a read into a local buffer from the non-blocking pipe, until it is empty.
        while unsafe { libc::read(rd, sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
    }

    /// Restores terminal modes and stops the process via `SIGTSTP` (unless SIGTSTP was
    /// ignored when the wake was installed), re-entering raw mode once it continues.
    ///
    /// # Errors
    /// Setting the terminal raw again failed.
    pub fn suspend(guard: &mut RawGuard) -> io::Result<()> {
        let modes = guard.leave();
        if !TSTP_IGNORED.load(Ordering::Acquire) {
            stop_self();
        }
        guard.reenter(modes)
    }

    /// Raise `SIGTSTP` under its default disposition and restore the caught disposition after `SIGCONT`.
    fn stop_self() {
        // SAFETY: an all-zero `sigaction` is a valid value (empty mask, no flags).
        let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
        default.sa_sigaction = libc::SIG_DFL;
        // SAFETY: as above.
        let mut caught: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: the default disposition for SIGTSTP; `caught` receives the one replaced.
        unsafe { libc::sigaction(libc::SIGTSTP, &raw const default, &raw mut caught) };
        // SAFETY: `set` is a local signal set, emptied before SIGTSTP is added.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut set);
            libc::sigaddset(&raw mut set, libc::SIGTSTP);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "SIGTSTP, no core signal: C-z stops the program as it stops any job"
        )]
        // SAFETY: `raise` with the default disposition stops the process; it returns
        // once SIGCONT continues it.
        unsafe {
            libc::raise(libc::SIGTSTP);
        }
        // SAFETY: puts back the disposition replaced above.
        unsafe { libc::sigaction(libc::SIGTSTP, &raw const caught, std::ptr::null_mut()) };
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{SignalWake, Signals};

    /// Sending `SIGWINCH` to this process wakes the pipe and is taken once.
    #[test]
    fn a_resize_wakes_the_pipe() {
        let _alone = crate::raw::TEST_TERMINAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wake = SignalWake::install().expect("the wake installs");
        assert!(
            SignalWake::install().is_err(),
            "one wake per process at a time"
        );
        // SAFETY: SIGWINCH to this process, which the wake now catches.
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGWINCH) }, 0);
        let mut pfd = libc::pollfd {
            fd: wake.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let budget = aterm_spec::wait::MUST_HAPPEN;
        let ms = libc::c_int::try_from(budget.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one live pollfd.
        let ready = unsafe { libc::poll(&raw mut pfd, 1, ms) };
        assert_eq!(ready, 1, "the pipe woke");
        assert_eq!(wake.take(), Signals::WINCH);
        assert_eq!(wake.take(), Signals::NONE, "taken once");
        // SAFETY: as above.
        assert_eq!(
            unsafe { libc::poll(&raw mut pfd, 1, 0) },
            0,
            "the pipe is empty"
        );
        drop(wake);
        let again = SignalWake::install().expect("installs again once dropped");
        drop(again);
    }
}
