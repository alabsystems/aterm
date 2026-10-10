// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The pushes that tell the window's host a newer Claude Code or Codex is
//! installed, and the one that says an agent has exited.
//!
//! A newer build arrives two ways, each watched: atpkg atomically replaces a
//! marker after a pass repoints `agents/claude` or `agents/codex`
//! (`atpkg::activation_notice`), and Claude's native updater repoints
//! `~/.local/bin/claude` into `~/.local/share/claude/versions`. The owner's
//! word (`aterm harness upgrade <sid> --now|--defer|--skip`) replaces a marker
//! of its own ([`super::upgrade_drive::word_marker`],
//! [`ActivationWake::with_word`]), so the tab's worker takes it at its next
//! idle point.
//!
//! On macOS one kqueue watches the directory holding each, and a change wakes
//! only when the thing itself moved (the marker's identity, the link's target).
//! The wake is a hint: the host re-reads the builds before acting
//! ([`super::upgrade_drive::due`]). A user event on the same kqueue wakes the
//! parked thread at shutdown ([`WakeTrigger`]).
//!
//! And the OWNER'S WORD on an upgrade (`aterm harness upgrade <sid>
//! --now|--defer|--skip`, from another process) replaces a marker of its own
//! ([`super::upgrade_drive::word_marker`], [`ActivationWake::with_word`]): the
//! same wake, so the tab's worker takes the word at its next idle point.
//!
//! One more marker rides the same watch for a LISTENER OF ITS OWN
//! ([`ActivationWake::with_side`]): the accounts store's wake marker
//! (`super::accounts::Store::wake_path`), replaced when what a session
//! waiting at a usage limit should do has changed. It is no newer build — a
//! change of it is told to its listener and wakes no [`ActivationWake::wait`].
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
//! it (a lost ancestor wake once held a first install until the
//! fallback). Off macOS there is no directory watch, and the fallback IS the
//! wake: a KNOWN POLL, every fallback period, cut short by the trigger. A
//! side marker is looked at on that poll and told ONLY WHEN IT IS ANOTHER FILE
//! than at the last look ([`SidePoll`]): a machine that keeps no such marker
//! — one that lists no account — hears nothing from it, ever.
//!
//! [`wait_exit`] is the push for an agent's end: `EVFILT_PROC`/`NOTE_EXIT` on
//! its pid.

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
    /// The side marks ([`Self::with_side`]), each looked at on every fallback
    /// — the only wake there is off macOS — and told only when it changed.
    #[cfg(not(target_os = "macos"))]
    sides: Vec<SidePoll>,
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
                sides: Vec::new(),
            }
        }
    }

    /// Also wake on the owner's word ([`super::upgrade_drive::word_marker`]
    /// under the harness state `path` names): its directory is made here, so
    /// the watch sits on it, never on an ancestor. Off macOS the fallback is
    /// the only wake, for the word as for a build.
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

    /// Also watch `path` FOR A LISTENER OF ITS OWN: when the file there is
    /// replaced (or first made), `told` is called on the waiting thread and
    /// the wait GOES ON — it is no newer build and no word on an upgrade, so
    /// [`Self::wait`] never answers `true` for it. The accounts store's wake
    /// marker is the one such mark (module header).
    ///
    /// ITS DIRECTORY IS NOT MADE HERE, unlike the word's: the accounts store
    /// exists only once the owner has listed an account, and a machine that
    /// lists none keeps nothing of it on disk. Until the directory exists the
    /// watch sits on the nearest ancestor that does, and moves inward as it
    /// is made (the pass that follows the making reads the marker again, so
    /// one written in between is seen); the marker as it stands when the
    /// watch is armed is the baseline, and only a later change is told.
    ///
    /// Off macOS there is no directory watch: the marker is LOOKED AT at
    /// every fallback, a KNOWN POLL, as the builds are looked at there, and
    /// `told` is called only where it is another file than at the last look
    /// ([`SidePoll`]) — never for a marker that is not there and never was,
    /// so a machine with no accounts store hears nothing on any platform.
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

    /// Park until a newer build may be installed (`true`: atpkg's marker
    /// was replaced or moved with the package prefix, or the native link was
    /// repointed), else until `fallback` passes or the [`WakeTrigger`] is
    /// pulled (`false`). A failed watch waits out the same bounded fallback.
    /// Off macOS the fallback is the wake (`true` after it) and the trigger
    /// cuts it short (`false`).
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
            // Read BEFORE registration, register, then read AGAIN: a change
            // in the gap is either seen here or queued by kqueue. A wake
            // never depends on timing that gap.
            if self.any_changed() {
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

#[cfg(target_os = "macos")]
impl ActivationWake {
    /// Whether a mark that IS a wake changed since it was last seen. Every
    /// mark is looked at (each is then seen); a SIDE mark that changed is
    /// told to its own listener ([`Self::with_side`]) and is no wake here.
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

/// WHETHER A WATCHED THING IS ANOTHER THAN WHEN IT WAS LAST SEEN, `next`
/// being what it is now (`None`: not there) — and it is then seen as `next`.
/// The one rule of every mark on every platform: the kqueue's marks compare
/// their file identity by it, the fallback's poll of a side marker
/// ([`SidePoll`]) its stamp. A thing that is not there and was not there has
/// not changed, however often it is asked. PURE but for `seen`.
fn moved<T: PartialEq>(seen: &mut Option<T>, next: Option<T>) -> bool {
    if next == *seen {
        return false;
    }
    *seen = next;
    true
}

/// What a side marker IS to the fallback's poll ([`SidePoll`]): its inode
/// (unix; `0` elsewhere), its length and when it was last modified. The
/// marker is replaced by a rename of a new file, so a replacement is another
/// inode — and another modification time where there is none to read.
#[cfg(any(test, not(target_os = "macos")))]
type SideStamp = (u64, u64, Option<std::time::SystemTime>);

/// [`SideStamp`] of the file at `path`; `None` where no file is there.
#[cfg(any(test, not(target_os = "macos")))]
fn side_stamp(path: &Path) -> Option<SideStamp> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() {
        return None;
    }
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt as _;
        m.ino()
    };
    #[cfg(not(unix))]
    let ino = 0;
    Some((ino, m.len(), m.modified().ok()))
}

/// A SIDE MARK AS THE FALLBACK'S POLL KEEPS IT ([`ActivationWake::with_side`]
/// off macOS, where no directory is watched): where the marker lives, what it
/// was at the last look, and its listener. The marker as it stands when the
/// poll is made is the baseline, as it is for a watched mark: only a LATER
/// change is told. Until this was kept the listener was told at every
/// fallback whatever the marker did, so a session waiting at a usage limit
/// was decided again every ten minutes on a machine with no roster at all.
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
    /// last look ([`moved`]), and whether it was.
    fn look(&mut self) -> bool {
        let next = self.path.as_deref().and_then(side_stamp);
        let changed = moved(&mut self.seen, next);
        if changed {
            (self.told)();
        }
        changed
    }
}

/// Wait at most `limit` for `pid` to exit: `true` once it has (at once when
/// it already has — a zombie its parent has not reaped yet included), `false`
/// when it still runs at the end. A push on macOS (`EVFILT_PROC`/`NOTE_EXIT`);
/// elsewhere `kill(pid, 0)` every 250 ms.
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
            // ESRCH: nothing by that pid the kernel will watch — it has
            // exited, reaped or NOT: a zombie (ended, its parent yet to reap
            // it) is refused too, while `kill(pid, 0)` still finds it. Asked
            // of `gone` alone, a zombie read as still running at once, with
            // no wait: the restart's escalation took a program that heard
            // its SIGTERM for one that outlived the deadline, and the
            // upgrade's relaunch said `wait:exiting` for an agent that had
            // ended.
            let esrch = std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            return esrch || gone();
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
    /// A SIDE mark's listener ([`ActivationWake::with_side`]): a change of
    /// it is told here and ends no wait. `None`: the mark is a wake.
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
        moved(&mut self.seen, next)
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

/// atpkg's marker under the configured package prefix, its notice directory
/// made where the prefix exists but no pass has made it yet
/// (`atpkg::activation_notice::prepare`), so the watch sits on that directory
/// and the [`ANCESTOR_RECHECK`] timer never runs there. Only a machine with no
/// prefix at all keeps the recheck, which its first install ends.
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

/// What a thing is: the file its path resolves to (through a link) and that
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
        // Negative control: a wait that only runs out its fallback reports
        // no activation, so the host does not look for nothing.
        let mut quiet = ActivationWake::with_marks(vec![Mark::new(fixed(marker.clone()))]);
        assert!(!quiet.wait(Duration::from_millis(50)));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The owner's word is a push too ([`ActivationWake::with_word`]): the
    /// marker `aterm harness upgrade <sid> --now|--defer|--skip` replaces
    /// after writing the word wakes the parked host, and its directory is
    /// made up front so the watch sits on it. Negative control: a write beside
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

    /// A SIDE MARK ([`ActivationWake::with_side`], the accounts store's wake
    /// marker) is told to its own listener and is NO wake: replaced, its
    /// listener hears it once and the wait answers nothing for it; the same
    /// replacement of a mark that IS a wake answers `true` and tells the
    /// listener nothing. Its directory is NOT made for the watch (a machine
    /// that lists no account keeps nothing of the store), and a PARKED wait,
    /// its watch on the nearest ancestor, hears the marker made under a
    /// directory made after it parked — and waits on: the word after it is
    /// what that wait answers `true` for. NEGATIVE CONTROLS: nothing changed
    /// tells nobody, and neither does a write beside the marker. Every wait
    /// here is a hang detector or no wait at all (a zero fallback).
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
        // NEGATIVE CONTROL: nothing changed.
        assert!(!wake.wait(now));
        assert_eq!(heard(), 0);
        // The marker made, then replaced: told each time, and no wake.
        std::fs::create_dir_all(&store).unwrap();
        replace(&marker);
        assert!(!wake.wait(now), "a side mark ends no wait");
        assert_eq!(heard(), 1);
        replace(&marker);
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2);
        // NEGATIVE CONTROLS: a write beside it; and a mark that IS a wake.
        std::fs::write(store.join("state.json"), b"{}").unwrap();
        assert!(!wake.wait(now));
        assert_eq!(heard(), 2, "a write beside the marker is not it");
        replace(&word);
        assert!(wake.wait(now), "the word is a wake");
        assert_eq!(heard(), 2, "and no news of the side mark");

        // PARKED, with no store yet: the watch sits on an ancestor.
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
        // The wait went on: what it answers is the word that comes after.
        replace(&word);
        assert_eq!(
            done_rx.recv_timeout(HANG).ok(),
            Some(true),
            "the side mark ended no wait: the word did"
        );
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// CLAUDE'S OWN NATIVE UPDATER is a push too: it writes the new build
    /// under `versions/` and repoints `~/.local/bin/claude`, and the link's
    /// new target wakes the parked host. Negative controls: a new build written
    /// beside the link's directory, and another file in `~/.local/bin`, wake
    /// nobody until the link moves.
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

    /// The trigger ends a parked wait at once, reporting no activation (the
    /// host's shutdown), inside [`HANG`], which the fallback is ten times.
    /// Negative control: unpulled, the same wait runs out its fallback.
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

    /// A first install: the package prefix's parent does not exist yet, so the
    /// watch sits on the nearest ancestor that does, and the marker atpkg then
    /// creates, nested dirs and all, wakes the host before its fallback.
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
    /// a marker created under a missing directory is still seen by the
    /// [`ANCESTOR_RECHECK`] look inside [`HANG`], which the [`FALLBACK`] is
    /// twice. Negative control: the same mark not rechecked waits the fallback
    /// out.
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

    /// An agent's exit is a push: [`wait_exit`] returns as the process ends,
    /// inside [`HANG`], which the [`FALLBACK`] limit is twice, and at once for
    /// one already gone. Negative control: a process that keeps running is
    /// waited for until the limit, and reads as not exited.
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
        let started = Instant::now();
        assert!(!wait_exit(runs.id(), Duration::from_millis(200)));
        assert!(started.elapsed() >= Duration::from_millis(200));
        runs.kill().unwrap();
        runs.wait().unwrap();
    }

    /// A ZOMBIE HAS EXITED: a process that ended and that its parent has not
    /// reaped yet (an agent's shell slow to reap it) is refused by
    /// `EVFILT_PROC` (`ESRCH`) while `kill(pid, 0)` still finds it, and
    /// [`wait_exit`] says it exited, at once. FAILS WITHOUT IT: read by
    /// `kill(pid, 0)` alone it still ran, and no wait was made at all — the
    /// restart's escalation said "still running at the act's deadline" of a
    /// program that had heard its SIGTERM (3 of 30 runs of aterm-gui's
    /// `-- restart` six at a time on a loaded machine).
    #[test]
    fn a_zombie_has_exited() {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .unwrap();
        let pid = child.id();
        // Until it has exited, left unreaped (`WNOWAIT`): a zombie.
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
    /// directory, the host makes it, so the watch sits on it and no recheck
    /// runs. Negative control: with no prefix at all nothing is made, and the
    /// mark still rechecks.
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
        // Negative control: no prefix, nothing made, and the recheck stays.
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

/// The fallback's poll of a side marker ([`SidePoll`]), on EVERY platform:
/// off macOS it is the only thing that tells a side mark's listener.
#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A marker replaced as the accounts store replaces its own: a new file
    /// renamed over it.
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

    /// THE FALLBACK'S TICK TELLS A SIDE MARK'S LISTENER ONLY WHEN THE MARKER
    /// IS ANOTHER FILE (the no-roster invariant, off macOS): a marker that
    /// is not there — no store, no directory, no path at all — is told at
    /// no tick, however many pass; made, it is told once; left alone, at no
    /// later tick; replaced, once more; removed, once more (it changed).
    /// NEGATIVE CONTROLS: a write BESIDE the marker tells nobody, and a
    /// marker already there when the poll is made is the baseline, never
    /// news. Before this the listener was told at every tick.
    #[test]
    fn a_fallback_tick_tells_a_side_listener_only_when_its_marker_changed() {
        let root =
            std::env::temp_dir().join(format!("aterm-accounts-side-poll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = root.join("accounts");
        let marker = store.join("wake");

        // No roster: no store, no marker — and no path at all.
        let (mut none, heard_none) = poll(None);
        let (mut side, heard) = poll(Some(marker.clone()));
        for _ in 0..5 {
            assert!(!side.look() && !none.look());
        }
        assert_eq!((heard(), heard_none()), (0, 0), "nothing there: no news");

        // The marker made, then left alone, then replaced.
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
        // NEGATIVE CONTROL: a write beside the marker.
        std::fs::write(store.join("state.json"), b"{}").unwrap();
        assert!(!side.look());
        assert_eq!(heard(), 2, "a write beside the marker is not it");
        // NEGATIVE CONTROL: a marker that stands when the poll is made.
        let (mut late, heard_late) = poll(Some(marker.clone()));
        assert!(!late.look());
        assert_eq!(heard_late(), 0, "the baseline is no news");
        // Removed: it changed, once.
        std::fs::remove_file(&marker).unwrap();
        assert!(side.look() && !side.look());
        assert_eq!(heard(), 3);
        let _ = std::fs::remove_dir_all(root);
    }

    /// [`moved`], the one rule: nothing and nothing is no change; a thing
    /// appearing, becoming another and going are each one change, seen once.
    #[test]
    fn a_thing_has_moved_only_when_it_is_another_than_last_seen() {
        let mut seen: Option<u8> = None;
        assert!(!moved(&mut seen, None));
        assert!(moved(&mut seen, Some(1)));
        assert!(!moved(&mut seen, Some(1)));
        assert!(moved(&mut seen, Some(2)));
        assert_eq!(seen, Some(2));
        assert!(moved(&mut seen, None));
        assert!(!moved(&mut seen, None));
    }
}
