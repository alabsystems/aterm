// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Upgrade activation watches and process exit waiters for the window host.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a mark whose own directory does not exist yet (atpkg's marker
/// before a first install) is looked at again while its watch sits on an
/// ancestor.
#[cfg(target_os = "macos")]
const ANCESTOR_RECHECK: Duration = Duration::from_millis(250);

/// One host's watch on what a newer build's landing moves: where a Claude Code or Codex build
/// lands ([`Self::new`]), each agent's own marks (agy's and Muse Code's, [`Self::with_agent`]),
/// the owner's word ([`Self::with_word`]) and the side marks ([`Self::with_side`]).
pub struct ActivationWake {
    #[cfg(target_os = "macos")]
    marks: Vec<Mark>,
    #[cfg(target_os = "macos")]
    kq: Option<std::sync::Arc<std::os::fd::OwnedFd>>,
    /// Off macOS, which has no directory watch: the marks looked at once
    /// every fallback ([`Polled`]).
    #[cfg(not(target_os = "macos"))]
    polled: Polled,
    /// Fault injection: the kqueue's vnode events are lost (read as the
    /// wait running out), as an ancestor's may be.
    #[cfg(all(test, target_os = "macos"))]
    deaf: bool,
    /// How many times [`Self::wait_macos`] parked in its kqueue or deaf sleep.
    #[cfg(all(test, target_os = "macos"))]
    parks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The filter the last [`kq_wait`] reported (`None` on timeout or before parking).
    #[cfg(all(test, target_os = "macos"))]
    last_event: Option<i16>,
}

/// Wakes a parked [`ActivationWake::wait`] at once (the host's shutdown).
#[derive(Clone)]
pub struct WakeTrigger {
    #[cfg(target_os = "macos")]
    kq: Option<std::sync::Arc<std::os::fd::OwnedFd>>,
    #[cfg(not(target_os = "macos"))]
    pulled: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl WakeTrigger {
    /// Wake the parked wait.
    pub fn pull(&self) {
        #[cfg(target_os = "macos")]
        if let Some(kq) = &self.kq {
            use std::os::fd::AsRawFd as _;
            let fire = user_event(libc::NOTE_TRIGGER, 0);
            // SAFETY: one initialized change entry on a live kqueue, and no output slot.
            unsafe {
                libc::kevent(
                    kq.as_raw_fd(),
                    &fire,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                );
            }
        }
        #[cfg(not(target_os = "macos"))]
        ring(&self.pulled);
    }
}

impl ActivationWake {
    /// The wake, armed at atpkg's marker and the managed twin (off macOS, where atpkg publishes no
    /// marker, at Claude Code's and Codex's twins) and at the native install's link (a copy on
    /// Windows) under `home` (none without a home) as they stand now.
    #[must_use]
    pub fn new(home: Option<&Path>) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::with_claude_sources(
                home,
                Box::new(configured_marker),
                Box::new(|| configured_twin("claude")),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {
                polled: Polled::with_claude_sources(
                    home,
                    vec![
                        Box::new(|| configured_twin("claude")),
                        Box::new(|| configured_twin("codex")),
                    ],
                ),
            }
        }
    }

    /// Watch the managed twin itself as well as its best-effort notice. A pass
    /// can move the twin even when publishing its notice failed.
    #[cfg(target_os = "macos")]
    fn with_claude_sources(
        home: Option<&Path>,
        notice: Box<dyn FnMut() -> Option<PathBuf> + Send>,
        twin: Box<dyn FnMut() -> Option<PathBuf> + Send>,
    ) -> Self {
        let native = home.map(native_launcher);
        Self::with_marks(vec![
            Mark::new(notice).rechecked_before_its_dir(),
            Mark::new(twin).rechecked_before_its_dir(),
            Mark::new(Box::new(move || native.clone())),
        ])
    }

    /// Also wake on the owner's word, the marker at `path` ([`super::upgrade_drive::word_marker`]).
    #[must_use]
    pub fn with_word(self, path: Option<PathBuf>) -> Self {
        #[cfg(target_os = "macos")]
        if let Some(path) = path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let mut wake = self;
            wake.marks
                .push(Mark::new(Box::new(move || Some(path.clone()))));
            return wake;
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(path) = path {
            let mut wake = self;
            wake.polled.waking(Box::new(move || Some(path.clone())));
            return wake;
        }
        self
    }

    /// Also watch atpkg's marker for `program`'s own twin (`atpkg::activation_notice::marker_path_of`;
    /// off macOS, where atpkg publishes no marker, `program`'s twin itself: `agent_twin`).
    #[must_use]
    pub fn with_agent(self, program: &'static str, told: Box<dyn FnMut() + Send>) -> Self {
        #[cfg(target_os = "macos")]
        {
            self.with_agent_mark(Box::new(move || configured_marker_of(program)), told)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut wake = self;
            wake.polled
                .with_agent(agent_twin(program, atpkg::store::resolve_configured), told);
            wake
        }
    }

    /// [`Self::with_agent`] at the marker `locate` names (asked again at
    /// every pass, as the first marker's is).
    #[cfg(target_os = "macos")]
    fn with_agent_mark(
        self,
        locate: Box<dyn FnMut() -> Option<PathBuf> + Send>,
        told: Box<dyn FnMut() + Send>,
    ) -> Self {
        let mut wake = self;
        let mut mark = Mark::new(locate).rechecked_before_its_dir();
        mark.told = Some(told);
        wake.marks.push(mark);
        wake
    }

    /// Also watch `path` for a listener of its own.
    #[must_use]
    pub fn with_side(self, path: Option<PathBuf>, told: Box<dyn FnMut() + Send>) -> Self {
        let mut wake = self;
        #[cfg(target_os = "macos")]
        if let Some(path) = path {
            let mut mark = Mark::new(Box::new(move || Some(path.clone())));
            mark.told = Some(told);
            wake.marks.push(mark);
        }
        #[cfg(not(target_os = "macos"))]
        wake.polled.sides.push(SidePoll::new(path, told));
        wake
    }

    #[cfg(target_os = "macos")]
    fn with_marks(marks: Vec<Mark>) -> Self {
        Self {
            marks,
            kq: kqueue().ok().map(std::sync::Arc::new),
            #[cfg(test)]
            deaf: false,
            #[cfg(test)]
            parks: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            last_event: None,
        }
    }

    /// What wakes this wait from another thread.
    #[must_use]
    pub fn trigger(&self) -> WakeTrigger {
        WakeTrigger {
            #[cfg(target_os = "macos")]
            kq: self.kq.clone(),
            #[cfg(not(target_os = "macos"))]
            pulled: std::sync::Arc::clone(&self.polled.pulled),
        }
    }

    /// Park until a newer build is detected (`true`), or until `fallback` elapses or [`WakeTrigger`] fires (`false`).
    /// Off macOS, which has no directory watch, the marks are looked at once `fallback` elapses
    /// ([`Polled::wait`]).
    pub fn wait(&mut self, fallback: Duration) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.wait_macos(fallback)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.polled.wait(fallback)
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_macos(&mut self, fallback: Duration) -> bool {
        #[cfg(test)]
        {
            self.last_event = None;
        }
        let now = Instant::now();
        let deadline = now
            .checked_add(fallback)
            .unwrap_or_else(|| now + Duration::from_secs(86_400));
        loop {
            // A Settings edit may have moved the package prefix: the builds
            // it holds are others, and the watch moves with it.
            if self
                .marks
                .iter_mut()
                .fold(false, |woke, m| m.relocated() | woke)
            {
                return true;
            }
            if self.any_changed() {
                return true;
            }
            let Some(kq) = self.kq.clone() else {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                return false;
            };
            for m in &mut self.marks {
                m.ensure_watch(&kq);
            }
            if self.any_changed() {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let early = self.marks.iter().any(Mark::watching_an_ancestor);
            let wait_for = if early {
                remaining.min(ANCESTOR_RECHECK)
            } else {
                remaining
            };
            #[cfg(test)]
            let woke = {
                self.parks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.deaf {
                    std::thread::sleep(wait_for);
                    Ok(None)
                } else {
                    let res = kq_wait(&kq, wait_for);
                    if let Ok(ev) = res {
                        self.last_event = ev;
                    }
                    res
                }
            };
            #[cfg(not(test))]
            let woke = kq_wait(&kq, wait_for);
            match woke {
                Ok(Some(libc::EVFILT_VNODE)) => {}
                Ok(None) if early => {}
                Ok(_) => return false,
                Err(_) if early => std::thread::sleep(wait_for),
                Err(_) => {
                    std::thread::sleep(remaining);
                    return false;
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl ActivationWake {
    fn any_changed(&mut self) -> bool {
        self.marks.iter_mut().fold(false, |woke, m| {
            let changed = m.changed();
            match m.told.as_mut() {
                Some(told) => {
                    if changed {
                        told();
                    }
                    woke
                }
                None => changed | woke,
            }
        })
    }
}

fn moved<T: PartialEq>(seen: &mut Option<T>, next: Option<T>) -> bool {
    if next == *seen {
        return false;
    }
    *seen = next;
    true
}

/// What a marker IS to the fallback's poll ([`SidePoll`], [`PolledMark`]): its
/// file number (the inode on unix, the file index on Windows; `0` elsewhere),
/// its length and when it was last modified.
#[cfg(any(test, not(target_os = "macos")))]
type SideStamp = (u64, u64, Option<std::time::SystemTime>);

/// [`SideStamp`] of the file at `path`; `None` where no file is there.
#[cfg(any(test, not(target_os = "macos")))]
fn side_stamp(path: &Path) -> Option<SideStamp> {
    #[cfg(windows)]
    let (m, ino) = {
        let file = std::fs::File::open(path).ok()?;
        let m = file.metadata().ok()?;
        let ino = super::session_usage::open_identity(&file, &m).map_or(0, |(_, index)| index);
        (m, ino)
    };
    #[cfg(not(windows))]
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt as _;
        m.ino()
    };
    #[cfg(not(any(unix, windows)))]
    let ino = 0;
    Some((ino, m.len(), m.modified().ok()))
}

/// A side mark as the fallback's poll keeps it ([`ActivationWake::with_side`]
/// off macOS): where the marker lives, its stamp at the last look, and its
/// listener.
#[cfg(any(test, not(target_os = "macos")))]
struct SidePoll {
    path: Option<PathBuf>,
    seen: Option<SideStamp>,
    told: Box<dyn FnMut() + Send>,
}

#[cfg(any(test, not(target_os = "macos")))]
impl SidePoll {
    fn new(path: Option<PathBuf>, told: Box<dyn FnMut() + Send>) -> Self {
        Self {
            seen: path.as_deref().and_then(side_stamp),
            path,
            told,
        }
    }

    fn look(&mut self) -> bool {
        let next = self.path.as_deref().and_then(side_stamp);
        let changed = moved(&mut self.seen, next);
        if changed {
            (self.told)();
        }
        changed
    }
}

/// Where a mark lives, asked again at every look: a Settings edit may move the package prefix.
#[cfg(any(test, not(target_os = "macos")))]
type Locate = Box<dyn FnMut() -> Option<PathBuf> + Send>;

/// Where the poll looks for `program`'s own twin ([`ActivationWake::with_agent`] off macOS, where
/// `layout` is `atpkg::store::resolve_configured`): [`twin_under`] the package prefix `layout`
/// names at that look, or nowhere while it names none.
#[cfg(any(test, not(target_os = "macos")))]
fn agent_twin(
    program: &'static str,
    mut layout: impl FnMut() -> Option<atpkg::store::Layout> + Send + 'static,
) -> Locate {
    Box::new(move || twin_under(&layout()?, program))
}

/// The wake where no directory watch exists (off macOS): what a newer build moves (the managed
/// twins, the native link, the owner's word), the agents' own twins and the side marks are looked
/// at once every fallback, and a wait answers `true` only for a mark that moved, as the watch does
/// on macOS.
#[cfg(any(test, not(target_os = "macos")))]
struct Polled {
    /// What a [`WakeTrigger`] pulls ([`ring`]).
    pulled: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    marks: Vec<PolledMark>,
    /// The side marks ([`ActivationWake::with_side`]), told only when changed.
    sides: Vec<SidePoll>,
}

#[cfg(any(test, not(target_os = "macos")))]
impl Polled {
    /// The marks of Claude Code's builds: each of `twins` and the native install's link under
    /// `home` (none without a home).
    fn with_claude_sources(home: Option<&Path>, twins: Vec<Locate>) -> Self {
        let native = home.map(native_launcher);
        let mut wake = Self {
            pulled: std::sync::Arc::default(),
            marks: Vec::new(),
            sides: Vec::new(),
        };
        for twin in twins {
            wake.waking(twin);
        }
        wake.waking(Box::new(move || native.clone()));
        wake
    }

    /// Also answer `true` once the mark `locate` names moves.
    fn waking(&mut self, locate: Locate) {
        self.marks.push(PolledMark::new(locate));
    }

    /// Also tell `told` once the mark `locate` names moves, without answering `true` for the
    /// general wake ([`ActivationWake::with_agent`]).
    fn with_agent(&mut self, locate: Locate, told: Box<dyn FnMut() + Send>) {
        let mut mark = PolledMark::new(locate);
        mark.told = Some(told);
        self.marks.push(mark);
    }

    /// Park for `fallback`, cut short by a pulled trigger (`false`, nothing looked at); then tell
    /// each side or agent mark that changed, and answer whether a general mark moved.
    fn wait(&mut self, fallback: Duration) -> bool {
        let (flag, bell) = &*self.pulled;
        let pulled = flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut pulled, _) = bell
            .wait_timeout_while(pulled, fallback, |p| !*p)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fired = std::mem::take(&mut *pulled);
        drop(pulled);
        if fired {
            return false;
        }
        for side in &mut self.sides {
            side.look();
        }
        self.marks.iter_mut().fold(false, |woke, m| {
            let changed = m.changed();
            match m.told.as_mut() {
                Some(told) => {
                    if changed {
                        told();
                    }
                    woke
                }
                None => changed | woke,
            }
        })
    }
}

/// Wake a parked [`Polled::wait`] at once.
#[cfg(any(test, not(target_os = "macos")))]
fn ring(pulled: &(std::sync::Mutex<bool>, std::sync::Condvar)) {
    let (flag, bell) = pulled;
    *flag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    bell.notify_all();
}

/// One thing a newer build moves, as [`Polled`] keeps it: where it lives, its stamp at the last
/// look ([`SideStamp`], through a link to the file it names) and, for an agent's own mark
/// ([`Polled::with_agent`]), the listener `told` of its move, which ends no wait.
#[cfg(any(test, not(target_os = "macos")))]
struct PolledMark {
    locate: Locate,
    path: Option<PathBuf>,
    seen: Option<SideStamp>,
    told: Option<Box<dyn FnMut() + Send>>,
}

#[cfg(any(test, not(target_os = "macos")))]
impl PolledMark {
    fn new(mut locate: Locate) -> Self {
        let path = locate();
        Self {
            seen: path.as_deref().and_then(side_stamp),
            path,
            locate,
            told: None,
        }
    }

    /// Whether the mark moved since the last look: it lives elsewhere now, or its stamp changed.
    fn changed(&mut self) -> bool {
        let at = (self.locate)();
        if at != self.path {
            self.path = at;
            self.seen = self.path.as_deref().and_then(side_stamp);
            return true;
        }
        let next = self.path.as_deref().and_then(side_stamp);
        moved(&mut self.seen, next)
    }
}

/// Wait at most `limit` for `pid` to exit.
#[must_use]
pub fn wait_exit(pid: u32, limit: Duration) -> bool {
    wait_exit_inner(
        pid,
        None,
        limit,
        #[cfg(all(test, target_os = "macos"))]
        None,
    )
    .0
}

/// Wait at most `limit` for the process `pid` names while it is the one born at `start`
/// (`upgrade_drive::kernel_start`'s spelling) to exit: true once its pid is free or names a
/// process born at another time, never by another process's exit. The watch is armed before
/// the start is read, so it watches that process.
#[must_use]
pub fn wait_exit_as(pid: u32, start: &str, limit: Duration) -> bool {
    wait_exit_inner(
        pid,
        Some(start),
        limit,
        #[cfg(all(test, target_os = "macos"))]
        None,
    )
    .0
}

fn wait_exit_inner(
    pid: u32,
    born: Option<&str>,
    limit: Duration,
    #[cfg(all(test, target_os = "macos"))] parks: Option<&std::sync::atomic::AtomicUsize>,
) -> (bool, Option<i16>) {
    let reborn = || {
        born.is_some_and(|born| {
            super::upgrade_drive::kernel_start(pid).is_some_and(|now| now != born)
        })
    };
    // Gone by the one rule every reader of a process's life holds ([`super::upgrade_drive::alive`],
    // `aterm_uds::process::process_presence`): an exited task with no other thread left, never a
    // main thread that exited while its other threads run, so the exit this wait reports is the
    // one [`super::upgrade_drive::St::agent_exit`] reads next.
    let gone = || !super::upgrade_drive::alive(pid) || reborn();
    #[cfg(target_os = "macos")]
    if let Ok(kq) = kqueue() {
        use std::os::fd::AsRawFd as _;
        let watch = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one initialized change entry on a fresh kqueue, no output.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &watch,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            let esrch = std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            return (esrch || gone(), None);
        }
        if gone() {
            return (true, None);
        }
        #[cfg(test)]
        if let Some(p) = parks {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let ev = kq_wait(&kq, limit).ok().flatten();
        return (ev == Some(libc::EVFILT_PROC) || gone(), ev);
    }
    let start = Instant::now();
    while start.elapsed() < limit {
        if gone() {
            return (true, None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    (gone(), None)
}

/// A kqueue of our own, close-on-exec, the trigger's user event registered
/// on it for its life.
#[cfg(target_os = "macos")]
fn kqueue() -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: kqueue creates a new descriptor owned by this process.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` is fresh and exclusively ours.
    let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // SAFETY: F_SETFD changes only this descriptor's close-on-exec flag.
    unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    let add = user_event(0, libc::EV_ADD | libc::EV_CLEAR);
    // SAFETY: one initialized change entry, and no output slot.
    let rc = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            &add,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kq)
}

/// The trigger's `EVFILT_USER` change entry (ident 0).
#[cfg(target_os = "macos")]
const fn user_event(fflags: u32, flags: u16) -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: libc::EVFILT_USER,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Park on `kq` for at most `timeout`: the filter of the event that woke it,
/// or `None` when the timeout passed.
#[cfg(target_os = "macos")]
fn kq_wait(kq: &std::os::fd::OwnedFd, timeout: Duration) -> std::io::Result<Option<i16>> {
    use std::os::fd::AsRawFd as _;
    kq_wait_by(timeout, |ts, event| {
        // SAFETY: one event slot and an initialized timeout, both live for the call.
        let rc = unsafe { libc::kevent(kq.as_raw_fd(), std::ptr::null(), 0, event, 1, ts) };
        if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(rc)
        }
    })
}

/// [`kq_wait`]'s loop over `wait_once`, one call of the kernel's wait with the timeout and the
/// event slot it is given: a call a signal interrupted (`EINTR`) is made again with what is left
/// of `timeout`, so an event that comes later in the wait still wakes it.
#[cfg(target_os = "macos")]
fn kq_wait_by(
    timeout: Duration,
    mut wait_once: impl FnMut(&libc::timespec, &mut libc::kevent) -> std::io::Result<libc::c_int>,
) -> std::io::Result<Option<i16>> {
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let left = deadline.map_or(timeout, |at| at.saturating_duration_since(Instant::now()));
        let ts = libc::timespec {
            tv_sec: libc::time_t::try_from(left.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(left.subsec_nanos()),
        };
        // SAFETY: a `kevent` holds integers and one raw pointer, each valid as all-zero bytes.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let rc = match wait_once(&ts, &mut event) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            rc => rc?,
        };
        if rc == 0 {
            return Ok(None);
        }
        if event.flags & libc::EV_ERROR != 0 {
            return Err(std::io::Error::from_raw_os_error(
                i32::try_from(event.data).unwrap_or(libc::EIO),
            ));
        }
        return Ok(Some(event.filter));
    }
}

/// One thing a newer build moves: where it lives (asked again at every
/// pass), its identity when last seen, and the watched directory holding it.
#[cfg(target_os = "macos")]
struct Mark {
    locate: Box<dyn FnMut() -> Option<PathBuf> + Send>,
    path: Option<PathBuf>,
    seen: Option<Identity>,
    /// The directory watched and its descriptor, registered on the kqueue
    /// (closing it removes the registration).
    watch: Option<(PathBuf, std::fs::File)>,
    /// Looked at again every [`ANCESTOR_RECHECK`] while its watch sits on an
    /// ancestor of its own directory.
    recheck: bool,
    /// A side mark's or an agent's own mark's listener ([`ActivationWake::with_side`],
    /// [`ActivationWake::with_agent`]), told of a change that ends no wait.
    told: Option<Box<dyn FnMut() + Send>>,
}

#[cfg(target_os = "macos")]
impl Mark {
    fn new(mut locate: Box<dyn FnMut() -> Option<PathBuf> + Send>) -> Self {
        let path = locate();
        Self {
            seen: path.as_deref().and_then(identity),
            path,
            locate,
            watch: None,
            recheck: false,
            told: None,
        }
    }

    const fn rechecked_before_its_dir(mut self) -> Self {
        self.recheck = true;
        self
    }

    fn watching_an_ancestor(&self) -> bool {
        self.recheck
            && self
                .path
                .as_deref()
                .and_then(Path::parent)
                .is_some_and(|own| self.watch.as_ref().is_none_or(|(dir, _)| dir != own))
    }

    fn relocated(&mut self) -> bool {
        let now = (self.locate)();
        if now == self.path {
            return false;
        }
        self.path = now;
        self.seen = self.path.as_deref().and_then(identity);
        self.watch = None;
        true
    }

    fn changed(&mut self) -> bool {
        let next = self.path.as_deref().and_then(identity);
        moved(&mut self.seen, next)
    }

    fn ensure_watch(&mut self, kq: &std::os::fd::OwnedFd) {
        let desired = self.path.as_deref().and_then(watch_dir);
        if self.watch.as_ref().map(|(dir, _)| dir) == desired.as_ref() {
            return;
        }
        self.watch = desired.and_then(|dir| watch(kq, &dir).ok().map(|f| (dir, f)));
    }
}

/// atpkg's marker under the configured package prefix. Where the prefix
/// exists, its notice directory is made here if no pass has made it yet
/// (`atpkg::activation_notice::prepare`), so the watch sits on that directory
/// and only a machine with no prefix at all runs the [`ANCESTOR_RECHECK`].
#[cfg(target_os = "macos")]
fn configured_marker() -> Option<PathBuf> {
    configured_marker_of("claude")
}

/// Whether `program`'s landing wakes only its own sessions ([`ActivationWake::with_agent`])
/// rather than every supervised session ([`ActivationWake::new`]).
#[must_use]
pub fn has_own_wake(program: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        atpkg::activation_notice::marker_name(program) != atpkg::activation_notice::CLAUDE_MARKER
    }
    #[cfg(not(target_os = "macos"))]
    {
        !matches!(program, "claude" | "codex")
    }
}

/// The managed entrypoint of `program` under the configured package prefix ([`twin_under`]),
/// independent of its activation notice.
fn configured_twin(program: &str) -> Option<PathBuf> {
    twin_under(&atpkg::store::resolve_configured()?, program)
}

/// `layout`'s `agents/` twin of `program`, where atpkg lays it (`atpkg::store::Layout::agent_shim`:
/// `agents/<program>`, `agents\<program>.cmd` on Windows).
fn twin_under(layout: &atpkg::store::Layout, program: &str) -> Option<PathBuf> {
    Some(layout.agent_shim(&atpkg::store::ToolName::new(program)?))
}

/// The native install's launcher under `home`: the `claude` link into its versions (macOS,
/// Linux), or `claude.exe`, the installer's copy of the live version (Windows).
fn native_launcher(home: &Path) -> PathBuf {
    home.join(".local")
        .join("bin")
        .join(format!("claude{}", std::env::consts::EXE_SUFFIX))
}

/// atpkg's marker of `program`'s twin under the configured package prefix
/// (`atpkg::activation_notice::marker_name`), as [`configured_marker`] is.
#[cfg(target_os = "macos")]
fn configured_marker_of(program: &str) -> Option<PathBuf> {
    atpkg::store::resolve_configured().map(|layout| marker_under(&layout, program))
}

/// `layout`'s marker path of `program`'s twin, its notice directory made
/// first where the prefix exists and the directory does not
/// ([`configured_marker`]).
#[cfg(target_os = "macos")]
fn marker_under(layout: &atpkg::store::Layout, program: &str) -> PathBuf {
    let marker = atpkg::activation_notice::marker_path_of(layout, program);
    if layout.prefix.is_dir() && marker.parent().is_some_and(|dir| !dir.is_dir()) {
        atpkg::activation_notice::prepare(layout);
    }
    marker
}

/// What a thing is: the file its path resolves to (through a link) and that
/// file's identity.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    target: PathBuf,
    dev: u64,
    ino: u64,
    len: u64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(target_os = "macos")]
fn identity(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let target = std::fs::canonicalize(path).ok()?;
    let m = std::fs::symlink_metadata(&target).ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    Some(Identity {
        target,
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        ctime: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

/// The directory a watch on `path` parks on: its own, or before that exists
/// the nearest existing ancestor.
#[cfg(target_os = "macos")]
fn watch_dir(path: &Path) -> Option<PathBuf> {
    path.parent()?
        .ancestors()
        .find(|p| p.is_dir())
        .map(PathBuf::from)
}

/// Open `dir` and register it on `kq` for entry changes.
#[cfg(target_os = "macos")]
fn watch(kq: &std::os::fd::OwnedFd, dir: &Path) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)?;
    if !file.metadata()?.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
    }
    let change = libc::kevent {
        ident: file.as_raw_fd() as libc::uintptr_t,
        filter: libc::EVFILT_VNODE,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
        fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one initialized change entry, and no output slot.
    let rc = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "upgrade_wake_tests.rs"]
mod tests;

/// The fallback's poll of a side marker ([`SidePoll`]), on every platform: off macOS it is the
/// only thing that tells a side mark's listener.
#[cfg(test)]
#[path = "upgrade_wake_poll_tests.rs"]
mod poll_tests;
