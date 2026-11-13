// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A kernel witness of another process's death.

/// A one-process exit watch.
#[derive(Debug)]
pub struct ExitWatch {
    #[cfg(target_vendor = "apple")]
    kq: std::os::fd::OwnedFd,
    pid: u32,
    seen: std::sync::OnceLock<i32>,
}

#[cfg(target_vendor = "apple")]
pub(crate) mod kq {
    /// `struct kevent` (sys/event.h), 32 bytes on both Apple ABIs.
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
    /// Watch `pid`, which must still be alive.
    #[cfg(target_vendor = "apple")]
    #[must_use]
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
    #[inline]
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// The target's `wait(2)`-encoded status if the kernel has recorded one.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    pub fn exit_status(&self) -> Option<i32> {
        self.exit_status_within(std::time::Duration::ZERO)
    }

    /// [`Self::exit_status`], waiting up to `within` for the kernel to queue it: macOS stops
    /// listing a dying process's pid before it queues the exit's event (1 µs to 1.2 ms later
    /// under load, `atpkg`'s `build_admit_hold`), so a reader that found the pid gone waits
    /// briefly rather than read the exit as one with no status.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn exit_status_within(&self, within: std::time::Duration) -> Option<i32> {
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
            tv_sec: i64::try_from(within.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(within.subsec_nanos()),
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
        let raw = i32::try_from(event.data & 0xffff_ffff).ok()?;
        Some(*self.seen.get_or_init(|| raw))
    }

    /// See the Darwin twin.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn exit_status(&self) -> Option<i32> {
        self.seen.get().copied()
    }

    /// See the Darwin twin.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn exit_status_within(&self, _within: std::time::Duration) -> Option<i32> {
        self.seen.get().copied()
    }

    /// The descriptor `poll(2)` reports readable once the exit is queued.
    #[cfg(target_vendor = "apple")]
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.kq.as_raw_fd()
    }

    /// See the Darwin twin: never called, since [`ExitWatch::watch`] answers `None` here.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        -1
    }
}

/// The name of the thread an [`ExitMoment`] on `pid` waits on (a test's stamp checks it).
#[must_use]
pub fn exit_moment_thread(pid: u32) -> String {
    format!("aterm-exit-moment-{pid}")
}

/// When another process ended.
#[derive(Debug)]
pub struct ExitMoment<T> {
    stamped: std::sync::Arc<std::sync::OnceLock<T>>,
    /// Dropped with the watch: the thread's pipe reads end-of-file and the
    /// thread returns.
    #[cfg(unix)]
    _cancel: std::io::PipeWriter,
}

impl<T: Clone + Send + Sync + 'static> ExitMoment<T> {
    /// Watch `pid`, which must still be alive, and stamp its exit with `stamp`.
    #[cfg(unix)]
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn watch(pid: u32, stamp: fn() -> T) -> Option<Self> {
        Self::watch_then(pid, stamp, || {})
    }

    /// As [`Self::watch`], notifying only after the exit stamp is readable.
    /// The notification is an observation, never child-control or reap authority.
    #[cfg(unix)]
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn watch_then(
        pid: u32,
        stamp: fn() -> T,
        ready: impl FnOnce() + Send + 'static,
    ) -> Option<Self> {
        let target = exit_readable(pid)?;
        let (cancelled, cancel) = std::io::pipe().ok()?;
        let stamped = std::sync::Arc::new(std::sync::OnceLock::new());
        let into = std::sync::Arc::clone(&stamped);
        std::thread::Builder::new()
            .name(exit_moment_thread(pid))
            .spawn(move || {
                if wait_readable(&target, &cancelled) && into.set(stamp()).is_ok() {
                    ready();
                }
            })
            .ok()?;
        Some(Self {
            stamped,
            _cancel: cancel,
        })
    }

    /// See the Unix twin: no primitive here.
    #[cfg(any(not(unix), target_arch = "wasm32"))]
    #[must_use]
    pub fn watch(_pid: u32, _stamp: fn() -> T) -> Option<Self> {
        None
    }

    /// See [`Self::watch_then`]: no primitive is wired on this platform.
    #[cfg(any(not(unix), target_arch = "wasm32"))]
    #[must_use]
    pub fn watch_then(
        _pid: u32,
        _stamp: fn() -> T,
        _ready: impl FnOnce() + Send + 'static,
    ) -> Option<Self> {
        None
    }

    /// The stamp taken when the kernel reported the exit, once it has.
    #[must_use]
    pub fn get(&self) -> Option<T> {
        self.stamped.get().cloned()
    }

    /// What the watch's thread holds while it runs: gone once it has ended.
    #[cfg(all(test, unix))]
    fn thread_alive(&self) -> std::sync::Weak<std::sync::OnceLock<T>> {
        std::sync::Arc::downgrade(&self.stamped)
    }
}

/// A descriptor `poll(2)` reports readable once `pid` has exited (Darwin: an [`ExitWatch`]
/// kqueue; Linux: a `pidfd`), for a loop that waits on it beside its own.
#[cfg(unix)]
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn exit_fd(pid: u32) -> Option<std::os::fd::OwnedFd> {
    #[cfg(target_vendor = "apple")]
    {
        exit_readable(pid).map(|watch| watch.kq)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        exit_readable(pid)
    }
}

/// What an [`ExitMoment`]'s thread holds open and waits on until the exit.
#[cfg(target_vendor = "apple")]
type ExitReadable = ExitWatch;

/// See the Darwin twin: a pidfd.
#[cfg(all(unix, not(target_vendor = "apple")))]
type ExitReadable = std::os::fd::OwnedFd;

/// Something that `poll(2)` reports readable once `pid` has exited — Darwin:
/// the [`ExitWatch`] kqueue.
#[cfg(target_vendor = "apple")]
fn exit_readable(pid: u32) -> Option<ExitReadable> {
    ExitWatch::watch(pid)
}

/// See the Darwin twin. Linux: `pidfd_open(pid, 0)`, close-on-exec by
/// default. Its syscall number is 434 on every architecture this arm compiles
/// for (alpha, ia64 and mips differ, and are not wired).
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "x86",
        target_arch = "arm",
        target_arch = "riscv64"
    )
))]
#[cfg_attr(trust_verify, trust::skip)]
fn exit_readable(pid: u32) -> Option<ExitReadable> {
    use std::os::fd::FromRawFd as _;
    const SYS_PIDFD_OPEN: core::ffi::c_long = 434;
    unsafe extern "C" {
        fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
    }
    let pid = i32::try_from(pid).ok().filter(|p| *p > 1)?;
    // SAFETY: `pidfd_open` takes a pid and a flags word, and returns a new
    // descriptor or -1; nothing is borrowed.
    let fd = unsafe { syscall(SYS_PIDFD_OPEN, pid, 0u32) };
    let fd = i32::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: `fd` is a fresh descriptor this process exclusively owns.
    Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// See the Darwin twin: no primitive wired here.
#[cfg(all(
    unix,
    not(target_vendor = "apple"),
    not(all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "x86",
            target_arch = "arm",
            target_arch = "riscv64"
        )
    ))
))]
fn exit_readable(_pid: u32) -> Option<ExitReadable> {
    None
}

/// Block until `target` or `cancel` is readable: `true` when the target is
/// (the exit came), `false` when only the cancel pipe is or `poll` fails.
#[cfg(unix)]
fn wait_readable(target: &ExitReadable, cancel: &std::io::PipeReader) -> bool {
    use std::os::fd::AsFd as _;
    #[cfg(target_vendor = "apple")]
    let target = target.kq.as_fd();
    #[cfg(not(target_vendor = "apple"))]
    let target = target.as_fd();
    crate::ready::either(target, cancel.as_fd()).is_ok_and(|ready| ready.first)
}

#[cfg(all(test, unix))]
#[path = "exitwatch_moment_tests.rs"]
mod moment_tests;

#[cfg(all(test, target_vendor = "apple"))]
#[path = "exitwatch_tests.rs"]
mod tests;

#[cfg(all(test, unix, not(target_arch = "wasm32")))]
#[path = "exitwatch_notification_tests.rs"]
mod notification_tests;
