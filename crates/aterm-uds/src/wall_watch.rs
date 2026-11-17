// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALL CLOCK'S MOVES, ANNOUNCED (ATERM-KI-1105-20): a sleep and wake, and a step of the
//! wall clock, as the system announces them, on descriptors a loop polls beside its own
//! ([`WallWatch::fds`]), so a loop that keeps a wall-clock deadline (or reads the wall at all)
//! needs no timer to hear that the wall moved under its monotonic one.
//!
//! - macOS: libnotify's `com.apple.system.clock_set` (posted by every `settimeofday`, the time
//!   daemon's steps included) on a descriptor of its own (`notify_register_file_descriptor`), and
//!   the root power domain's wake ([`crate::power::listen`], on that listener's thread, which
//!   rings a doorbell here). The sleep's length is read off the two clocks between the will-sleep
//!   and the wake: the monotonic clock std reads (`CLOCK_UPTIME_RAW`) stops in sleep, the wall
//!   does not.
//! - Linux: one `timerfd` on `CLOCK_REALTIME` armed for the far future with
//!   `TFD_TIMER_CANCEL_ON_SET`, which the kernel cancels on every `clock_settime` and at every
//!   resume from suspend (`timekeeping_resume` calls `timerfd_resume`, "resume is equivalent to
//!   clock_was_set"), so one descriptor announces both, told apart from nothing: each reads as a
//!   step.
//! - Windows: the power manager's suspend and resume, announced to a callback
//!   (`PowerRegisterSuspendResumeNotification`: no window, no message loop), which rings an
//!   event here (`Doorbell`, waited on with `any`). The sleep's length is read off the wall
//!   clock and the unbiased interrupt time, which stops in a suspend (std's `Instant` does not
//!   there). A set of the wall clock is announced to windows only, so a step is not heard
//!   ([`STEPS_ANNOUNCED`]).
//! - Elsewhere [`WallWatch::start`] answers `None` and the owner keeps looking.
//!
//! A watch made by [`WallWatch::start_zoned`] also hears the local time zone set
//! ([`Moved::zoned`]): libnotify's `com.apple.system.timezone` on macOS, a new `/etc/localtime`
//! on Linux, nothing on Windows.
//!
//! A loop polls [`WallWatch::fds`] (readable means "something moved"; on Windows it waits on
//! `WallWatch::bell`) and then calls [`WallWatch::take`], which empties every descriptor
//! before it says what moved, so a move announced during the caller's reaction polls ready at
//! its next wait.

use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use crate::ready::Doorbell;
#[cfg(windows)]
pub use imp::{Doorbell, any};

/// Whether this system announces a step of the wall clock to a [`WallWatch`]: where it does
/// not (Windows), a watch hears a sleep's wake only, and an owner that needs a step keeps
/// reading its own clocks for one.
pub const STEPS_ANNOUNCED: bool = cfg!(not(windows));

/// What moved since the last [`WallWatch::take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// The wall clock was set (on Linux, or the machine resumed from suspend: one announcement).
    pub stepped: bool,
    /// The system woke from sleep (macOS), or an idle sleep it pondered did not come.
    pub woke: bool,
    /// How long the system slept, when a will-sleep was heard before the wake: the wall clock's
    /// run past the monotonic one between the two.
    pub slept: Option<Duration>,
    /// The local time zone was set, on a watch that hears it ([`WallWatch::start_zoned`]): the
    /// wall clock reads another local hour at the same instant.
    pub zoned: bool,
}

impl Moved {
    /// Whether anything moved.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.stepped || self.woke || self.zoned
    }
}

/// What a wake handler and a test's hand tell, rung on [`Heard::bell`].
#[derive(Debug)]
struct Heard {
    bell: Doorbell,
    stepped: AtomicBool,
    woke: AtomicBool,
    /// A test's hand told a zone change ([`WallWatch::tell`]).
    zoned: AtomicBool,
    /// Both clocks at the will-sleep, until its wake reads them.
    asleep_at: Mutex<Option<(Instant, SystemTime)>>,
    /// The sleep the last wake measured, until [`WallWatch::take`] hands it out.
    slept: Mutex<Option<Duration>>,
}

impl Heard {
    fn new() -> io::Result<Self> {
        Ok(Self {
            bell: Doorbell::new()?,
            stepped: AtomicBool::new(false),
            woke: AtomicBool::new(false),
            zoned: AtomicBool::new(false),
            asleep_at: Mutex::new(None),
            slept: Mutex::new(None),
        })
    }

    /// The will-sleep: both clocks noted for the wake's measure.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "only macOS hears a will-sleep")
    )]
    fn will_sleep(&self, mono: Instant, wall: SystemTime) {
        *self
            .asleep_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((mono, wall));
    }

    /// The wake: the sleep measured where a will-sleep was noted, then rung.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "only macOS hears a wake apart from a step")
    )]
    fn woke(&self, mono: Instant, wall: SystemTime) {
        let asleep = self
            .asleep_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(slept) = asleep.and_then(|at| slept(at, (mono, wall))) {
            *self.slept.lock().unwrap_or_else(PoisonError::into_inner) = Some(slept);
        }
        self.woke.store(true, Ordering::SeqCst);
        self.bell.ring();
    }

    /// The resume, with the sleep its listener measured (`None`: no suspend was heard before
    /// it), then rung.
    #[cfg(windows)]
    fn resumed(&self, slept: Option<Duration>) {
        if let Some(slept) = slept {
            *self.slept.lock().unwrap_or_else(PoisonError::into_inner) = Some(slept);
        }
        self.woke.store(true, Ordering::SeqCst);
        self.bell.ring();
    }

    fn take(&self) -> Moved {
        self.bell.clear();
        Moved {
            stepped: self.stepped.swap(false, Ordering::SeqCst),
            woke: self.woke.swap(false, Ordering::SeqCst),
            slept: self
                .slept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
            zoned: self.zoned.swap(false, Ordering::SeqCst),
        }
    }
}

/// The wall clock's run past the monotonic clock from `asleep` to `woke`: how long the system
/// slept, or `None` where the wall went backwards meanwhile (it was set back) and says nothing.
fn slept(asleep: (Instant, SystemTime), woke: (Instant, SystemTime)) -> Option<Duration> {
    let mono = woke.0.saturating_duration_since(asleep.0);
    let wall = woke.1.duration_since(asleep.1).ok()?;
    Some(wall.saturating_sub(mono))
}

/// Whether this system's step announcement also announces a resume from suspend: Linux's timer
/// is cancelled at every resume as at every set; macOS's `clock_set` is posted by a set of the
/// clock alone, which a wake need not make.
const STEPS_TELL_A_RESUME: bool = cfg!(target_os = "linux");

/// A live watch of the wall clock's moves; dropping it ends every registration.
#[derive(Debug)]
pub struct WallWatch {
    /// Rung by the wake handler (and a test's hand).
    heard: Arc<Heard>,
    /// The system's own step announcement, where it has one.
    steps: Option<imp::Steps>,
    /// The system's announcement of a new local time zone, on a watch started for it
    /// ([`Self::start_zoned`]).
    zone: Option<imp::Zone>,
    /// The sleep and wake listener; it hears until dropped.
    _power: Option<imp::Listening>,
    /// Whether a sleep's wake reaches this watch ([`Self::hears_sleep`]).
    hears_sleep: bool,
}

impl WallWatch {
    /// Watch this system's announcements: `None` where it announces neither a step nor a wake
    /// (or none could be registered), so the owner keeps its own look. `Some` once either half
    /// registered, so a watch whose power half was refused hears steps only
    /// ([`Self::hears_sleep`]).
    #[must_use]
    pub fn start() -> Option<Self> {
        Self::start_with(None)
    }

    /// [`Self::start`], hearing a change of the local time zone too ([`Moved::zoned`]): on macOS
    /// libnotify's `com.apple.system.timezone`, which the system posts when its zone is set; on
    /// Linux a new `/etc/localtime` (`timedatectl set-timezone` renames a link over it); not on
    /// Windows. `None` where the system announces none of the three.
    #[must_use]
    pub fn start_zoned() -> Option<Self> {
        Self::start_with(imp::Zone::start())
    }

    fn start_with(zone: Option<imp::Zone>) -> Option<Self> {
        let heard = Arc::new(Heard::new().ok()?);
        let steps = imp::Steps::start();
        let power = imp::power(&heard);
        Self::of(heard, steps, power, zone)
    }

    /// The watch the registered announcements make, `None` where none of them registered. Its
    /// hearing of a sleep is its power half's, or its steps' where a resume is a step; a zone
    /// announcement hears no sleep.
    fn of(
        heard: Arc<Heard>,
        steps: Option<imp::Steps>,
        power: Option<imp::Listening>,
        zone: Option<imp::Zone>,
    ) -> Option<Self> {
        if steps.is_none() && power.is_none() && zone.is_none() {
            return None;
        }
        let hears_sleep = power.is_some() || (STEPS_TELL_A_RESUME && steps.is_some());
        Some(Self {
            heard,
            steps,
            zone,
            _power: power,
            hears_sleep,
        })
    }

    /// A watch the system never tells, for a test that tells it by hand ([`Self::tell`]). It
    /// counts as hearing a sleep: the test tells it each one.
    ///
    /// # Errors
    /// Its doorbell's pipe could not be made.
    #[cfg(any(test, debug_assertions))]
    pub fn by_hand() -> io::Result<Self> {
        Ok(Self {
            heard: Arc::new(Heard::new()?),
            steps: None,
            zone: None,
            _power: None,
            hears_sleep: true,
        })
    }

    /// A watch of the system's step announcement alone, as [`Self::start`] makes one where the
    /// power half was refused: `None` where steps are not announced ([`STEPS_ANNOUNCED`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn steps_only() -> Option<Self> {
        Self::of(
            Arc::new(Heard::new().ok()?),
            imp::Steps::start(),
            None,
            None,
        )
    }

    /// Whether this watch hears a sleep's wake: its power half registered (macOS, Windows), or
    /// its step announcement also announces a resume (Linux's timer). A watch that hears steps
    /// alone (macOS with the power half refused) says nothing of a sleep the clock was not set
    /// across, so an owner that waits for a sleep keeps reading its own clocks.
    #[must_use]
    pub const fn hears_sleep(&self) -> bool {
        self.hears_sleep
    }

    /// Announce `moved` as the system would: the next poll of [`Self::fds`] reads ready.
    #[cfg(any(test, debug_assertions))]
    pub fn tell(&self, moved: Moved) {
        if let Some(slept) = moved.slept {
            *self
                .heard
                .slept
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(slept);
        }
        self.heard.stepped.fetch_or(moved.stepped, Ordering::SeqCst);
        self.heard.woke.fetch_or(moved.woke, Ordering::SeqCst);
        self.heard.zoned.fetch_or(moved.zoned, Ordering::SeqCst);
        self.heard.bell.ring();
    }

    /// The descriptors that poll readable once something moved: poll them, never read them.
    #[cfg(unix)]
    #[must_use]
    pub fn fds(&self) -> Vec<BorrowedFd<'_>> {
        let mut fds = vec![self.heard.bell.fd()];
        fds.extend(self.steps.as_ref().map(imp::Steps::fd));
        fds.extend(self.zone.as_ref().map(imp::Zone::fd));
        fds
    }

    /// The doorbell that is rung once something moved: wait on it ([`any`]), never clear it
    /// ([`Self::take`] does).
    #[cfg(windows)]
    #[must_use]
    pub fn bell(&self) -> &Doorbell {
        &self.heard.bell
    }

    /// What moved since the last take, every descriptor emptied first.
    #[must_use]
    pub fn take(&self) -> Moved {
        let stepped = self.steps.as_ref().is_some_and(imp::Steps::take);
        let zoned = self.zone.as_ref().is_some_and(imp::Zone::take);
        let mut moved = self.heard.take();
        moved.stepped |= stepped;
        moved.zoned |= zoned;
        moved
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Heard;
    use std::ffi::{CStr, c_char, c_int};
    use std::io;
    use std::os::fd::{AsRawFd as _, BorrowedFd, RawFd};
    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    /// The sleep and wake listener a watch keeps.
    pub(super) type Listening = crate::power::Listening;

    /// The name every `settimeofday` posts (`<notify_keys.h>`'s `kNotifyClockSet`).
    const CLOCK_SET: &CStr = c"com.apple.system.clock_set";

    /// The name posted when the system's time zone is set (`<notify_keys.h>`'s
    /// `kNotifyTimeZoneChange`).
    const TIME_ZONE: &CStr = c"com.apple.system.timezone";

    /// `NOTIFY_STATUS_OK`.
    const OK: u32 = 0;

    unsafe extern "C" {
        fn notify_register_file_descriptor(
            name: *const c_char,
            notify_fd: *mut c_int,
            flags: c_int,
            out_token: *mut c_int,
        ) -> u32;
        fn notify_cancel(token: c_int) -> u32;
        #[cfg(test)]
        fn notify_post(name: *const c_char) -> u32;
    }

    /// One libnotify registration on a descriptor of its own. libnotify owns the descriptor and
    /// closes it when the token is cancelled ([`Drop`]), so it is never an `OwnedFd` here.
    #[derive(Debug)]
    pub(super) struct Steps {
        fd: RawFd,
        token: c_int,
    }

    impl crate::spawn_lock::Descriptors for Steps {
        fn mark(&self) -> io::Result<()> {
            crate::keep_out_of_children(&self.fd())
        }
    }

    impl Steps {
        pub(super) fn start() -> Option<Self> {
            Self::on(CLOCK_SET).ok()
        }

        /// A registration for `name`, its descriptor nonblocking and kept out of children.
        pub(super) fn on(name: &CStr) -> io::Result<Self> {
            let steps = crate::kept_out(|| {
                let mut fd: c_int = -1;
                let mut token: c_int = 0;
                // SAFETY: `name` is a NUL-terminated string, and both out-pointers are this
                // frame's own integers; flags 0 asks libnotify for a fresh descriptor.
                let status = unsafe {
                    notify_register_file_descriptor(name.as_ptr(), &raw mut fd, 0, &raw mut token)
                };
                if status != OK || fd < 0 {
                    return Err(io::Error::other(format!(
                        "notify_register_file_descriptor: status {status}"
                    )));
                }
                Ok(Self { fd, token })
            })?;
            // SAFETY: F_GETFL / F_SETFL on the descriptor this registration holds open.
            let flags = unsafe { libc::fcntl(steps.fd, libc::F_GETFL) };
            // SAFETY: as above.
            if flags < 0
                || unsafe { libc::fcntl(steps.fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(steps)
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            // SAFETY: the descriptor stays open until `self` is dropped (libnotify closes it at
            // the cancel), and the borrow cannot outlive `self`.
            unsafe { BorrowedFd::borrow_raw(self.fd) }
        }

        /// Whether a post came since the last take: every token libnotify wrote (one 4-byte
        /// token per post) read off.
        pub(super) fn take(&self) -> bool {
            let mut posted = false;
            let mut bytes = [0_u8; 64];
            loop {
                // SAFETY: reads into this frame's buffer from the registration's own descriptor.
                let read = unsafe {
                    libc::read(
                        self.fd().as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if read <= 0 {
                    break;
                }
                posted = true;
            }
            posted
        }

        /// Post `name`, as a test that cannot set the clock tells its own registration.
        #[cfg(test)]
        pub(super) fn post(name: &CStr) -> bool {
            // SAFETY: a NUL-terminated name.
            unsafe { notify_post(name.as_ptr()) == OK }
        }
    }

    impl Drop for Steps {
        fn drop(&mut self) {
            // SAFETY: the token this registration owns, cancelled once; libnotify closes its
            // descriptor with it.
            let _ = unsafe { notify_cancel(self.token) };
        }
    }

    /// The registration for a new time zone: a [`Steps`] on [`TIME_ZONE`].
    #[derive(Debug)]
    pub(super) struct Zone(Steps);

    impl Zone {
        pub(super) fn start() -> Option<Self> {
            Steps::on(TIME_ZONE).ok().map(Self)
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            self.0.fd()
        }

        pub(super) fn take(&self) -> bool {
            self.0.take()
        }
    }

    /// The sleep and wake listener whose announcements ring `heard`.
    pub(super) fn power(heard: &Arc<Heard>) -> Option<crate::power::Listening> {
        let heard = Arc::clone(heard);
        crate::power::listen(Box::new(move |power| match power {
            crate::power::Power::WillSleep => heard.will_sleep(Instant::now(), SystemTime::now()),
            crate::power::Power::Woke => heard.woke(Instant::now(), SystemTime::now()),
        }))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Heard;
    use std::ffi::c_int;
    use std::io;
    use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
    use std::sync::Arc;

    /// The sleep and wake listener a watch keeps (never made here).
    pub(super) type Listening = crate::power::Listening;

    const TFD_NONBLOCK: c_int = libc::O_NONBLOCK;
    const TFD_CLOEXEC: c_int = libc::O_CLOEXEC;
    const TFD_TIMER_ABSTIME: c_int = 1;
    const TFD_TIMER_CANCEL_ON_SET: c_int = 2;

    #[repr(C)]
    struct Itimerspec {
        interval: libc::timespec,
        value: libc::timespec,
    }

    unsafe extern "C" {
        fn timerfd_create(clockid: c_int, flags: c_int) -> c_int;
        fn timerfd_settime(
            fd: c_int,
            flags: c_int,
            new: *const Itimerspec,
            old: *mut Itimerspec,
        ) -> c_int;
    }

    /// The far-future `CLOCK_REALTIME` timer the kernel cancels at every set and resume.
    #[derive(Debug)]
    pub(super) struct Steps {
        fd: OwnedFd,
    }

    impl Steps {
        pub(super) fn start() -> Option<Self> {
            let fd = crate::kept_out(|| {
                // SAFETY: plain syscall; a non-negative result is a descriptor this call owns.
                let fd =
                    unsafe { timerfd_create(libc::CLOCK_REALTIME, TFD_NONBLOCK | TFD_CLOEXEC) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `fd` was just made and is owned by nobody else.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            })
            .ok()?;
            let steps = Self { fd };
            steps.arm().then_some(steps)
        }

        /// Arm (again, after a cancel) for the far future, cancelled on a set.
        fn arm(&self) -> bool {
            let zero = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let spec = Itimerspec {
                interval: zero,
                value: libc::timespec {
                    tv_sec: libc::time_t::MAX,
                    tv_nsec: 0,
                },
            };
            // SAFETY: the timer this owns, a spec on this frame, no old value asked for.
            unsafe {
                timerfd_settime(
                    self.fd.as_raw_fd(),
                    TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET,
                    &raw const spec,
                    std::ptr::null_mut(),
                ) == 0
            }
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }

        /// Whether the timer was cancelled (a set or a resume) since the last take; re-armed if so.
        pub(super) fn take(&self) -> bool {
            let mut expirations = 0_u64;
            // SAFETY: an 8-byte read into this frame's counter from the timer this owns.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    (&raw mut expirations).cast(),
                    std::mem::size_of::<u64>(),
                )
            };
            let cancelled =
                read < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECANCELED);
            if cancelled {
                let _ = self.arm();
            }
            cancelled
        }
    }

    /// An inotify watch of `/etc`, which hears `/etc/localtime` made anew: `timedatectl
    /// set-timezone` (and a package's `ln -sf`) renames a new link over it, and a watch of the
    /// link itself would go with the old one.
    #[derive(Debug)]
    pub(super) struct Zone {
        fd: OwnedFd,
    }

    impl Zone {
        pub(super) fn start() -> Option<Self> {
            let fd = crate::kept_out(|| {
                // SAFETY: plain syscall; a non-negative result is a descriptor this call owns.
                let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `fd` was just made and is owned by nobody else.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            })
            .ok()?;
            /// `<sys/inotify.h>`'s bit for a write's close, which the generated libc does not carry.
            const IN_CLOSE_WRITE: u32 = 0x0000_0008;
            // SAFETY: the instance this owns and a NUL-terminated path.
            let wd = unsafe {
                libc::inotify_add_watch(
                    fd.as_raw_fd(),
                    c"/etc".as_ptr(),
                    libc::IN_CREATE | libc::IN_MOVED_TO | IN_CLOSE_WRITE | libc::IN_DELETE,
                )
            };
            (wd >= 0).then_some(Self { fd })
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }

        /// Whether an event of `/etc` named `localtime` came since the last take: every event
        /// read off, those of its other entries passed over.
        pub(super) fn take(&self) -> bool {
            /// `<sys/inotify.h>`'s event head: wd, mask, cookie, len (the generated libc carries
            /// no `inotify_event`).
            const HEAD: usize = 16;
            let mut zoned = false;
            let mut buf = [0_u8; 4096];
            loop {
                // SAFETY: reads into this frame's buffer from the instance this owns.
                let read =
                    unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                let Ok(got) = usize::try_from(read) else {
                    return zoned;
                };
                if got == 0 {
                    return zoned;
                }
                let mut at = 0;
                while at + HEAD <= got {
                    let mut len = [0_u8; 4];
                    len.copy_from_slice(&buf[at + 12..at + 16]);
                    let len = usize::try_from(u32::from_ne_bytes(len)).unwrap_or(usize::MAX);
                    let name = buf
                        .get(at + HEAD..(at + HEAD).saturating_add(len).min(got))
                        .unwrap_or_default();
                    let name = name.split(|byte| *byte == 0).next().unwrap_or_default();
                    zoned |= name == b"localtime";
                    at = at.saturating_add(HEAD).saturating_add(len);
                }
            }
        }
    }

    /// Linux announces a resume on the timer itself: no listener.
    pub(super) fn power(_heard: &Arc<Heard>) -> Option<crate::power::Listening> {
        None
    }
}

#[cfg(windows)]
#[path = "wall_watch_win.rs"]
mod imp;

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
mod imp {
    use super::Heard;
    use std::os::fd::BorrowedFd;
    use std::sync::Arc;

    /// The sleep and wake listener a watch keeps (never made here).
    pub(super) type Listening = crate::power::Listening;

    /// Never made: nothing announces a step here.
    #[derive(Debug)]
    pub(super) enum Steps {}

    impl Steps {
        pub(super) fn start() -> Option<Self> {
            None
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            match *self {}
        }

        pub(super) fn take(&self) -> bool {
            match *self {}
        }
    }

    /// Never made: nothing announces a new time zone here.
    #[derive(Debug)]
    pub(super) enum Zone {}

    impl Zone {
        pub(super) fn start() -> Option<Self> {
            None
        }

        pub(super) fn fd(&self) -> BorrowedFd<'_> {
            match *self {}
        }

        pub(super) fn take(&self) -> bool {
            match *self {}
        }
    }

    pub(super) fn power(_heard: &Arc<Heard>) -> Option<crate::power::Listening> {
        None
    }
}

#[cfg(test)]
#[path = "wall_watch_tests.rs"]
mod tests;
