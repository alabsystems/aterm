// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE PUSHES that tell the window's host a newer Claude Code or Codex is
//! installed, and the one that says an agent has exited.
//!
//! A newer build arrives two ways, and each is watched:
//!
//! * atpkg atomically replaces one marker in a dedicated directory after a
//!   mutating pass leaves `agents/claude` or `agents/codex` pointing
//!   elsewhere (`atpkg::activation_notice`);
//! * Claude's own native updater writes the new build under
//!   `~/.local/share/claude/versions` and repoints `~/.local/bin/claude` at
//!   it — no atpkg notice fires for that, and a session running the native
//!   install upgrades only from this watch.
//!
//! And the OWNER'S WORD on an upgrade (`aterm harness upgrade <sid>
//! --now|--defer|--skip`, from another process) replaces a marker of its own
//! ([`super::upgrade_drive::word_marker`], [`ActivationWake::with_word`]): the
//! same wake, so the tab's worker takes the word at its next idle point.
//!
//! On macOS ONE kqueue watches the directory holding each (the nearest one
//! that exists, moved inward as it is created), and a change is a wake only
//! when the thing itself moved — the marker's file identity, the link's
//! target — so unrelated writes beside them wake nobody. The watch is only a
//! wake hint: the host's worker re-reads the builds before it acts
//! ([`super::upgrade_drive::due`]). The same kqueue carries a user event, so
//! the host's shutdown wakes the parked thread at once ([`WakeTrigger`]).
//!
//! The one timer left is the fallback a caller passes, which re-reads WHERE
//! the marker lives (a Settings edit may move the package prefix) — and,
//! only while atpkg's whole package prefix does not exist yet (before a
//! machine's first install; where the prefix exists the host makes the
//! marker's directory itself, [`configured_marker`]), a [`ANCESTOR_RECHECK`]
//! look at its path: a vnode watch on an outer
//! ancestor need not report what a burst of nested `mkdir`s creates under
//! it (m3, 2026-09-24: a lost ancestor wake held a first install until the
//! fallback). Off macOS there is no directory watch, and the fallback IS the
//! wake: a KNOWN POLL, every fallback period, cut short by the trigger.
//!
//! [`wait_exit`] is the push for an agent's end: `EVFILT_PROC`/`NOTE_EXIT` on
//! its pid, where the relaunch used to ask `kill(pid, 0)` every 250 ms.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a mark whose own directory does not exist yet is looked at
/// again while its watch sits on an ancestor (module header): atpkg's marker
/// before the first install creates it. Once the directory exists the watch
/// is on it, and the wait is the kernel's alone.
#[cfg(target_os = "macos")]
const ANCESTOR_RECHECK: Duration = Duration::from_millis(250);

/// One host's watch on where a newer Claude Code build lands.
pub struct ActivationWake {
    #[cfg(target_os = "macos")]
    marks: Vec<Mark>,
    #[cfg(target_os = "macos")]
    kq: Option<std::sync::Arc<std::os::fd::OwnedFd>>,
    #[cfg(not(target_os = "macos"))]
    pulled: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    /// Fault injection: the kqueue's vnode events are lost (read as the
    /// wait running out), as an ancestor's may be.
    #[cfg(all(test, target_os = "macos"))]
    deaf: bool,
}

/// Wakes a parked [`ActivationWake::wait`] at once (the host's shutdown):
/// the wait returns `false`, as a fallback does.
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
            // SAFETY: one initialized change entry on a live kqueue, and no
            // output slot.
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
    /// The wake, armed at atpkg's marker and at the native install's link
    /// under `home` (none without a home) as they stand now: only a later
    /// change wakes it.
    #[must_use]
    pub fn new(home: Option<&Path>) -> Self {
        #[cfg(target_os = "macos")]
        {
            let native = home.map(|h| h.join(".local/bin/claude"));
            let marks = vec![
                Mark::new(Box::new(configured_marker)).rechecked_before_its_dir(),
                Mark::new(Box::new(move || native.clone())),
            ];
            Self::with_marks(marks)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = home;
            Self {
                pulled: std::sync::Arc::default(),
            }
        }
    }

    /// Also wake on the owner's word ([`super::upgrade_drive::word_marker`]
    /// under the harness state `path` names): its directory is made here, so
    /// the watch sits on it, never on an ancestor. Off macOS the fallback is
    /// the only wake, for the word as for a build.
    #[must_use]
    pub fn with_word(mut self, path: Option<PathBuf>) -> Self {
        #[cfg(target_os = "macos")]
        if let Some(path) = path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            self.marks
                .push(Mark::new(Box::new(move || Some(path.clone()))));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = path;
        self
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

    /// Park until a newer build may be installed (`true`: atpkg's marker
    /// was replaced or moved with the package prefix, or the native link was
    /// repointed), else until `fallback` passes or the [`WakeTrigger`] is
    /// pulled (`false`). A failed watch waits out the same bounded fallback.
    /// Off macOS the fallback IS the wake — `true` after it, so the host
    /// still looks — and the trigger cuts it short (`false`).
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
            !std::mem::take(&mut *pulled)
        }
    }

    #[cfg(target_os = "macos")]
    fn wait_macos(&mut self, fallback: Duration) -> bool {
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
            if self
                .marks
                .iter_mut()
                .fold(false, |woke, m| m.changed() | woke)
            {
                return true;
            }
            let Some(kq) = self.kq.clone() else {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                return false;
            };
            for m in &mut self.marks {
                m.ensure_watch(&kq);
            }
            // Read BEFORE registration, register, then read AGAIN: a change
            // in the gap is either seen here or queued by kqueue. A wake
            // never depends on timing that gap.
            if self
                .marks
                .iter_mut()
                .fold(false, |woke, m| m.changed() | woke)
            {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            // Before its own directory exists (a first install), a mark is
            // looked at again every [`ANCESTOR_RECHECK`] (module header).
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
                // A directory entry moved: the next pass compares each mark
                // and moves a watch inward; unrelated writes cost nothing.
                Ok(Some(libc::EVFILT_VNODE)) => {}
                // The recheck's turn: look at the path again.
                Ok(None) if early => {}
                // The trigger, or the fallback.
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

/// Wait at most `limit` for `pid` to exit: `true` once it has (at once when
/// it already has), `false` when it still runs at the end. A push on macOS
/// (`EVFILT_PROC`/`NOTE_EXIT`); elsewhere `kill(pid, 0)` every 250 ms.
#[must_use]
pub fn wait_exit(pid: u32, limit: Duration) -> bool {
    let gone = || !super::upgrade_drive::alive(pid);
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
            // ESRCH: nothing by that pid to watch — it has exited.
            return gone();
        }
        // Registered, then read again: an exit in the gap is queued or seen.
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
    // The trigger's user event, registered once for the queue's life.
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
    /// ancestor of its own directory (atpkg's marker: module header).
    recheck: bool,
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
        }
    }

    /// This mark is looked at again while its own directory does not exist
    /// ([`Self::watching_an_ancestor`]).
    fn rechecked_before_its_dir(mut self) -> Self {
        self.recheck = true;
        self
    }

    /// Whether this mark's watch sits on an ancestor of its own directory,
    /// and it is to be looked at again ([`ANCESTOR_RECHECK`]).
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
        if next == self.seen {
            return false;
        }
        self.seen = next;
        true
    }

    /// Watch the nearest existing directory holding the thing, moving the
    /// watch inward as its directories are created.
    fn ensure_watch(&mut self, kq: &std::os::fd::OwnedFd) {
        let desired = self.path.as_deref().and_then(watch_dir);
        if self.watch.as_ref().map(|(dir, _)| dir) == desired.as_ref() {
            return;
        }
        self.watch = desired.and_then(|dir| watch(kq, &dir).ok().map(|f| (dir, f)));
    }
}

/// atpkg's marker under the configured package prefix — its notice
/// directory made where the prefix exists but no pass has made it yet
/// (`atpkg::activation_notice::prepare`), so the watch sits on that
/// directory itself and the [`ANCESTOR_RECHECK`] timer never runs there: it
/// ran at 4 Hz for the host's whole life on such a machine (the philosophy
/// review of 2026-09-25). Only a machine with no prefix at all — before its
/// first install ever — keeps the recheck, which that install ends.
#[cfg(target_os = "macos")]
fn configured_marker() -> Option<PathBuf> {
    atpkg::store::resolve_configured().map(|layout| marker_under(&layout))
}

/// `layout`'s marker path, its notice directory made first where the prefix
/// exists and the directory does not ([`configured_marker`]).
#[cfg(target_os = "macos")]
fn marker_under(layout: &atpkg::store::Layout) -> PathBuf {
    let marker = atpkg::activation_notice::marker_path(layout);
    if layout.prefix.is_dir() && marker.parent().is_some_and(|dir| !dir.is_dir()) {
        atpkg::activation_notice::prepare(layout);
    }
    marker
}

/// What a thing IS: the file its path resolves to (through a link) and that
/// file's identity. A marker replaced by a rename, and a link repointed at
/// another build, both read as another.
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
/// the nearest existing ancestor. For atpkg's marker a normal host watches
/// only `activation-notices/`, so `progress.json`'s atomic writes in the
/// package prefix never wake it; on a fresh machine even the package
/// prefix's parent may not exist yet, so the watch starts at the nearest
/// ancestor that does and moves inward as atpkg creates each directory.
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

    /// A wake that must come is waited for this long: a hang detector, never a
    /// latency budget (AGENTS.md).
    const HANG: Duration = Duration::from_secs(60);

    /// The fallback a parked wait is given where the test tells a push from the
    /// fallback running out: twice [`HANG`], so the one outcome lands inside
    /// the detector and the other far outside it whatever the machine's load.
    const FALLBACK: Duration = Duration::from_secs(120);

    fn fixed(path: PathBuf) -> Box<dyn FnMut() -> Option<PathBuf> + Send> {
        Box::new(move || Some(path.clone()))
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
        // Arm the parent watch before the writer starts. This makes the
        // unrelated-write assertion exercise a real parked watcher even on a
        // heavily loaded test machine.
        let kq = wake.kq.clone().unwrap();
        wake.marks[0].ensure_watch(&kq);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let woke = wake.wait(FALLBACK);
            tx.send(woke).unwrap();
        });
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(root.join("unrelated"), b"x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "an unrelated parent write is not a Claude upgrade"
        );
        std::fs::create_dir(root.join("pkg")).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::create_dir(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            watch_dir(&marker),
            Some(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR))
        );
        std::fs::write(root.join("pkg/progress.json"), b"busy").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "an active download's progress writes wake nobody"
        );
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
        // NEGATIVE CONTROL: a wait that only runs out its fallback reports
        // no activation, so the host does not look for nothing.
        let mut quiet = ActivationWake::with_marks(vec![Mark::new(fixed(marker.clone()))]);
        assert!(!quiet.wait(Duration::from_millis(50)));
        let _ = std::fs::remove_dir_all(root);
    }

    /// THE OWNER'S WORD is a push too ([`ActivationWake::with_word`]): the
    /// marker `aterm harness upgrade <sid> --now|--defer|--skip` replaces
    /// after writing the word wakes the parked host, and its directory is
    /// made up front so the watch sits on it. NEGATIVE CONTROL: a write beside
    /// it wakes nobody.
    #[test]
    fn the_owners_word_wakes_a_parked_host() {
        let root =
            std::env::temp_dir().join(format!("aterm-claude-upgrade-word-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let marker = super::super::upgrade_drive::word_marker(&root);
        let mut wake = ActivationWake::with_marks(Vec::new()).with_word(Some(marker.clone()));
        let dir = marker.parent().expect("a directory").to_path_buf();
        assert!(dir.is_dir(), "made up front");
        let kq = wake.kq.clone().unwrap();
        wake.marks[0].ensure_watch(&kq);
        assert_eq!(watch_dir(&marker), Some(dir.clone()));
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wake.wait(FALLBACK)).unwrap();
        });
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.join("unrelated"), b"x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "a write beside the marker is no word"
        );
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

    /// CLAUDE'S OWN NATIVE UPDATER is a push too: it writes the new build
    /// under `versions/` and repoints `~/.local/bin/claude`, and the link's
    /// new target wakes the parked host — no atpkg notice fires for it.
    /// NEGATIVE CONTROLS: a new build written beside the link's directory,
    /// and another file in `~/.local/bin`, wake nobody until the link moves.
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
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(wake.wait(FALLBACK)).unwrap();
        });
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(versions.join("2.1.282"), b"new").unwrap();
        std::fs::write(bin.join("other-tool"), b"x").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "a download and a neighbour are not the new build in place"
        );
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

    /// THE TRIGGER ends a parked wait at once, reporting no activation (the
    /// host's shutdown), instead of the fallback's ten minutes: inside [`HANG`],
    /// which the fallback is ten times. NEGATIVE CONTROL: unpulled, the same
    /// wait runs out its fallback.
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
        let mut quiet = ActivationWake::with_marks(Vec::new());
        let started = Instant::now();
        assert!(!quiet.wait(Duration::from_millis(80)));
        assert!(
            started.elapsed() >= Duration::from_millis(80),
            "the fallback"
        );
    }

    /// A FIRST INSTALL (m3, 2026-09-24): the package prefix's parent does not
    /// exist yet, so the watch sits on the nearest ancestor that does, and
    /// the marker atpkg then creates, nested dirs and all, wakes the host
    /// before its fallback.
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

    /// A LOST ANCESTOR WAKE (m3, 2026-09-24): with every vnode event lost
    /// (the fault injected), a marker created under a missing directory is
    /// still seen by the [`ANCESTOR_RECHECK`] look, long before the fallback:
    /// inside [`HANG`], which the [`FALLBACK`] is twice, so a recheck that
    /// waited for the fallback is told from one that did not. NEGATIVE
    /// CONTROL: the same mark not rechecked waits the fallback out.
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
        let mut control = ActivationWake::with_marks(vec![Mark::new(fixed(root.join("x/y/z")))]);
        control.deaf = true;
        assert!(!control.wait(Duration::from_millis(300)), "the control");
        let _ = std::fs::remove_dir_all(root);
    }

    /// AN AGENT'S EXIT IS A PUSH: [`wait_exit`] returns as the process ends,
    /// long before its limit — inside [`HANG`], which the [`FALLBACK`] limit
    /// is twice — and at once for one already gone. NEGATIVE CONTROL: a
    /// process that keeps running is waited for until the limit, and reads as
    /// not exited.
    #[test]
    fn an_exit_is_seen_as_it_happens() {
        let mut child = std::process::Command::new("/bin/sleep")
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
        let mut runs = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let started = Instant::now();
        assert!(!wait_exit(runs.id(), Duration::from_millis(200)));
        assert!(started.elapsed() >= Duration::from_millis(200));
        runs.kill().unwrap();
        runs.wait().unwrap();
    }
    /// THE PHILOSOPHY REVIEW OF 2026-09-25 (minor): on a machine whose
    /// package prefix exists but where no atpkg pass has made the notice
    /// directory, the wake watched an ancestor and looked again every
    /// ANCESTOR_RECHECK (250 ms) for the host's whole life. The host makes
    /// that directory where the prefix exists, so the watch sits on it and
    /// no recheck runs. NEGATIVE CONTROL: with no prefix at all nothing is
    /// made (the first install's own), and the mark still rechecks.
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
        let marker = marker_under(&layout);
        assert!(marker.parent().is_some_and(Path::is_dir), "{marker:?}");
        let kq = kqueue().expect("kqueue");
        let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
        mark.ensure_watch(&kq);
        assert!(
            !mark.watching_an_ancestor(),
            "the watch is on the dir itself"
        );
        // NEGATIVE CONTROL: no prefix, nothing made, and the recheck stays.
        let absent = atpkg::store::Layout {
            prefix: root.join("none"),
        };
        let marker = marker_under(&absent);
        assert!(!root.join("none").exists());
        let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
        mark.ensure_watch(&kq);
        assert!(mark.watching_an_ancestor());
        let _ = std::fs::remove_dir_all(&root);
    }
}
