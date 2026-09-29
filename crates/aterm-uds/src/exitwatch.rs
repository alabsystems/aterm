// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A kernel witness of ANOTHER process's death, with its full `wait(2)` status —
//! lifted from `aterm-gui`'s seamless handoff (`CandidateExitWatch`,
//! `app_update_handoff.rs`) so the PTY keeper can classify a window's end the
//! same way (`docs/DESIGN-pty-keeper-2026-09-26.md` F12, §5.4).
//!
//! Darwin's `kqueue` `EVFILT_PROC` with `NOTE_EXIT | NOTE_EXITSTATUS` delivers a
//! process's wait status to a watcher that is NOT its parent. XNU gates
//! `NOTE_EXITSTATUS` on being the parent, the tracer, or being permitted to
//! signal the target — which a same-uid watcher is. Measured (Darwin 25.5.0,
//! recorded in the handoff): `exit(7)` reads `0x0700`, `SIGKILL` reads `0x9`.
//!
//! Two properties the callers lean on:
//! * THE EVENT IS DURABLE. XNU queues the knote inside `proc_exit`; it stays in
//!   this watch's queue until read, so the target being reaped by someone else
//!   cannot take the fact away.
//! * THE KNOTE IS ONE-SHOT. The read that returns it dequeues it, so the watch
//!   KEEPS what it read and answers the same on every later call.
//!
//! The watch's descriptor is a kqueue, which `poll(2)` reports readable once an
//! event is queued: a single-threaded loop can wait on many watches beside its
//! sockets ([`ExitWatch::as_raw_fd`]).
//!
//! Registration must happen while the target is alive; `ESRCH` (it is already
//! gone) answers `None` from [`ExitWatch::watch`], and the caller has no status
//! to classify by. Other platforms: `watch` answers `None` (no primitive wired;
//! Linux's `pidfd` cannot read a non-child's status).

/// A one-process exit watch. See the module docs.
#[derive(Debug)]
pub struct ExitWatch {
    #[cfg(target_vendor = "apple")]
    kq: std::os::fd::OwnedFd,
    pid: u32,
    seen: std::sync::OnceLock<i32>,
}

#[cfg(target_vendor = "apple")]
mod kq {
    /// `struct kevent` (sys/event.h), 32 bytes on both Apple ABIs (measured with
    /// the SDK 2026-09-28).
    #[repr(C)]
    pub struct Kevent {
        pub ident: usize,
        pub filter: i16,
        pub flags: u16,
        pub fflags: u32,
        pub data: isize,
        pub udata: *mut core::ffi::c_void,
    }

    #[repr(C)]
    pub struct Timespec {
        pub tv_sec: i64,
        pub tv_nsec: i64,
    }

    pub const EVFILT_PROC: i16 = -5;
    pub const EV_ADD: u16 = 0x1;
    pub const EV_ENABLE: u16 = 0x4;
    pub const EV_ERROR: u16 = 0x4000;
    pub const NOTE_EXIT: u32 = 0x8000_0000;
    pub const NOTE_EXITSTATUS: u32 = 0x0400_0000;
    pub const F_SETFD: i32 = 2;
    pub const FD_CLOEXEC: i32 = 1;

    unsafe extern "C" {
        pub fn kqueue() -> i32;
        pub fn kevent(
            kq: i32,
            changelist: *const Kevent,
            nchanges: i32,
            eventlist: *mut Kevent,
            nevents: i32,
            timeout: *const Timespec,
        ) -> i32;
        pub fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
}

impl ExitWatch {
    /// Watch `pid`, which must still be alive. `None`: it is already gone, it
    /// is not a process anyone watches (`<= 1`), or this platform has no
    /// primitive.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    // Skip: bottoms out at the `kqueue`/`kevent`/`fcntl` FFI calls.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn watch(pid: u32) -> Option<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        if pid <= 1 || pid > i32::MAX as u32 {
            return None;
        }
        // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { kq::kqueue() };
        if raw < 0 {
            return None;
        }
        // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
        let kqfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // Close-on-exec by house rule (Darwin does not inherit a kqueue across
        // fork anyway), so "which descriptors can leave" needs no footnote.
        // SAFETY: F_SETFD takes an int and touches only this descriptor's flags.
        unsafe { kq::fcntl(kqfd.as_raw_fd(), kq::F_SETFD, kq::FD_CLOEXEC) };
        let change = kq::Kevent {
            ident: pid as usize,
            filter: kq::EVFILT_PROC,
            flags: kq::EV_ADD | kq::EV_ENABLE,
            fflags: kq::NOTE_EXIT | kq::NOTE_EXITSTATUS,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // No output slot: a registration failure (ESRCH) then comes back through
        // the return value, never as an EV_ERROR event mistaken for an exit.
        // SAFETY: one change entry, live for the call; no event list requested.
        let rc = unsafe {
            kq::kevent(
                kqfd.as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        (rc == 0).then_some(Self {
            kq: kqfd,
            pid,
            seen: std::sync::OnceLock::new(),
        })
    }

    /// See the Darwin twin: no primitive here.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn watch(_pid: u32) -> Option<Self> {
        None
    }

    /// The pid this watch names.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The target's `wait(2)`-encoded status IF THE KERNEL HAS RECORDED ONE.
    /// Never blocks and never reaps. Once it has answered, it answers the same
    /// on every later call.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    // Skip: bottoms out at the `kevent` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn exit_status(&self) -> Option<i32> {
        use std::os::fd::AsRawFd as _;
        if let Some(status) = self.seen.get() {
            return Some(*status);
        }
        let mut event = kq::Kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let timeout = kq::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: one event slot and one timespec, both live for the call.
        let rc = unsafe {
            kq::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        if rc != 1 || event.flags & kq::EV_ERROR != 0 {
            return None;
        }
        if event.ident != self.pid as usize
            || event.fflags & kq::NOTE_EXIT == 0
            || event.fflags & kq::NOTE_EXITSTATUS == 0
        {
            return None;
        }
        // The status is the low 32 bits of `data`, in `wait(2)`'s encoding.
        let raw = i32::try_from(event.data & 0xffff_ffff).ok()?;
        Some(*self.seen.get_or_init(|| raw))
    }

    /// See the Darwin twin.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn exit_status(&self) -> Option<i32> {
        self.seen.get().copied()
    }

    /// The descriptor `poll(2)` reports readable once the exit is queued.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.kq.as_raw_fd()
    }

    /// See the Darwin twin: no descriptor here (a watch is never built, since
    /// [`ExitWatch::watch`] answers `None`). `-1` is the one value `poll(2)`
    /// is defined to skip.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        -1
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::ExitWatch;
    use std::process::{Command, Stdio};

    /// A child that exits 7 reads `0x0700`, one SIGKILLed reads `0x9`, and the
    /// second read answers the same as the first (the knote is one-shot; the
    /// watch keeps it). The child is not reaped by the watch: `wait` still
    /// answers afterwards.
    #[test]
    fn the_watch_reads_exit_and_signal_statuses() {
        let mut exits = Command::new("/bin/sh")
            .args(["-c", "read _; exit 7"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn");
        let watch = ExitWatch::watch(exits.id()).expect("watch a live child");
        assert_eq!(watch.exit_status(), None, "alive: nothing recorded yet");
        drop(exits.stdin.take());
        let status = exits.wait().expect("wait");
        assert_eq!(status.code(), Some(7));
        let got = wait_for(&watch);
        assert_eq!(got, 0x0700, "exit(7)");
        assert_eq!(watch.exit_status(), Some(0x0700), "answers the same twice");

        let mut killed = Command::new("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("spawn");
        let watch = ExitWatch::watch(killed.id()).expect("watch a live child");
        killed.kill().expect("SIGKILL");
        let _ = killed.wait();
        assert_eq!(wait_for(&watch), 0x9, "SIGKILL");
    }

    /// A pid that names nothing cannot be watched.
    #[test]
    fn a_gone_pid_cannot_be_watched() {
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id();
        let _ = child.wait();
        assert!(ExitWatch::watch(pid).is_none(), "reaped: ESRCH");
        assert!(ExitWatch::watch(0).is_none());
        assert!(ExitWatch::watch(1).is_none());
    }

    /// Block on the watch's descriptor (poll), never on a clock: the kernel
    /// queues the event inside `proc_exit`, which `wait` has already observed.
    fn wait_for(watch: &ExitWatch) -> i32 {
        #[repr(C)]
        struct PollFd {
            fd: i32,
            events: i16,
            revents: i16,
        }
        unsafe extern "C" {
            fn poll(fds: *mut PollFd, n: u32, timeout: i32) -> i32;
        }
        let mut pfd = PollFd {
            fd: watch.as_raw_fd(),
            events: 1,
            revents: 0,
        };
        // A minute is a hang detector, not a latency budget.
        // SAFETY: one live pollfd.
        let rc = unsafe { poll(&mut pfd, 1, 60_000) };
        assert_eq!(rc, 1, "the exit event is queued");
        watch
            .exit_status()
            .expect("a status once the queue is readable")
    }
}
