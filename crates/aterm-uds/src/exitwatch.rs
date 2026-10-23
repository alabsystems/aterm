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
        let target = exit_readable(pid)?;
        let (cancelled, cancel) = std::io::pipe().ok()?;
        let stamped = std::sync::Arc::new(std::sync::OnceLock::new());
        let into = std::sync::Arc::clone(&stamped);
        std::thread::Builder::new()
            .name(exit_moment_thread(pid))
            .spawn(move || {
                use std::os::fd::AsRawFd as _;
                if wait_readable(target.as_raw_fd(), cancelled.as_raw_fd()) {
                    let _ = into.set(stamp());
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

/// A descriptor `poll(2)` reports readable once `pid` has exited (Darwin.
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

/// the [`ExitWatch`] kqueue.
#[cfg(target_vendor = "apple")]
fn exit_readable(pid: u32) -> Option<ExitReadable> {
    ExitWatch::watch(pid)
}

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
    // SAFETY: `pidfd_open` takes a pid and a flags word, and returns a new descriptor or -1.
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

/// (the exit came), `false` when only the cancel pipe is or `poll` fails.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn wait_readable(target: i32, cancel: i32) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    #[cfg(target_os = "linux")]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = core::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    }
    const POLLIN: i16 = 0x1;
    loop {
        let mut fds = [
            PollFd {
                fd: target,
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: cancel,
                events: POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two initialized pollfds that outlive the call.
        let ready = unsafe { poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if fds[0].revents != 0 {
            return true;
        }
        if fds[1].revents != 0 {
            return false;
        }
    }
}

#[cfg(all(test, unix))]
mod moment_tests {
    use super::{ExitMoment, exit_moment_thread};

    use crate::spawnfd::Command;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Each stamp taken, with the name of the thread that took it.
    static TAKEN: Mutex<Vec<(Option<String>, Instant)>> = Mutex::new(Vec::new());

    fn stamp() -> Instant {
        let now = Instant::now();
        TAKEN
            .lock()
            .unwrap()
            .push((std::thread::current().name().map(String::from), now));
        now
    }

    /// watched; a watch dropped before the exit ends its thread without stamping.
    #[test]
    fn the_moment_is_stamped_when_the_kernel_reports_the_exit() {
        let mut child = Command::new("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let watch = ExitMoment::watch(pid, stamp);
        if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
            assert!(watch.is_none(), "no primitive: no watch");
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        let watch = watch.expect("watch a live child");
        assert_eq!(watch.get(), None, "alive: nothing stamped");
        let killed = Instant::now();
        child.kill().expect("SIGKILL");
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(60);
        let at = loop {
            if let Some(at) = watch.get() {
                break at;
            }
            assert!(Instant::now() < deadline, "the exit was never stamped");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(at >= killed, "stamped after the kill");
        let taken = TAKEN.lock().unwrap().clone();
        assert!(
            taken
                .iter()
                .any(|(name, when)| *name == Some(exit_moment_thread(pid)) && *when == at),
            "stamped on the watch's thread: {taken:?}"
        );

        let mut gone = Command::new("/usr/bin/true").spawn().expect("spawn");
        let gone_pid = gone.id();
        let _ = gone.wait();
        assert!(
            ExitMoment::watch(gone_pid, stamp).is_none(),
            "reaped: ESRCH"
        );
        assert!(ExitMoment::watch(0, stamp).is_none());
        assert!(ExitMoment::watch(1, stamp).is_none());

        let mut idle = Command::new("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("spawn");
        let before = TAKEN.lock().unwrap().len();
        let watch = ExitMoment::watch(idle.id(), stamp).expect("watch");
        let thread = watch.thread_alive();
        drop(watch);
        let deadline = Instant::now() + Duration::from_secs(60);
        while thread.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "a dropped watch's thread never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = idle.kill();
        let _ = idle.wait();
        assert_eq!(
            TAKEN.lock().unwrap().len(),
            before,
            "a watch dropped while its process lived stamps nothing"
        );
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::ExitWatch;

    use crate::spawnfd::{Command, Stdio};

    /// does not reap.
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

    /// `proc_exit`, which `wait` has already observed.
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
        // SAFETY: one live pollfd.
        let rc = unsafe { poll(&mut pfd, 1, 60_000) };
        assert_eq!(rc, 1, "the exit event is queued");
        watch
            .exit_status()
            .expect("a status once the queue is readable")
    }
}
