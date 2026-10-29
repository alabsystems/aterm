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
        let deadline = Instant::now() + fallback;
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
        limit,
        #[cfg(all(test, target_os = "macos"))]
        None,
    )
    .0
}

fn wait_exit_inner(
    pid: u32,
    limit: Duration,
    #[cfg(all(test, target_os = "macos"))] parks: Option<&std::sync::atomic::AtomicUsize>,
) -> (bool, Option<i16>) {
    let gone = || !super::upgrade_drive::alive(pid) || zombie(pid);
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

/// Whether `pid` has exited and waits for its parent to reap it, which
/// `kill(pid, 0)` still finds.
#[cfg(target_os = "linux")]
fn zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| stat_exited(&stat))
}

/// Whether one `/proc/<pid>/stat` line names a process that has exited: its
/// state, the field after the command's last closing parenthesis (a command
/// may hold one), is `Z` (a zombie) or `X` (dead).
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn stat_exited(stat: &str) -> bool {
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
        .is_some_and(|state| matches!(state, 'Z' | 'X'))
}

/// [`zombie`] on Apple kernels (`proc_pidinfo` `SZOMB` or off the live list).
#[cfg(target_vendor = "apple")]
fn zombie(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(|p| {
        aterm_uds::process::process_presence(p) == aterm_uds::process::ProcessPresence::Exited
    })
}

/// [`zombie`] where the kernel names none (see there).
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
const fn zombie(_pid: u32) -> bool {
    false
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
    // SAFETY: `kevent` initializes this out-parameter before it is read.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    let ts = libc::timespec {
        tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
    };
    // SAFETY: one event slot and an initialized timeout, both live for the call.
    let rc = unsafe { libc::kevent(kq.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &ts) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if rc == 0 {
        return Ok(None);
    }
    if event.flags & libc::EV_ERROR != 0 {
        return Err(std::io::Error::from_raw_os_error(
            i32::try_from(event.data).unwrap_or(libc::EIO),
        ));
    }
    Ok(Some(event.filter))
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
mod tests {
    use super::*;

    #[test]
    fn a_proc_stat_line_names_an_exited_process_by_its_state() {
        assert!(stat_exited("4242 (claude) Z 4100 4242 4100 0 -1"));
        assert!(stat_exited("4242 (claude) X 4100"));
        assert!(!stat_exited("4242 (claude) S 4100 4242 4100 0 -1"));
        assert!(!stat_exited("4242 (a) Z b) R 4100"));
        assert!(!stat_exited(""));
    }

    const HANG: Duration = Duration::from_secs(60);

    const FALLBACK: Duration = Duration::from_secs(120);

    fn fixed(path: PathBuf) -> Box<dyn FnMut() -> Option<PathBuf> + Send> {
        Box::new(move || Some(path.clone()))
    }

    fn scratch(prefix: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .unwrap()
    }

    fn wait_parked(parks: &std::sync::atomic::AtomicUsize, at_least: usize) {
        let deadline = Instant::now() + HANG;
        while parks.load(std::sync::atomic::Ordering::SeqCst) < at_least {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {at_least} park(s)"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_managed_claude_twin_move_is_seen_without_its_notice() {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-claude-twin-wake-")
            .tempdir()
            .unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.path().join("pkg"),
        };
        let twin = twin_under(&layout, "claude").unwrap();
        let notice = atpkg::activation_notice::marker_path(&layout);
        std::fs::create_dir_all(layout.agents_dir()).unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        std::fs::write(&old, b"old captured build").unwrap();
        std::fs::write(&new, b"new captured build").unwrap();
        std::os::unix::fs::symlink(&old, &twin).unwrap();
        let mut wake =
            ActivationWake::with_claude_sources(None, fixed(notice.clone()), fixed(twin.clone()));
        wake.deaf = true;
        assert!(
            !wake.wait(Duration::ZERO),
            "the installed build is the baseline"
        );
        std::fs::write(layout.agents_dir().join("unrelated"), b"x").unwrap();
        assert!(!wake.wait(Duration::ZERO), "a neighbour is no activation");
        let replacement = layout.agents_dir().join("next");
        std::os::unix::fs::symlink(&new, &replacement).unwrap();
        std::fs::rename(&replacement, &twin).unwrap();
        assert!(!notice.exists(), "no activation notice was published");
        assert!(wake.wait(Duration::ZERO), "the managed entrypoint moved");
        assert!(!wake.wait(Duration::ZERO), "each change is told once");

        std::fs::write(&replacement, b"captured regular twin").unwrap();
        std::fs::rename(&replacement, &twin).unwrap();
        assert!(
            wake.wait(Duration::ZERO),
            "a regular twin replacement is news"
        );
        assert!(!wake.wait(Duration::ZERO));
    }

    #[test]
    fn marker_wakes_a_parked_host_and_unrelated_writes_do_not() {
        let dir = scratch("aterm-claude-upgrade-wake-");
        let root = dir.path();
        let marker = root
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(marker.clone()))]);
        std::fs::write(root.join("unrelated"), b"x").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "an unrelated parent write is not a Claude upgrade"
        );
        std::fs::create_dir(root.join("pkg")).unwrap();
        assert!(!wake.wait(Duration::ZERO));
        std::fs::create_dir(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR)).unwrap();
        assert_eq!(
            watch_dir(&marker),
            Some(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR))
        );
        std::fs::write(root.join("pkg/progress.json"), b"busy").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "an active download's progress writes wake nobody"
        );
        let parks = std::sync::Arc::clone(&wake.parks);
        let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send((woke, wake.last_event)).unwrap();
        });
        wait_parked(&parks, parks_before + 1);
        let tmp = root
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(".marker.tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(&tmp, &marker).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((true, Some(libc::EVFILT_VNODE))),
            "the marker replacement wakes the parked host on EVFILT_VNODE, and says so"
        );
        waiter.join().unwrap();
        let mut quiet = ActivationWake::with_marks(vec![Mark::new(fixed(marker))]);
        assert!(!quiet.wait(Duration::ZERO));
    }

    #[test]
    fn the_owners_word_wakes_a_parked_host() {
        let scratch_dir = scratch("aterm-claude-upgrade-word-");
        let root = scratch_dir.path();
        let marker = super::super::upgrade_drive::word_marker(root);
        let mut wake = ActivationWake::with_marks(Vec::new()).with_word(Some(marker.clone()));
        let dir = marker.parent().expect("a directory").to_path_buf();
        assert!(dir.is_dir(), "made up front");
        assert_eq!(watch_dir(&marker), Some(dir.clone()));
        std::fs::write(dir.join("unrelated"), b"x").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "a write beside the marker is no word"
        );
        let parks = std::sync::Arc::clone(&wake.parks);
        let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send((woke, wake.last_event)).unwrap();
        });
        wait_parked(&parks, parks_before + 1);
        let tmp = marker.with_extension("tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(&tmp, &marker).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((true, Some(libc::EVFILT_VNODE))),
            "the word wakes the parked host on EVFILT_VNODE"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn a_side_mark_tells_its_listener_and_ends_no_wait() {
        let scratch_dir = scratch("aterm-accounts-side-wake-");
        let root = scratch_dir.path();
        let word = root.join("word");
        let store = root.join("accounts");
        let marker = store.join("wake");
        let replace = |path: &Path| {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, b"").unwrap();
            std::fs::rename(&tmp, path).unwrap();
        };
        let told = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&told);
        let heard = move || told.load(std::sync::atomic::Ordering::SeqCst);
        let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(word.clone()))]).with_side(
            Some(marker.clone()),
            Box::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        assert!(!store.exists(), "the store's directory is not made for it");
        let now = Duration::ZERO;
        assert!(!wake.wait(now));
        assert_eq!(heard(), 0);
        std::fs::create_dir_all(&store).unwrap();
        replace(&marker);
        assert!(!wake.wait(now), "a side mark ends no wait");
        assert_eq!(heard(), 1);
        replace(&marker);
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2);
        std::fs::write(store.join("state.json"), b"{}").unwrap();
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2, "a write beside the marker is not it");
        replace(&word);
        assert!(wake.wait(now), "the word is a wake");
        assert_eq!(heard(), 2, "and no news of the side mark");

        let _ = std::fs::remove_dir_all(&store);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut parked = ActivationWake::with_marks(vec![Mark::new(fixed(word.clone()))])
            .with_side(
                Some(marker.clone()),
                Box::new(move || {
                    let _ = tx.send(());
                }),
            );
        let parks = std::sync::Arc::clone(&parked.parks);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = done_tx.send(parked.wait(FALLBACK));
        });
        wait_parked(&parks, 1);
        std::fs::create_dir_all(&store).unwrap();
        replace(&marker);
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(()),
            "the marker made under a directory made after the wait parked is told"
        );
        replace(&word);
        assert_eq!(
            done_rx.recv_timeout(HANG).ok(),
            Some(true),
            "the side mark ended no wait: the word did"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn an_agents_own_marker_tells_its_listener_and_wakes_no_other_agent() {
        let scratch_dir = scratch("aterm-agent-marker-wake-");
        let root = scratch_dir.path();
        let notices = root.join(atpkg::activation_notice::NOTICE_DIR);
        std::fs::create_dir_all(&notices).unwrap();
        let first = notices.join(atpkg::activation_notice::CLAUDE_MARKER);
        let own = notices.join(atpkg::activation_notice::marker_name("muse"));
        assert_ne!(first, own, "Muse Code's marker is its own");
        let replace = |path: &Path| {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, b"1\n").unwrap();
            std::fs::rename(&tmp, path).unwrap();
        };
        let told = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&told);
        let heard = move || told.load(std::sync::atomic::Ordering::SeqCst);
        let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(first.clone()))])
            .with_agent_mark(
                fixed(own.clone()),
                Box::new(move || {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            );
        let now = Duration::ZERO;
        assert!(!wake.wait(now));
        assert_eq!(heard(), 0, "nothing changed");
        replace(&own);
        assert!(
            !wake.wait(now),
            "a Muse landing wakes no other agent's worker"
        );
        assert_eq!(heard(), 1, "Muse Code's workers are told");
        replace(&own);
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2);
        std::fs::write(notices.join("progress.json"), b"{}").unwrap();
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2, "a write beside the markers is neither");
        replace(&first);
        assert!(wake.wait(now), "the first marker is every worker's wake");
        assert_eq!(heard(), 2, "and no news of Muse Code's own");
    }

    #[test]
    fn a_repointed_native_link_wakes_the_host_and_its_neighbours_do_not() {
        let scratch_dir = scratch("aterm-native-wake-");
        let home = scratch_dir.path();
        let versions = home.join(".local/share/claude/versions");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(versions.join("2.1.281"), b"old").unwrap();
        let link = bin.join("claude");
        std::os::unix::fs::symlink(versions.join("2.1.281"), &link).unwrap();
        let mut wake = ActivationWake::new(Some(home));
        wake.marks
            .retain(|m| m.path.as_deref() == Some(link.as_path()));
        std::fs::write(versions.join("2.1.282"), b"new").unwrap();
        std::fs::write(bin.join("other-tool"), b"x").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "a download and a neighbour are not the new build in place"
        );
        let parks = std::sync::Arc::clone(&wake.parks);
        let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send((woke, wake.last_event)).unwrap();
        });
        wait_parked(&parks, parks_before + 1);
        let next = bin.join(".claude.next");
        std::os::unix::fs::symlink(versions.join("2.1.282"), &next).unwrap();
        std::fs::rename(&next, &link).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((true, Some(libc::EVFILT_VNODE))),
            "the repointed link wakes the host on EVFILT_VNODE"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn the_trigger_ends_a_parked_wait_at_once() {
        let mut wake = ActivationWake::with_marks(Vec::new());
        let trigger = wake.trigger();
        let parks = std::sync::Arc::clone(&wake.parks);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send((woke, wake.last_event)).unwrap();
        });
        wait_parked(&parks, 1);
        trigger.pull();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((false, Some(libc::EVFILT_USER))),
            "the pulled trigger wakes kq_wait via EVFILT_USER, reporting no activation"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn first_install_wakes_even_when_the_package_parent_did_not_exist() {
        let scratch_dir = scratch("aterm-claude-upgrade-first-install-");
        let root = scratch_dir.path();
        let marker = root
            .join("new-parent")
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        assert_eq!(watch_dir(&marker), Some(root.to_path_buf()));
        let mut wake = ActivationWake::with_marks(vec![
            Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
        ]);
        let parks = std::sync::Arc::clone(&wake.parks);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wake.wait(FALLBACK)).unwrap();
        });
        wait_parked(&parks, 1);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"1\n").unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(true),
            "the new marker is observed once its ancestor tree appears"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn first_install_rechecks_a_missing_path_after_a_lost_ancestor_notification() {
        let scratch_dir = scratch("aterm-claude-upgrade-lost-ancestor-");
        let root = scratch_dir.path();
        let marker = root
            .join("new-parent")
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        let mut wake = ActivationWake::with_marks(vec![
            Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
        ]);
        wake.deaf = true;
        let parks = std::sync::Arc::clone(&wake.parks);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send((wake.wait(FALLBACK), wake.last_event)).unwrap();
        });
        wait_parked(&parks, 1);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"1\n").unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((true, None)),
            "ancestor recheck observes the new marker while deaf (without kqueue events)"
        );
        waiter.join().unwrap();
    }

    #[test]
    fn an_exit_is_seen_as_it_happens() {
        let mut child = aterm_uds::spawnfd::command("/bin/cat")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let pid = child.id();
        assert_eq!(wait_exit_inner(pid, Duration::ZERO, None), (false, None));
        assert!(!wait_exit(pid, Duration::ZERO));

        let parks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let parks_waiter = std::sync::Arc::clone(&parks);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wait_exit_inner(pid, FALLBACK, Some(&parks_waiter)))
                .unwrap();
        });
        wait_parked(&parks, 1);
        drop(stdin);
        child.wait().unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some((true, Some(libc::EVFILT_PROC))),
            "EVFILT_PROC reports the exit to kq_wait"
        );
        waiter.join().unwrap();
        assert!(wait_exit(pid, FALLBACK), "already gone");
    }

    #[test]
    fn a_zombie_has_exited() {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .unwrap();
        let pid = child.id();
        loop {
            // SAFETY: a zeroed out-parameter `waitid` fills, for our own child.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: as above; `WNOWAIT` leaves the child to `child.wait`.
            let rc =
                unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
            let err = std::io::Error::last_os_error();
            if rc == 0 {
                break;
            }
            assert_eq!(err.raw_os_error(), Some(libc::EINTR), "waitid: {err}");
        }
        assert_eq!(
            // SAFETY: signal 0 queries existence without delivering a signal.
            unsafe { libc::kill(pid as libc::pid_t, 0) },
            0,
            "kill(pid, 0) still finds a zombie"
        );
        assert!(zombie(pid), "a zombie reads as exited before reap");
        assert!(
            !super::super::upgrade_drive::alive(pid),
            "an unreaped zombie is not live"
        );
        let parks = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(
            wait_exit_inner(pid, FALLBACK, Some(&parks)),
            (true, None),
            "a zombie returns immediately as exited without parking"
        );
        assert_eq!(parks.load(std::sync::atomic::Ordering::SeqCst), 0);
        child.wait().unwrap();
    }

    #[test]
    fn an_existing_prefix_gets_its_notice_dir_and_no_recheck() {
        let scratch_dir = scratch("aterm-wake-prepare-");
        let root = scratch_dir.path();
        let prefix = root.join("pkg");
        std::fs::create_dir(&prefix).expect("prefix");
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        let marker = marker_under(&layout, "claude");
        assert!(marker.parent().is_some_and(Path::is_dir), "{marker:?}");
        let kq = kqueue().expect("kqueue");
        let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
        mark.ensure_watch(&kq);
        assert!(
            !mark.watching_an_ancestor(),
            "the watch is on the dir itself"
        );
        let absent = atpkg::store::Layout {
            prefix: root.join("none"),
        };
        let marker = marker_under(&absent, "claude");
        assert!(!root.join("none").exists());
        let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
        mark.ensure_watch(&kq);
        assert!(mark.watching_an_ancestor());
    }
}

/// The fallback's poll of a side marker ([`SidePoll`]), on every platform: off macOS it is the
/// only thing that tells a side mark's listener.
#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn replace(path: &Path) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, b"").unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    fn poll(path: Option<PathBuf>) -> (SidePoll, impl Fn() -> usize) {
        let told = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&told);
        let side = SidePoll::new(
            path,
            Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (side, move || told.load(Ordering::SeqCst))
    }

    /// Off macOS the fallback is a look at the marks ([`Polled`]), tested on every unix here: a
    /// wait answers `true` only for a mark that moved, once per move; a side mark is told and
    /// ends no wait; a pulled trigger answers `false` at once and leaves a move for the next look.
    #[cfg(unix)]
    #[test]
    fn the_polled_wake_answers_only_a_mark_that_moved() {
        const TICK: Duration = Duration::from_millis(20);
        const HANG: Duration = Duration::from_secs(60);
        let scratch_dir = aterm_tempfile::Builder::new()
            .prefix("aterm-polled-wake-")
            .tempdir()
            .unwrap();
        let root = scratch_dir.path();
        let layout = atpkg::store::Layout {
            prefix: root.join("pkg"),
        };
        let agents = layout.agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::write(&old, b"old build").unwrap();
        std::fs::write(&new, b"new build").unwrap();
        let twin = agents.join("claude");
        std::os::unix::fs::symlink(&old, &twin).unwrap();
        let home = root.join("home");
        let word = root.join("owner/word");
        let fixed = |path: PathBuf| -> Locate { Box::new(move || Some(path.clone())) };
        let mut wake = Polled::with_claude_sources(Some(&home), vec![fixed(twin.clone())]);
        wake.waking(fixed(word.clone()));
        let store = root.join("accounts");
        let (side, heard) = poll(Some(store.join("wake")));
        wake.sides.push(side);
        let muse_twin = agents.join("muse");
        std::os::unix::fs::symlink(&old, &muse_twin).unwrap();
        let muse_told = Arc::new(AtomicUsize::new(0));
        let muse_count = Arc::clone(&muse_told);
        wake.with_agent(
            agent_twin("muse", move || Some(layout.clone())),
            Box::new(move || {
                muse_count.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(
            wake.marks.last().and_then(|m| m.path.clone()),
            Some(muse_twin.clone()),
            "Muse Code's own mark is the twin atpkg lays for it"
        );

        assert!(!wake.wait(TICK), "nothing moved: no activation");
        assert_eq!(muse_told.load(Ordering::SeqCst), 0);
        let next_muse = agents.join("next-muse");
        std::os::unix::fs::symlink(&new, &next_muse).unwrap();
        std::fs::rename(&next_muse, &muse_twin).unwrap();
        assert!(
            !wake.wait(TICK),
            "an agent's own twin wakes no other agent's worker"
        );
        assert_eq!(
            (muse_told.load(Ordering::SeqCst), heard()),
            (1, 0),
            "the agent's own listener is told, and no other"
        );
        assert!(!wake.wait(TICK));
        assert_eq!(muse_told.load(Ordering::SeqCst), 1, "told once");
        std::fs::create_dir_all(word.parent().unwrap()).unwrap();
        replace(&word);
        assert!(wake.wait(TICK), "the owner's word");
        assert!(!wake.wait(TICK), "told once");
        let next = agents.join("next");
        std::os::unix::fs::symlink(&new, &next).unwrap();
        std::fs::rename(&next, &twin).unwrap();
        assert!(wake.wait(TICK), "the managed twin moved");
        assert_eq!(
            muse_told.load(Ordering::SeqCst),
            1,
            "Claude Code's landing tells no agent's own listener"
        );
        assert!(!wake.wait(TICK));
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::os::unix::fs::symlink(&new, native_launcher(&home)).unwrap();
        assert!(wake.wait(TICK), "the native link appeared");
        std::fs::write(agents.join("unrelated"), b"x").unwrap();
        assert!(!wake.wait(TICK), "a neighbour is no activation");

        std::fs::create_dir_all(&store).unwrap();
        replace(&store.join("wake"));
        assert!(!wake.wait(TICK), "a side mark ends no wait");
        assert_eq!(heard(), 1, "its listener is told");

        replace(&word);
        ring(&wake.pulled);
        assert!(!wake.wait(HANG), "a pulled trigger is no news");
        assert!(wake.wait(TICK), "the word it cut short is the next look's");
    }

    /// The twin the poll looks at is the file atpkg lays ([`twin_under`]; on Windows the `.cmd`),
    /// and a newer build's landing moves it for the next look, on every platform this runs on.
    #[test]
    fn the_polled_twin_is_the_one_atpkg_lays_and_a_landing_moves_it() {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-polled-twin-")
            .tempdir()
            .unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.path().canonicalize().unwrap(),
        };
        let claude = atpkg::store::ToolName::new("claude").unwrap();
        let land = |build: &str| {
            let package = layout.prefix.join("store/claude").join(build);
            let exe = package.join("bin").join(claude.exe_file());
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, format!("#!/bin/sh\necho {build}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            atpkg::store::mark_build_ready(&package).unwrap();
            atpkg::activate::activate_build(&layout, &package).unwrap();
            atpkg::activate::install_shims(
                &layout,
                &package,
                &["claude".to_owned()],
                atpkg::Aliases::Off,
            )
            .unwrap();
        };
        land("2026102801");
        let twin = twin_under(&layout, "claude").unwrap();
        assert!(twin.is_file(), "atpkg laid no twin at {}", twin.display());
        let at = twin.clone();
        let mut mark = PolledMark::new(Box::new(move || Some(at.clone())));
        assert!(!mark.changed(), "the laid build is the baseline");
        land("2026102802");
        assert!(mark.changed(), "a newer build's landing moved the twin");
        assert!(!mark.changed(), "told once");
    }

    #[test]
    fn a_fallback_tick_tells_a_side_listener_only_when_its_marker_changed() {
        let scratch_dir = aterm_tempfile::Builder::new()
            .prefix("aterm-accounts-side-poll-")
            .tempdir()
            .unwrap();
        let root = scratch_dir.path();
        let store = root.join("accounts");
        let marker = store.join("wake");

        let (mut none, heard_none) = poll(None);
        let (mut side, heard) = poll(Some(marker.clone()));
        for _ in 0..5 {
            assert!(!side.look() && !none.look());
        }
        assert_eq!((heard(), heard_none()), (0, 0), "nothing there: no news");

        std::fs::create_dir_all(&store).unwrap();
        replace(&marker);
        assert!(side.look(), "the marker was made");
        assert_eq!(heard(), 1);
        for _ in 0..5 {
            assert!(!side.look());
        }
        assert_eq!(heard(), 1, "left alone: no tick tells it again");
        replace(&marker);
        assert!(side.look(), "replaced");
        assert_eq!(heard(), 2);
        std::fs::write(store.join("state.json"), b"{}").unwrap();
        assert!(!side.look());
        assert_eq!(heard(), 2, "a write beside the marker is not it");
        let (mut late, heard_late) = poll(Some(marker.clone()));
        assert!(!late.look());
        assert_eq!(heard_late(), 0, "the baseline is no news");
        std::fs::remove_file(&marker).unwrap();
        assert!(side.look() && !side.look());
        assert_eq!(heard(), 3);
    }
}
