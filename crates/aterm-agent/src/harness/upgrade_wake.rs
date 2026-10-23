// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The pushes that tell the window's host a newer Claude Code or Codex is installed, and the one that says an agent has exited.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a mark whose own directory does not exist yet (atpkg's marker
#[cfg(target_os = "macos")]
const ANCESTOR_RECHECK: Duration = Duration::from_millis(250);

/// One host's watch on where a newer Claude Code or Codex build lands.
pub struct ActivationWake {
    #[cfg(target_os = "macos")]
    marks: Vec<Mark>,
    #[cfg(target_os = "macos")]
    kq: Option<std::sync::Arc<std::os::fd::OwnedFd>>,
    #[cfg(not(target_os = "macos"))]
    pulled: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    /// The side marks ([`Self::with_side`]), looked at on every fallback and
    #[cfg(not(target_os = "macos"))]
    sides: Vec<SidePoll>,
    /// Fault injection: the kqueue's vnode events are lost (read as the
    #[cfg(all(test, target_os = "macos"))]
    deaf: bool,
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
        {
            let (flag, bell) = &*self.pulled;
            *flag
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            bell.notify_all();
        }
    }
}

impl ActivationWake {
    /// The wake, armed at atpkg's marker and at the native install's link under `home` (none without a home) as they stand now.
    #[must_use]
    pub fn new(home: Option<&Path>) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::with_claude_sources(
                home,
                Box::new(configured_marker),
                Box::new(configured_claude_twin),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = home;
            Self {
                pulled: std::sync::Arc::default(),
                sides: Vec::new(),
            }
        }
    }

    /// Watch the managed twin itself as well as its best-effort notice. A pass
    #[cfg(target_os = "macos")]
    fn with_claude_sources(
        home: Option<&Path>,
        notice: Box<dyn FnMut() -> Option<PathBuf> + Send>,
        twin: Box<dyn FnMut() -> Option<PathBuf> + Send>,
    ) -> Self {
        let native = home.map(|h| h.join(".local/bin/claude"));
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
        let _ = path;
        self
    }

    /// Also watch atpkg's marker for `program`'s own twin (`atpkg::activation_notice::marker_path_of`).
    #[must_use]
    pub fn with_agent(self, program: &'static str, told: Box<dyn FnMut() + Send>) -> Self {
        #[cfg(target_os = "macos")]
        {
            self.with_agent_mark(Box::new(move || configured_marker_of(program)), told)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (program, told);
            self
        }
    }

    /// [`Self::with_agent`] at the marker `locate` names (asked again at
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
        wake.sides.push(SidePoll::new(path, told));
        wake
    }

    #[cfg(target_os = "macos")]
    fn with_marks(marks: Vec<Mark>) -> Self {
        Self {
            marks,
            kq: kqueue().ok().map(std::sync::Arc::new),
            #[cfg(test)]
            deaf: false,
        }
    }

    /// What wakes this wait from another thread.
    #[must_use]
    pub fn trigger(&self) -> WakeTrigger {
        WakeTrigger {
            #[cfg(target_os = "macos")]
            kq: self.kq.clone(),
            #[cfg(not(target_os = "macos"))]
            pulled: std::sync::Arc::clone(&self.pulled),
        }
    }

    /// Park until a newer build is detected (`true`), or until `fallback` elapses or [`WakeTrigger`] fires (`false`).
    pub fn wait(&mut self, fallback: Duration) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.wait_macos(fallback)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let (flag, bell) = &*self.pulled;
            let pulled = flag
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut pulled, _) = bell
                .wait_timeout_while(pulled, fallback, |p| !*p)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let woke = !std::mem::take(&mut *pulled);
            drop(pulled);
            if woke {
                for side in &mut self.sides {
                    side.look();
                }
            }
            woke
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_macos(&mut self, fallback: Duration) -> bool {
        let deadline = Instant::now() + fallback;
        loop {
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
            let woke = if self.deaf {
                std::thread::sleep(wait_for);
                Ok(None)
            } else {
                kq_wait(&kq, wait_for)
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
    /// Whether a wake mark changed since it was last seen. Every mark is
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

/// Whether a watched thing is another than when last seen, `next` being what
fn moved<T: PartialEq>(seen: &mut Option<T>, next: Option<T>) -> bool {
    if next == *seen {
        return false;
    }
    *seen = next;
    true
}

/// What a side marker IS to the fallback's poll ([`SidePoll`]): its file
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

    /// One look: the listener is told when the marker is another than at the
    fn look(&mut self) -> bool {
        let next = self.path.as_deref().and_then(side_stamp);
        let changed = moved(&mut self.seen, next);
        if changed {
            (self.told)();
        }
        changed
    }
}

/// Wait at most `limit` for `pid` to exit.
#[must_use]
pub fn wait_exit(pid: u32, limit: Duration) -> bool {
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
            return esrch || gone();
        }
        if gone() {
            return true;
        }
        return kq_wait(&kq, limit).is_ok_and(|e| e == Some(libc::EVFILT_PROC)) || gone();
    }
    let start = Instant::now();
    while start.elapsed() < limit {
        if gone() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    gone()
}

/// Whether `pid` has exited and waits for its parent to reap it, which
#[cfg(target_os = "linux")]
fn zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| stat_exited(&stat))
}

/// Whether one `/proc/<pid>/stat` line names a process that has exited: its
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
fn stat_exited(stat: &str) -> bool {
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
        .is_some_and(|state| matches!(state, 'Z' | 'X'))
}

/// [`zombie`] where the kernel names none (see there).
#[cfg(not(target_os = "linux"))]
fn zombie(_pid: u32) -> bool {
    false
}

/// A kqueue of our own, close-on-exec, the trigger's user event registered
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
fn user_event(fflags: u32, flags: u16) -> libc::kevent {
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
#[cfg(target_os = "macos")]
struct Mark {
    locate: Box<dyn FnMut() -> Option<PathBuf> + Send>,
    path: Option<PathBuf>,
    seen: Option<Identity>,
    /// The directory watched and its descriptor, registered on the kqueue
    watch: Option<(PathBuf, std::fs::File)>,
    /// Looked at again every [`ANCESTOR_RECHECK`] while its watch sits on an
    recheck: bool,
    /// A side mark's listener ([`ActivationWake::with_side`]), told of a
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

    /// This mark is looked at again while its own directory does not exist
    fn rechecked_before_its_dir(mut self) -> Self {
        self.recheck = true;
        self
    }

    /// Whether this mark's watch sits on an ancestor of its own directory,
    fn watching_an_ancestor(&self) -> bool {
        self.recheck
            && self
                .path
                .as_deref()
                .and_then(Path::parent)
                .is_some_and(|own| self.watch.as_ref().is_none_or(|(dir, _)| dir != own))
    }

    /// Whether the thing moved elsewhere (re-armed there).
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

    /// Whether the thing changed since it was last seen (then seen).
    fn changed(&mut self) -> bool {
        let next = self.path.as_deref().and_then(identity);
        moved(&mut self.seen, next)
    }

    /// Watch the nearest existing directory holding the thing, moving the
    fn ensure_watch(&mut self, kq: &std::os::fd::OwnedFd) {
        let desired = self.path.as_deref().and_then(watch_dir);
        if self.watch.as_ref().map(|(dir, _)| dir) == desired.as_ref() {
            return;
        }
        self.watch = desired.and_then(|dir| watch(kq, &dir).ok().map(|f| (dir, f)));
    }
}

/// atpkg's marker under the configured package prefix. Where the prefix
#[cfg(target_os = "macos")]
fn configured_marker() -> Option<PathBuf> {
    configured_marker_of("claude")
}

/// The managed Claude Code entrypoint, independent of its activation notice.
#[cfg(target_os = "macos")]
fn configured_claude_twin() -> Option<PathBuf> {
    Some(
        atpkg::store::resolve_configured()?
            .agents_dir()
            .join("claude"),
    )
}

/// atpkg's marker of `program`'s twin under the configured package prefix
#[cfg(target_os = "macos")]
fn configured_marker_of(program: &str) -> Option<PathBuf> {
    atpkg::store::resolve_configured().map(|layout| marker_under(&layout, program))
}

/// `layout`'s marker path of `program`'s twin, its notice directory made
#[cfg(target_os = "macos")]
fn marker_under(layout: &atpkg::store::Layout, program: &str) -> PathBuf {
    let marker = atpkg::activation_notice::marker_path_of(layout, program);
    if layout.prefix.is_dir() && marker.parent().is_some_and(|dir| !dir.is_dir()) {
        atpkg::activation_notice::prepare(layout);
    }
    marker
}

/// What a thing is: the file its path resolves to (through a link) and that
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

    /// [`stat_exited`] reads the state after the command's last `)`: `Z` or `X` is exited.
    #[test]
    fn a_proc_stat_line_names_an_exited_process_by_its_state() {
        assert!(stat_exited("4242 (claude) Z 4100 4242 4100 0 -1"));
        assert!(stat_exited("4242 (claude) X 4100"));
        assert!(!stat_exited("4242 (claude) S 4100 4242 4100 0 -1"));
        assert!(!stat_exited("4242 (a) Z b) R 4100"));
        assert!(!stat_exited(""));
    }

    /// A wake that must come is waited for this long: a hang detector, never a
    const HANG: Duration = Duration::from_secs(60);

    /// The fallback a parked wait is given where the test tells a push from the
    const FALLBACK: Duration = Duration::from_secs(120);

    fn fixed(path: PathBuf) -> Box<dyn FnMut() -> Option<PathBuf> + Send> {
        Box::new(move || Some(path.clone()))
    }

    /// A failed notice publication cannot hide a managed Claude Code update. The
    #[test]
    fn a_managed_claude_twin_move_is_seen_without_its_notice() {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-claude-twin-wake-")
            .tempdir()
            .unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.path().join("pkg"),
        };
        let twin = layout.agents_dir().join("claude");
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
        let root =
            std::env::temp_dir().join(format!("aterm-claude-upgrade-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send(woke).unwrap();
        });
        let tmp = root
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(".marker.tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(&tmp, &marker).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(true),
            "the marker replacement wakes the parked host before its fallback, and says so"
        );
        waiter.join().unwrap();
        let mut quiet = ActivationWake::with_marks(vec![Mark::new(fixed(marker.clone()))]);
        assert!(!quiet.wait(Duration::from_millis(50)));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The owner's word marker ([`ActivationWake::with_word`]) wakes a parked host; its
    #[test]
    fn the_owners_word_wakes_a_parked_host() {
        let root =
            std::env::temp_dir().join(format!("aterm-claude-upgrade-word-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let marker = super::super::upgrade_drive::word_marker(&root);
        let mut wake = ActivationWake::with_marks(Vec::new()).with_word(Some(marker.clone()));
        let dir = marker.parent().expect("a directory").to_path_buf();
        assert!(dir.is_dir(), "made up front");
        assert_eq!(watch_dir(&marker), Some(dir.clone()));
        std::fs::write(dir.join("unrelated"), b"x").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "a write beside the marker is no word"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wake.wait(FALLBACK)).unwrap();
        });
        let tmp = marker.with_extension("tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(&tmp, &marker).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(true),
            "the word wakes the parked host before its fallback"
        );
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A side mark ([`ActivationWake::with_side`]) is told to its listener and ends no wait,
    #[test]
    fn a_side_mark_tells_its_listener_and_ends_no_wait() {
        let root =
            std::env::temp_dir().join(format!("aterm-accounts-side-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = done_tx.send(parked.wait(FALLBACK));
        });
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
        let _ = std::fs::remove_dir_all(root);
    }

    /// AN AGENT'S OWN MARKER ([`ActivationWake::with_agent`], Muse Code's
    #[test]
    fn an_agents_own_marker_tells_its_listener_and_wakes_no_other_agent() {
        let root =
            std::env::temp_dir().join(format!("aterm-agent-marker-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
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
        let _ = std::fs::remove_dir_all(root);
    }

    /// Claude's native updater repointing `~/.local/bin/claude` wakes the parked host.
    #[test]
    fn a_repointed_native_link_wakes_the_host_and_its_neighbours_do_not() {
        let home = std::env::temp_dir().join(format!("aterm-native-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let versions = home.join(".local/share/claude/versions");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(versions.join("2.1.281"), b"old").unwrap();
        let link = bin.join("claude");
        std::os::unix::fs::symlink(versions.join("2.1.281"), &link).unwrap();
        let mut wake = ActivationWake::new(Some(&home));
        wake.marks
            .retain(|m| m.path.as_deref() == Some(link.as_path()));
        std::fs::write(versions.join("2.1.282"), b"new").unwrap();
        std::fs::write(bin.join("other-tool"), b"x").unwrap();
        assert!(
            !wake.wait(Duration::ZERO),
            "a download and a neighbour are not the new build in place"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wake.wait(FALLBACK)).unwrap();
        });
        let next = bin.join(".claude.next");
        std::os::unix::fs::symlink(versions.join("2.1.282"), &next).unwrap();
        std::fs::rename(&next, &link).unwrap();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(true),
            "the repointed link wakes the host"
        );
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    /// The trigger ends a parked wait at once, reporting no activation (the
    #[test]
    fn the_trigger_ends_a_parked_wait_at_once() {
        let mut wake = ActivationWake::with_marks(Vec::new());
        let trigger = wake.trigger();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(wake.wait(Duration::from_secs(600))));
        std::thread::sleep(Duration::from_millis(50));
        trigger.pull();
        assert_eq!(
            rx.recv_timeout(HANG).ok(),
            Some(false),
            "the pulled trigger ends the wait long before its ten-minute fallback, \
             reporting no activation"
        );
    }

    /// A first install: with the package prefix's parent missing, the watch sits on the
    #[test]
    fn first_install_wakes_even_when_the_package_parent_did_not_exist() {
        let root = std::env::temp_dir().join(format!(
            "aterm-claude-upgrade-first-install-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root
            .join("new-parent")
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        assert_eq!(watch_dir(&marker), Some(root.clone()));
        let mut wake = ActivationWake::with_marks(vec![
            Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
        ]);
        let waiter = std::thread::spawn(move || wake.wait(HANG));
        std::thread::sleep(Duration::from_millis(50));
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"1\n").unwrap();
        assert!(
            waiter.join().unwrap(),
            "the new marker must be observed before the fallback"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A lost ancestor wake: with every vnode event lost (the fault injected),
    #[test]
    fn first_install_rechecks_a_missing_path_after_a_lost_ancestor_notification() {
        let root = std::env::temp_dir().join(format!(
            "aterm-claude-upgrade-lost-ancestor-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root
            .join("new-parent")
            .join("pkg")
            .join(atpkg::activation_notice::NOTICE_DIR)
            .join(atpkg::activation_notice::CLAUDE_MARKER);
        let mut wake = ActivationWake::with_marks(vec![
            Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
        ]);
        wake.deaf = true;
        let started = Instant::now();
        let waiter = std::thread::spawn(move || wake.wait(FALLBACK));
        std::thread::sleep(Duration::from_millis(100));
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"1\n").unwrap();
        assert!(
            waiter.join().unwrap(),
            "a lost ancestor event must not delay a first install until the fallback"
        );
        assert!(started.elapsed() < HANG, "{:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An agent's exit is a push: [`wait_exit`] returns as the process ends,
    #[test]
    fn an_exit_is_seen_as_it_happens() {
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("0.3")
            .spawn()
            .unwrap();
        let pid = child.id();
        let reaper = std::thread::spawn(move || child.wait());
        let started = Instant::now();
        assert!(wait_exit(pid, FALLBACK));
        assert!(started.elapsed() < HANG, "{:?}", started.elapsed());
        reaper.join().unwrap().unwrap();
        assert!(wait_exit(pid, FALLBACK), "already gone");
        let mut runs = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(!wait_exit(runs.id(), Duration::from_millis(200)));
        runs.kill().unwrap();
        runs.wait().unwrap();
    }

    /// A zombie (ended, not yet reaped) is refused by `EVFILT_PROC` (`ESRCH`) while
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
        assert!(
            super::super::upgrade_drive::alive(pid),
            "kill(pid, 0) still finds a zombie"
        );
        let started = Instant::now();
        assert!(wait_exit(pid, FALLBACK), "a zombie has exited");
        assert!(started.elapsed() < HANG, "{:?}", started.elapsed());
        child.wait().unwrap();
    }

    /// Where the package prefix exists but no atpkg pass has made the notice
    #[test]
    fn an_existing_prefix_gets_its_notice_dir_and_no_recheck() {
        let root = std::env::temp_dir().join(format!("aterm-wake-prepare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
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
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The fallback's poll of a side marker ([`SidePoll`]), on every platform: off macOS it is the
#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A marker replaced as the accounts store replaces its own: a new file
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

    /// The fallback's poll tells a side listener only when the marker is another file: a
    #[test]
    fn a_fallback_tick_tells_a_side_listener_only_when_its_marker_changed() {
        let root =
            std::env::temp_dir().join(format!("aterm-accounts-side-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        let _ = std::fs::remove_dir_all(root);
    }
}
