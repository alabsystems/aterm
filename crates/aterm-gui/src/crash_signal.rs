// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Async-signal-safe capture of native fatal signals (M6 "CRASH-CORE").
//!
//! The panic hook in [`crate::logging`] only fires for Rust unwinds. Native
//! fatal signals — `SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, `SIGFPE` — never
//! run the panic machinery: they jump straight to the kernel-installed
//! disposition and the process is gone, with nothing on disk for a windowed
//! app whose stderr nobody reads. This module installs a `sigaction`(2)
//! handler that drops a crash *marker* before the process dies, so an
//! after-the-fact "did aterm take a signal, and which one?" question has an
//! artifact to point at.
//!
//! ASYNC-SIGNAL-SAFETY (the whole reason this is its own module):
//! a signal handler may interrupt the program at *any* instruction, including
//! the middle of `malloc`, a `Mutex` critical section, or libc's own buffers.
//! It may therefore call ONLY async-signal-safe functions (POSIX
//! `signal-safety(7)`): here that is `write(2)`, `sigaction(2)`, and
//! `raise(3)`. Everything that is NOT signal-safe — resolving the marker path,
//! `open`ing it, allocating, `format!` — is done ONCE AT INSTALL TIME and the
//! results are parked in `static`s. The handler itself:
//!   * does no allocation (the integer is formatted into a stack buffer with a
//!     hand-rolled, allocation-free decimal helper),
//!   * takes no lock,
//!   * calls no `format!`/`String`/`println!`,
//!   * writes a pre-built banner + the signal number to `STDERR_FILENO` and to
//!     a pre-opened marker fd, then
//!   * restores the signal's default disposition and `raise()`s it, so the
//!     process still core-dumps / exits with the conventional status.

/// Arm async-signal-safe fatal-signal capture (`SIGSEGV`/`SIGABRT`/`SIGBUS`/
/// `SIGILL`/`SIGFPE`). `arming` says what kind of start this is, which decides
/// whether this process's marker, left EMPTY by a death that ran no exit path, is
/// news for the next windowed launch ([`Arming::for_launch`]; the Windows lane
/// reports crashes only and ignores it — see [`MarkerOwner`]). Call alongside the panic
/// hook so both crash paths are covered. On Windows the analogue is an
/// unhandled-exception filter
/// (`SetUnhandledExceptionFilter`) writing a crash artifact under the same
/// discipline (everything non-trivial at install time; the filter itself
/// allocates nothing). No-op on targets with neither lane so the call site
/// stays portable.
///
/// COVERAGE IS NOT SYMMETRIC across platforms, and deliberately so. The unix
/// lane traps `SIGABRT`, which is where Rust's abort paths land — allocation
/// failure (`handle_alloc_error`), a panic-while-panicking, and
/// `std::process::abort` all funnel to `abort()` → `SIGABRT`, so those get a
/// marker. On Windows, Rust's `abort_internal` is `int 0x29` (`__fastfail`
/// with `FAST_FAIL_FATAL_APP_EXIT`), which terminates the process WITHOUT
/// running any in-process exception handler — the SUEF filter (and VEH) never
/// see it, so an OOM abort or double panic leaves no marker from *this* lane.
/// The double-panic case is still partly covered: the panic hook already wrote
/// `crash-<pid>.log` for the first panic. The only way to capture a bare
/// `__fastfail` on Windows is out-of-process, via WER LocalDumps
/// (`HKCU\Software\Microsoft\Windows\Windows Error Reporting\LocalDumps\<exe>`),
/// which is an install-time/deployment concern, not something this lane can arm.
pub(crate) fn install_signal_handlers_as(arming: Arming) {
    #[cfg(unix)]
    imp::install_signal_handlers(arming);
    #[cfg(not(unix))]
    let _ = arming;
    #[cfg(windows)]
    imp_windows::install();
}

/// WHOSE MARKER THIS IS — which decides whether its empty corpse means anything.
///
/// A marker is created empty at every start and written only by the fatal-signal
/// handler, so an empty marker whose owner is DEAD means that owner ended without
/// a signal and without an exit path: SIGKILL (Force Quit, a harness's cleanup),
/// jetsam, a watchdog, power loss. That is worth one row at the next launch —
/// for the daily driver. It is not worth one for the test, tool, dev-bundle and
/// headless starts that share the same log dir and are SIGKILLed by harnesses as
/// a matter of course (measured 2026-09-24 on the owner's Mac: 13 non-bundle GUI
/// starts in one day's `aterm.log`, `does not run from a .app bundle`); reporting
/// those would bury the one true report under false ones. So the name says which
/// it is, and only [`MarkerOwner::App`] is ever reported. Unix only: the Windows
/// lane keeps its `crash-signal-*` markers — held against deletion by the
/// owner's open handle rather than an owner-named lock — and reports crashes
/// alone.
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "the owner lives in the unix marker name only")
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerOwner {
    /// The installed, updater-owned `.app` bundle, running WINDOWED, and — when it
    /// arrived through an update handoff — past its Commit: the daily driver.
    App,
    /// Everything else: a test binary, a `target/` or store binary, a dev-marked
    /// bundle, a DMG or translocated launch, a `--headless` start, and a handoff
    /// candidate its parent may still reject (and reap with SIGKILL).
    Other,
}

impl MarkerOwner {
    /// The token in the marker's file name.
    #[cfg(unix)]
    const fn tag(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Other => "other",
        }
    }
}

/// How a launch arms its marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arming {
    /// As [`MarkerOwner::App`] from the start: a cold windowed launch of the
    /// installed bundle.
    App,
    /// As [`MarkerOwner::Other`], for good.
    Other,
    /// As [`MarkerOwner::Other`] until [`adopt_app_identity`] — an installed,
    /// windowed handoff CANDIDATE. Its parent rejects a candidate by SIGKILLing
    /// its process group (`main_entry`'s final-exit comment), which is no news;
    /// the same process after its Commit IS the daily driver, and after a
    /// machine's first update every daily driver is one.
    AppAfterCommit,
}

impl Arming {
    /// The arming of a launch: `installed_bundle` is the updater's own
    /// classification of the running copy (`which_copy::Running::InstalledApp` —
    /// an owned `.app`, not dev-marked, not on a disk image, not translocated),
    /// `headless` whether the launch arms headless mode, `handoff_candidate`
    /// whether it arrived through a validated update handoff.
    #[must_use]
    pub(crate) const fn for_launch(
        installed_bundle: bool,
        headless: bool,
        handoff_candidate: bool,
    ) -> Self {
        if !installed_bundle || headless {
            Self::Other
        } else if handoff_candidate {
            Self::AppAfterCommit
        } else {
            Self::App
        }
    }

    /// The owner the marker is CREATED under.
    #[cfg(unix)]
    const fn initial_owner(self) -> MarkerOwner {
        match self {
            Self::App => MarkerOwner::App,
            Self::Other | Self::AppAfterCommit => MarkerOwner::Other,
        }
    }
}

/// This process is about to end cleanly: remove its own marker, so the empty
/// corpse of a process that did NOT end cleanly stays meaningful.
///
/// ASYNC-SIGNAL-SAFE (one `getpid`, one atomic swap, one `unlink(2)` of a path
/// built at install time), because the exits that need it are the `_exit` ones:
/// the handoff parent after its Commit, a refused candidate, a launch-fatal —
/// see [`clean_exit_now`]. Every `exit(3)` (`std::process::exit`, a return from
/// `main`, AppKit's terminate) reaches it through the `atexit` handler the unix
/// install registers. A no-op in any process but the one that armed the marker
/// (a forked child that calls `exit` must not remove its parent's), and after
/// the first call.
#[cfg(unix)]
pub(crate) fn release_own_marker() {
    imp::release_own_marker();
}

/// The start this process's crash marker is named with
/// (`crash-marker-<pid>-<nanos>-<owner>.log`), once the install armed one;
/// `None` when it did not. The crash journal names this run's journal by it
/// (`crate::crash_journal::JournalId::of_this_run`), so a later claim can find
/// exactly this run's marker, under its consumed `.seen` name too.
#[cfg(unix)]
#[must_use]
pub(crate) fn own_marker_nanos() -> Option<u128> {
    imp::own_marker_nanos()
}

/// Where this process's crash marker is — its directory and the start it is
/// named with — while it is still linked (`None` before the install armed one,
/// and after the clean exit released it). The PTY keeper is told this at HELLO
/// (`keeper_link`), and after this window's death reads the marker's absence
/// as its exit path having run (§5.4 row 3's cross-check).
#[cfg(unix)]
#[must_use]
pub(crate) fn own_marker_place() -> Option<(std::path::PathBuf, u128)> {
    imp::own_marker_place()
}

/// The handoff candidate this process was is now the daily driver — its Commit
/// arrived: re-name its marker as [`MarkerOwner::App`]. A no-op unless the launch
/// armed [`Arming::AppAfterCommit`], and after the first call. `true` exactly when
/// this call is that promotion — the moment the recovery census records the
/// successor (`recovery_census`), once.
#[cfg(unix)]
pub(crate) fn adopt_app_identity() -> bool {
    imp::adopt_app_identity()
}

/// Leave the process with `code` NOW, through `_exit(2)`, after removing this
/// process's marker ([`release_own_marker`]). THE spelling of a CLEAN `_exit` in
/// this crate: a bare `libc::_exit` on a path that is not a crash leaves an empty
/// marker behind, which the next windowed launch of the installed app reports as
/// "aterm was killed" (`crash_signal::exit_gate` holds the line).
#[cfg(unix)]
pub(crate) fn clean_exit_now(code: i32) -> ! {
    release_own_marker();
    // SAFETY: async-signal-safe immediate exit of the calling process. It ends
    // this process and nothing else, runs no handler and no destructor, and
    // cannot return.
    unsafe { libc::_exit(code) }
}

/// Release THIS launch's crash marker at a CLEAN exit — the WINDOWS half of
/// the clean-exit removal: close the pre-opened handle and delete the file if
/// it is still empty. A clean run never writes its marker, so without this
/// every clean exit left a zero-byte `crash-signal-<pid>-<nanos>.log` for the
/// NEXT launch's `sweep_stale_markers` to pay for (2026-09-22 audit: the
/// marker of every cleanly closed Windows instance survived its exit). That
/// sweep stays as the backstop for every exit that is not this one — a kill, a
/// crash — and a marker with bytes in it is a crash record and is kept
/// whatever path reaches this.
///
/// Called from the graceful-exit seam, after the event loop has returned and
/// the socket cleanup has run, and from the Windows launch-fatal terminator
/// (`exit_without_process_teardown`: a diagnosed exit, not a crash). From the
/// moment the handle is swapped out a fault still writes its banner to stderr,
/// just not to a file; that window is the last few statements before the
/// `process::exit`. No-op when no marker was opened, and after the first call.
///
/// A NO-OP ON UNIX, on purpose: that lane already removes its own marker at
/// every clean exit — `release_own_marker`, from the `atexit` handler its
/// install registers and from `clean_exit_now` — and the `atexit` one runs
/// LAST, after every static destructor, so a crash during `exit` still lands
/// in a linked file. Removing it earlier from here would give that window up.
pub(crate) fn remove_marker_on_clean_exit() {
    #[cfg(windows)]
    imp_windows::release_marker();
}

/// Launch-unique marker path: `crash-signal-<pid>-<nanos>.log`. Windows recycles
/// PIDs aggressively, so keying on PID alone lets a later launch that inherits a
/// crashed instance's PID truncate that instance's genuine marker at startup.
/// Salting with the startup timestamp keeps a recycled PID from clobbering a
/// prior crash's artifact. Called AT INSTALL TIME — allocation is fine here.
#[cfg(windows)]
fn marker_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!(
        "crash-signal-{}-{}.log",
        std::process::id(),
        startup_nanos()
    ))
}

/// The one rule shared by the Windows clean-exit release and the next-launch
/// sweep: a marker is a clean-run leftover — removable — exactly when it is
/// empty. Any byte in it is a crash record. (Plain `std`, so its tests run on
/// every host.)
#[cfg(any(windows, test))]
fn marker_is_clean_leftover(len: u64) -> bool {
    len == 0
}

/// Delete the marker at `path` if it is a regular file with nothing in it.
/// `true` when it was removed. `symlink_metadata`, so a planted link is judged
/// (and at most unlinked) as the link it is, never followed to its target.
#[cfg(any(windows, test))]
fn remove_marker_if_empty(path: &std::path::Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && marker_is_clean_leftover(meta.len()) => {
            std::fs::remove_file(path).is_ok()
        }
        _ => false,
    }
}

/// Nanoseconds since the epoch — the launch salt in a marker's name.
#[cfg(any(unix, windows))]
fn startup_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Delete zero-length `crash-signal-*.log` files left in `dir` (Windows). Every
/// launch creates an empty marker and a clean run never writes to it; a clean
/// EXIT removes its own ([`remove_marker_on_clean_exit`]), but a kill or a
/// crash that wrote nothing cannot, so without this sweep those accumulate one
/// stale file per such exit, unbounded. Only *empty* markers are removed — a
/// non-empty one is a real crash record we must preserve
/// ([`marker_is_clean_leftover`]).
///
/// A LIVE instance's marker is never swept, and the kernel is what says so: its
/// owner holds it open without `FILE_SHARE_DELETE` (`imp_windows::
/// open_marker_file`), so the `remove_file` below fails with a sharing
/// violation for as long as the owner lives, however it then dies. This is the
/// Windows form of the unix lane's owner lock (`markers`), and it is needed for
/// the same reason. Measured 2026-09-27 on Windows 11 (26200): the daily driver,
/// pid 16916 and up five days on 0.90.0, had no `crash-signal-16916-*.log` left
/// in the log dir — a later launch's sweep had deleted it — and `DeleteFileW`
/// on this Windows removes the NAME at once even while a handle is open (the
/// file lives on unnamed until that handle closes). A fault would have written
/// its banner into a file nothing could find.
#[cfg(windows)]
fn sweep_stale_markers(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("crash-signal-")
            && name.ends_with(".log")
            && entry
                .metadata()
                .map(|m| marker_is_clean_leftover(m.len()))
                .unwrap_or(false)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// THE UNIX MARKER'S LIFECYCLE: named for its owner, LOCKED for its owner's
/// life, removed by its owner's clean exit — so another aterm start can tell a
/// live marker from a dead one, and a dead EMPTY one from a clean run.
///
/// WHY (measured 2026-09-24 on the owner's Mac, 0.93.0): the sweep used to
/// delete EVERY empty `crash-signal-*.log`, owner alive or not. `lsof -p 7641`
/// showed the daily driver's fd 3 on `crash-signal-7641-….log` while `ls` said
/// the file no longer existed: a GUI-mode start with the real `$HOME` (pid
/// 87172) had swept it 20 ms after arming its own. From then on a SIGSEGV of the
/// daily driver would have written its banner into an unlinked inode — no
/// artifact, no report at the next launch. And because a clean run never removed
/// its own marker, a SIGKILL was indistinguishable from a clean quit.
///
/// THE RULES:
/// * the name is `crash-marker-<pid>-<nanos>-<owner>.log` (`MarkerOwner::tag`).
///   A NEW prefix on purpose: an older build's sweep deletes `crash-signal-*`
///   only, so a build that predates this module can no longer unlink the live
///   daily driver's marker either; its crash scan reads every non-empty
///   `crash-*.log`, so a real crash record is still reported by an older launch.
/// * the owner holds `flock(LOCK_EX)` on it for its whole life. It is created
///   under a `.pending` name, locked, THEN renamed into place, so no sweep ever
///   sees an unlocked live marker; the kernel drops the lock when the owner dies,
///   however it dies.
/// * a sweep removes an empty marker only when it can take that lock (owner
///   gone), and leaves a dead [`super::MarkerOwner::App`] one for the windowed
///   launch that reports it (`logging::take_kill_evidence`). A dead marker named
///   with THIS process's pid was this process's previous image — the updater's
///   boot re-exec keeps the pid and closes the close-on-exec fd, releasing the
///   lock — so it is removed, never reported.
/// * a legacy empty `crash-signal-<pid>-*.log` is removed only when its pid is
///   gone: an older build still running keeps its marker.
#[cfg(unix)]
pub(crate) mod markers {
    use std::path::{Path, PathBuf};

    use super::MarkerOwner;

    /// Prefix of this module's markers.
    pub(crate) const PREFIX: &str = "crash-marker-";
    /// Prefix of the markers builds before this module armed.
    const LEGACY_PREFIX: &str = "crash-signal-";
    /// Suffix of a marker still being armed (see [`create_locked`]).
    const PENDING_SUFFIX: &str = ".pending";

    /// A marker file name, parsed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct MarkerName {
        pub(crate) pid: u32,
        /// The owner's start, nanoseconds since the epoch: with the pid, what
        /// tells this run from an earlier one that had the same pid (the crash
        /// journal reads it, `crate::crash_journal::classify_death`).
        pub(crate) nanos: u128,
        pub(crate) owner: MarkerOwner,
    }

    /// `crash-marker-<pid>-<nanos>-<owner>.log`.
    #[must_use]
    pub(crate) fn file_name(pid: u32, nanos: u128, owner: MarkerOwner) -> String {
        format!("{PREFIX}{pid}-{nanos}-{}.log", owner.tag())
    }

    /// Parse a [`file_name`]; `None` for anything else (a legacy marker, a
    /// consumed `.seen` record, a pending one, a stranger).
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<MarkerName> {
        let body = name.strip_prefix(PREFIX)?.strip_suffix(".log")?;
        let mut parts = body.splitn(3, '-');
        let pid = parts.next()?.parse::<u32>().ok()?;
        let nanos = parts.next()?;
        if nanos.is_empty() || !nanos.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let nanos = nanos.parse::<u128>().ok()?;
        let owner = match parts.next()? {
            "app" => MarkerOwner::App,
            "other" => MarkerOwner::Other,
            _ => return None,
        };
        Some(MarkerName { pid, nanos, owner })
    }

    /// The pid of a legacy `crash-signal-<pid>-<nanos>.log`.
    fn legacy_pid(name: &str) -> Option<u32> {
        let body = name.strip_prefix(LEGACY_PREFIX)?.strip_suffix(".log")?;
        body.split('-').next()?.parse().ok()
    }

    /// What trying a marker's owner lock says about its owner.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Owner {
        /// Someone holds it: the owner is alive.
        Live,
        /// The lock was free: the owner is gone.
        Dead,
        /// The file could not be opened or locked for another reason — no
        /// evidence either way, so nothing is removed or reported.
        Unknown,
    }

    /// Probe `path`'s owner lock without waiting: `open` it read-only, NOT
    /// following a symlink and NOT blocking on a FIFO (a file in the log dir is
    /// not necessarily a file an aterm wrote — see the urandom incident), then
    /// `flock(LOCK_EX|LOCK_NB)`, then close. `flock` binds to the open file
    /// DESCRIPTION, so this conflicts even with a lock this same process holds
    /// through another open of the file.
    #[must_use]
    pub(crate) fn probe(path: &Path) -> Owner {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Owner::Unknown;
        };
        // SAFETY: `c_path` is a live NUL-terminated string for the call.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Owner::Unknown;
        }
        // SAFETY: `fd` was opened above and is closed below; LOCK_NB never waits.
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        let err = std::io::Error::last_os_error().raw_os_error();
        // A lock the probe took is released by LOCK_UN, not by the close alone:
        // `flock` binds to the open file description, and a child this process
        // forks inside the probe's microseconds (a shell, in the app) holds a copy
        // of it until it execs — the next probe of the same dead marker would read
        // it as LIVE. LOCK_UN strips the lock from the description, copies and
        // all (the fd-copy sweep of 2026-09-27; the sweep lock's 773b7136f).
        if locked {
            // SAFETY: `fd` is the descriptor opened above; LOCK_UN never waits.
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
        // SAFETY: closes the descriptor opened above exactly once.
        unsafe { libc::close(fd) };
        if locked {
            Owner::Dead
        } else if err == Some(libc::EWOULDBLOCK) {
            Owner::Live
        } else {
            Owner::Unknown
        }
    }

    /// Whether `pid` names a live process (`kill(pid, 0)`; `EPERM` is alive —
    /// it exists, it is just not ours).
    fn pid_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 performs the existence and permission checks only.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// An empty REGULAR file (not a symlink, FIFO or directory) — the only
    /// shape a clean-run marker has.
    fn empty_regular_file(entry: &std::fs::DirEntry) -> bool {
        // `DirEntry::metadata` does not traverse a symlink on unix.
        entry
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() == 0)
    }

    /// Remove the markers in `dir` whose owners are gone and whose emptiness is
    /// not news (the rules on this module). `own_pid` is the sweeping process's
    /// pid. Called AT INSTALL TIME, before this process's own marker exists.
    ///
    /// Answers the dead, empty markers of OTHER processes it removed: each is a
    /// run that ended with no signal and no exit path, which is no row for a
    /// test or dev start but is still the evidence the crash journal needs to
    /// reopen that run's layout (`crate::crash_journal::classify_death`). The
    /// install keeps the answer ([`swept_at_install`]); this process's own
    /// previous image is not in it — a boot re-exec is not a death.
    pub(crate) fn sweep(dir: &Path, own_pid: u32) -> Vec<MarkerName> {
        let mut swept = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return swept;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !empty_regular_file(&entry) {
                continue; // a real crash record, or not a marker at all
            }
            let path = entry.path();
            if let Some(pending) = name
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(PENDING_SUFFIX))
            {
                // An arming that died between its create and its rename.
                if parse(pending).is_some() && probe(&path) == Owner::Dead {
                    let _ = std::fs::remove_file(&path);
                }
            } else if let Some(marker) = parse(name) {
                let news = marker.owner == MarkerOwner::App && marker.pid != own_pid;
                if !news
                    && probe(&path) == Owner::Dead
                    && std::fs::remove_file(&path).is_ok()
                    && marker.pid != own_pid
                {
                    swept.push(marker);
                }
            } else if legacy_pid(name).is_some_and(|pid| !pid_alive(pid)) {
                let _ = std::fs::remove_file(&path);
            }
        }
        swept
    }

    /// What this process's install-time [`sweep`] removed, kept for the crash
    /// journal's claim, which runs later in the same launch.
    static SWEPT_AT_INSTALL: std::sync::OnceLock<Vec<MarkerName>> = std::sync::OnceLock::new();

    /// Keep the install-time sweep's answer (the first call wins).
    pub(super) fn keep_swept(swept: Vec<MarkerName>) {
        let _ = SWEPT_AT_INSTALL.set(swept);
    }

    /// The dead, empty markers of other processes this launch's install-time
    /// sweep removed — empty when nothing was swept or the install never ran.
    #[must_use]
    pub(crate) fn swept_at_install() -> &'static [MarkerName] {
        SWEPT_AT_INSTALL.get().map_or(&[], Vec::as_slice)
    }

    /// The dead, empty [`MarkerOwner::App`] markers in `dir` another process
    /// left: evidence that the daily driver ended without a signal and without
    /// a clean exit. `own_pid` excludes this process's own previous image.
    #[must_use]
    pub(crate) fn dead_app_markers(dir: &Path, own_pid: u32) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                name.to_str()
                    .and_then(parse)
                    .is_some_and(|marker| marker.owner == MarkerOwner::App && marker.pid != own_pid)
                    && empty_regular_file(entry)
                    && probe(&entry.path()) == Owner::Dead
            })
            .map(|entry| entry.path())
            .collect()
    }

    /// Create `dir/<name>` `0600`, LOCKED before it is visible under that name:
    /// created exclusively under `.<name>.pending` (which no sweep removes while
    /// it is locked), `flock`ed, then renamed into place. Returns the fd
    /// (close-on-exec, lock held) and the final path, or `None` when any step
    /// fails — the pending file is then removed, and [`arm`] falls back to an
    /// unlocked marker.
    pub(crate) fn create_locked(dir: &Path, name: &str) -> Option<(i32, PathBuf)> {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::io::IntoRawFd as _;
        let path = dir.join(name);
        let pending = dir.join(format!(".{name}{PENDING_SUFFIX}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&pending)
            .ok()?;
        let fd = file.into_raw_fd();
        // SAFETY: `fd` is the descriptor just opened; LOCK_NB never waits, and a
        // file this call created exclusively has no other holder.
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !locked || std::fs::rename(&pending, &path).is_err() {
            let _ = std::fs::remove_file(&pending);
            // SAFETY: closes the descriptor opened above, exactly once.
            unsafe { libc::close(fd) };
            return None;
        }
        Some((fd, path))
    }

    /// This launch's marker, as [`arm`] made it.
    #[derive(Debug)]
    pub(crate) struct Armed {
        /// The open descriptor the fatal-signal handler writes to.
        pub(crate) fd: i32,
        /// Its name in the log dir.
        pub(crate) path: PathBuf,
        /// Whether the owner lock is held — `false` only for the fallback.
        pub(crate) locked: bool,
    }

    /// Arm this launch's marker in `dir`: [`create_locked`] under `owner`'s
    /// name, the lifecycle on this module. FALLBACK — when that cannot be done
    /// (a log dir on a filesystem without `flock`, a rename refused, a stale
    /// pending name in the way), an UNLOCKED marker is created directly under
    /// its final name, so a native crash still leaves the banner on disk: before
    /// the owner lock existed every launch had an unlocked one, and a process
    /// with none at all would lose exactly the artifact this module exists for.
    /// It is always named [`MarkerOwner::Other`], whatever `owner` asked for: an
    /// unlocked marker cannot tell a live owner from a dead one, so it must
    /// never be read as "the daily driver was killed" — on a filesystem without
    /// `flock` another start's [`probe`] answers `Unknown` and leaves it alone,
    /// and where `flock` works the sweep may remove it while its owner lives,
    /// which is what every marker risked before this module. `None` only when
    /// neither file can be created.
    pub(crate) fn arm(dir: &Path, pid: u32, nanos: u128, owner: MarkerOwner) -> Option<Armed> {
        if let Some((fd, path)) = create_locked(dir, &file_name(pid, nanos, owner)) {
            return Some(Armed {
                fd,
                path,
                locked: true,
            });
        }
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::io::IntoRawFd as _;
        let path = dir.join(file_name(pid, nanos, MarkerOwner::Other));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .ok()?;
        Some(Armed {
            fd: file.into_raw_fd(),
            path,
            locked: false,
        })
    }

    /// The same marker, named for `owner` instead.
    #[must_use]
    pub(crate) fn renamed_for(path: &Path, owner: MarkerOwner) -> Option<PathBuf> {
        let name = path.file_name()?.to_str()?;
        let (stem, _) = name.strip_suffix(".log")?.rsplit_once('-')?;
        Some(path.with_file_name(format!("{stem}-{}.log", owner.tag())))
    }
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering};

    /// Fatal signals we trap. Each bypasses the Rust panic hook entirely, so
    /// without this handler they leave no on-disk trace for a windowed app.
    const FATAL_SIGNALS: [i32; 5] = [
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];

    /// Static, pre-built banner halves written around the formatted signal
    /// number. Keeping these as `&[u8]` constants means the handler does no
    /// string work at signal time — it just `write(2)`s known bytes. The same
    /// line goes to stderr and to the marker, whether or not a marker was
    /// opened, so it states only the signal.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal signal ";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// fd of the pre-opened crash-marker file, or `-1` when none was opened
    /// (no writable private dir at install time). `AtomicI32` so the handler
    /// reads it without a lock; written once during `install`.
    static MARKER_FD: AtomicI32 = AtomicI32::new(-1);

    /// Tripped once we have armed the `sigaction` handlers, so a second
    /// `install` call (defensive) does not re-open the marker fd.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// The marker's path as a leaked NUL-terminated string, for the clean-exit
    /// `unlink` ([`release_own_marker`]), or null when there is none (no marker,
    /// or already released). A raw pointer in an atomic so the release is one
    /// swap — async-signal-safe, no lock, no allocation.
    static MARKER_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    /// The pid that armed the marker: [`release_own_marker`] in any other
    /// process (a forked child calling `exit`) is a no-op.
    static OWNER_PID: AtomicI32 = AtomicI32::new(0);

    /// Set for an [`super::Arming::AppAfterCommit`] launch until
    /// [`adopt_app_identity`] renames the marker.
    static PROMOTE_ON_COMMIT: AtomicBool = AtomicBool::new(false);

    /// The `<nanos>` this process's marker is named with, once armed; 0 when
    /// none was. Written once at install, read by [`super::own_marker_nanos`].
    static MARKER_NANOS: AtomicU64 = AtomicU64::new(0);

    /// See [`super::own_marker_nanos`].
    pub(super) fn own_marker_nanos() -> Option<u128> {
        match MARKER_NANOS.load(Ordering::SeqCst) {
            0 => None,
            nanos => Some(u128::from(nanos)),
        }
    }

    /// See [`super::own_marker_place`].
    pub(super) fn own_marker_place() -> Option<(std::path::PathBuf, u128)> {
        let nanos = own_marker_nanos()?;
        let current = MARKER_PATH.load(Ordering::SeqCst);
        if current.is_null() {
            return None;
        }
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: a non-null MARKER_PATH is a leaked, never-freed CString
        // (`publish_marker_path`).
        let current = unsafe { std::ffi::CStr::from_ptr(current) };
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(current.to_bytes()));
        Some((path.parent()?.to_path_buf(), nanos))
    }

    /// Park `path` in [`MARKER_PATH`] (leaking the string: it must outlive every
    /// exit path). Returns false when the path holds a NUL.
    fn publish_marker_path(path: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        MARKER_PATH.store(c_path.into_raw(), Ordering::SeqCst);
        true
    }

    /// See [`super::release_own_marker`].
    pub(super) fn release_own_marker() {
        // SAFETY: `getpid` is async-signal-safe and has no preconditions.
        if unsafe { libc::getpid() } != OWNER_PID.load(Ordering::SeqCst) {
            return;
        }
        let path = MARKER_PATH.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !path.is_null() {
            // SAFETY: a non-null MARKER_PATH is a leaked, never-freed CString
            // (`publish_marker_path`); `unlink` is async-signal-safe.
            unsafe { libc::unlink(path) };
        }
    }

    /// The `atexit` half of [`release_own_marker`]: every `exit(3)` of this
    /// process removes its marker. Registered FIRST among the process's handlers
    /// that matter here (at `logging::init`, before any library is loaded), so it
    /// runs LAST: a static destructor that crashes during `exit` still finds the
    /// marker linked and its banner still lands in a file the next launch reads.
    extern "C" fn release_at_exit() {
        release_own_marker();
    }

    unsafe extern "C" {
        /// `stdlib.h`: register a handler for `exit(3)`. Declared here, as the
        /// launch-fatal fixture in `lib.rs` declares it, rather than added to
        /// `aterm-libc`: this marker's removal is the one handler the product
        /// registers, and it runs on `exit(3)` only — every `_exit` path
        /// (the launch-fatal law) removes the marker itself, through
        /// [`super::clean_exit_now`].
        fn atexit(cb: extern "C" fn()) -> libc::c_int;
    }

    /// See [`super::adopt_app_identity`].
    pub(super) fn adopt_app_identity() -> bool {
        if !PROMOTE_ON_COMMIT.swap(false, Ordering::SeqCst) {
            return false;
        }
        let current = MARKER_PATH.load(Ordering::SeqCst);
        if current.is_null() {
            return true;
        }
        use std::os::unix::ffi::OsStrExt as _;
        // SAFETY: a non-null MARKER_PATH is a leaked, never-freed CString.
        let current = unsafe { std::ffi::CStr::from_ptr(current) };
        let current = std::path::Path::new(std::ffi::OsStr::from_bytes(current.to_bytes()));
        let Some(app) = super::markers::renamed_for(current, super::MarkerOwner::App) else {
            return true;
        };
        // The fd, and with it the lock and the handler's target, rides the
        // rename: it binds to the inode, not the name.
        if std::fs::rename(current, &app).is_ok() {
            publish_marker_path(&app);
        }
        true
    }

    /// Maximum decimal digits a `u32` ever needs (`4294967295`). A signal
    /// number is far smaller, but sizing for `u32` keeps the helper reusable
    /// and the buffer trivially large enough.
    const U32_MAX_DIGITS: usize = 10;

    /// Format `value` as decimal ASCII into the tail of `buf`, returning the
    /// filled sub-slice. ALLOCATION-FREE and async-signal-safe: it only writes
    /// bytes into the caller's stack buffer, so it is callable from the signal
    /// handler. Digits are produced least-significant-first from the end of the
    /// buffer, then the populated tail slice is returned. Zero yields `"0"`.
    fn fmt_u32_decimal(value: u32, buf: &mut [u8; U32_MAX_DIGITS]) -> &[u8] {
        let mut n = value;
        // Index just past the last byte; we fill backwards from here.
        let mut pos = buf.len();
        loop {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        &buf[pos..]
    }

    /// Async-signal-safe `write(2)` of `bytes` to `fd`, looping over short and
    /// `EINTR`-interrupted writes. Returns once everything is written or a
    /// non-retryable error is seen. Errors are swallowed: a crash handler that
    /// cannot write its marker must still fall through to re-raising the signal.
    fn write_all_signal_safe(fd: i32, bytes: &[u8]) {
        if fd < 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            // SAFETY: `bytes[off..]` is a live slice for the duration of the
            // call; `fd` is checked non-negative above. `write` is on the POSIX
            // async-signal-safe list, so calling it from a signal handler is
            // sound. A short (`n >= 0`) or error (`n < 0`) return is handled
            // below; we never deref the return as a pointer.
            let n = unsafe {
                libc::write(
                    fd,
                    bytes[off..].as_ptr().cast::<libc::c_void>(),
                    bytes.len() - off,
                )
            };
            if n > 0 {
                off += n as usize;
                continue;
            }
            // n == 0 (nothing written) or n < 0 (error). Retry only on EINTR;
            // otherwise give up so we still re-raise the signal. `errno` is a
            // thread-local read, which is async-signal-safe.
            if n < 0 {
                // SAFETY: `errno_location()` returns a valid, thread-local
                // `*mut c_int`; reading it on this thread is sound and
                // async-signal-safe.
                let err = unsafe { *errno_location() };
                if err == libc::EINTR {
                    continue;
                }
            }
            break;
        }
    }

    /// Pointer to the calling thread's `errno` (`__error` on macOS/BSD,
    /// `__errno_location` elsewhere). Both are async-signal-safe.
    ///
    /// # Safety
    /// The returned pointer is valid for the calling thread only; deref it on
    /// that thread.
    #[inline]
    unsafe fn errno_location() -> *mut libc::c_int {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: `__error` returns this thread's errno address.
            unsafe { libc::__error() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // SAFETY: `__errno_location` returns this thread's errno address.
            unsafe { libc::__errno_location() }
        }
    }

    /// The fatal-signal handler. Installed without `SA_SIGINFO`, so it receives
    /// only the signal number. EVERYTHING here is async-signal-safe: a stack
    /// buffer, the allocation-free decimal helper, `write(2)`, `sigaction(2)`
    /// to restore the default disposition, and `raise(3)`.
    extern "C" fn handle_fatal_signal(sig: libc::c_int) {
        // Format the (non-negative) signal number with no allocation.
        let mut digits = [0u8; U32_MAX_DIGITS];
        let num = fmt_u32_decimal(sig.max(0) as u32, &mut digits);

        // Banner -> STDERR, then the same banner -> the pre-opened marker fd.
        let marker_fd = MARKER_FD.load(Ordering::Relaxed);
        for fd in [libc::STDERR_FILENO, marker_fd] {
            write_all_signal_safe(fd, BANNER_PREFIX);
            write_all_signal_safe(fd, num);
            write_all_signal_safe(fd, BANNER_SUFFIX);
        }

        // Restore the default disposition for THIS signal and re-raise it, so
        // the process dies the conventional way (core dump / exit status),
        // exactly as if we had never trapped it.
        //
        // SAFETY: `act` is a fully-zeroed, correctly-sized `sigaction` with
        // `sa_sigaction = SIG_DFL`; `sigaction`/`raise` are async-signal-safe.
        // We ignore the return values: if restoring fails we still `raise`,
        // and a handler must not unwind, so there is nothing to propagate.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Open this launch's crash marker (`0600`, locked for the process's life —
    /// [`super::markers`]) and return its raw fd, or `-1` when no private dir is
    /// available. Sweeps the dead owners' markers first. Done AT INSTALL TIME —
    /// `open`/path work is not signal-safe.
    fn open_marker_fd(owner: super::MarkerOwner) -> i32 {
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        let pid = std::process::id();
        super::markers::keep_swept(super::markers::sweep(&dir, pid));
        // The fd is leaked on purpose: the marker must stay open for the whole
        // process so the handler can write to it (and its lock must stay held).
        // The OS reclaims it at exit.
        let nanos = super::startup_nanos();
        match super::markers::arm(&dir, pid, nanos, owner) {
            Some(armed) if publish_marker_path(&armed.path) => {
                MARKER_NANOS.store(u64::try_from(nanos).unwrap_or(0), Ordering::SeqCst);
                if !armed.locked {
                    // The unlocked fallback is named `Other` and must stay so: a
                    // Commit must not rename it into a marker that reads as a
                    // killed daily driver while its owner lives.
                    PROMOTE_ON_COMMIT.store(false, Ordering::SeqCst);
                }
                armed.fd
            }
            Some(armed) => {
                let _ = std::fs::remove_file(&armed.path);
                // SAFETY: closes the descriptor `arm` returned, once.
                unsafe { libc::close(armed.fd) };
                -1
            }
            None => -1,
        }
    }

    /// Arm async-signal-safe capture of the fatal signals. Idempotent: the
    /// marker fd is opened (and handlers installed) only on the first call.
    pub(super) fn install_signal_handlers(arming: super::Arming) {
        if ARMED.swap(true, Ordering::SeqCst) {
            return; // already armed
        }
        // SAFETY: `getpid` has no preconditions.
        OWNER_PID.store(unsafe { libc::getpid() }, Ordering::SeqCst);
        PROMOTE_ON_COMMIT.store(arming == super::Arming::AppAfterCommit, Ordering::SeqCst);
        // Pre-open the marker fd now (NOT in the handler — `open` is unsafe in
        // a signal context). A `-1` simply means the handler writes to stderr
        // only.
        let fd = open_marker_fd(arming.initial_owner());
        MARKER_FD.store(fd, Ordering::SeqCst);
        if fd >= 0 {
            // SAFETY: registers a plain `extern "C"` function with no captured
            // state; a failure to register only costs the clean-exit removal.
            unsafe { atexit(release_at_exit) };
        }

        for &sig in &FATAL_SIGNALS {
            // SAFETY: `act` is a zeroed `sigaction` with a valid function
            // pointer in `sa_sigaction`, an empty (zeroed, then `sigemptyset`)
            // signal mask, and standard flags. `sigaction` is called outside
            // any signal context (install time) with a correctly-shaped struct,
            // so the call is sound. We ignore the result — a failure to arm one
            // signal must not abort startup of the terminal.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                // The `sa_sigaction` field is the function-pointer slot; libc
                // types it as `usize`, so cast via a thin pointer (not a direct
                // fn-item-to-int cast, which lints).
                act.sa_sigaction = handle_fatal_signal as *const () as usize;
                // SA_RESTART so syscalls we trap *out of* are restarted where
                // possible. SA_NODEFER is intentionally NOT set, so the signal
                // stays masked while our handler runs.
                // SA_ONSTACK: a SIGSEGV from a STACK OVERFLOW arrives with no
                // stack to run a handler on; without the alternate stack the
                // handler itself faults and the process dies silently — no
                // banner, no marker, no "closed unexpectedly" on the next
                // launch. Rust's runtime installs a sigaltstack on the main
                // thread and on every std::thread at start (that is how its own
                // "has overflowed its stack" message works), and replacing its
                // SIGSEGV/SIGBUS handler here does not remove those stacks, so
                // the flag is all it takes to reuse them. Found by the
                // 2026-09-02 abort audit.
                act.sa_flags = libc::SA_RESTART | libc::SA_ONSTACK;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Render `value` via the allocation-free helper into a `String` for
        /// easy comparison against `to_string`.
        fn render(value: u32) -> String {
            let mut buf = [0u8; U32_MAX_DIGITS];
            let bytes = fmt_u32_decimal(value, &mut buf);
            String::from_utf8(bytes.to_vec()).unwrap()
        }
        // (imp_windows carries the mirror-image tests for its hex helper.)

        #[test]
        fn decimal_formatter_matches_std_for_many_values() {
            // Exhaustive small range + boundaries + the widest u32 cases.
            for v in 0u32..2000 {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                9,
                10,
                11,
                99,
                100,
                255, // max u8 — bigger than any signal number
                999,
                1000,
                65_535,
                1_000_000,
                4_294_967_294,
                u32::MAX, // 4294967295 — the widest 10-digit case
            ] {
                assert_eq!(render(v), v.to_string(), "mismatch at {v}");
            }
        }

        #[test]
        fn decimal_formatter_handles_zero_without_dropping_the_digit() {
            assert_eq!(render(0), "0");
        }

        #[test]
        fn every_fatal_signal_number_formats_within_the_buffer() {
            for &sig in &FATAL_SIGNALS {
                let mut buf = [0u8; U32_MAX_DIGITS];
                let bytes = fmt_u32_decimal(sig as u32, &mut buf);
                assert_eq!(
                    bytes,
                    sig.to_string().as_bytes(),
                    "signal {sig} formatted wrong"
                );
            }
        }

        #[test]
        fn banner_bytes_are_the_expected_marker_text() {
            assert_eq!(BANNER_PREFIX, b"aterm: fatal signal ");
            assert_eq!(BANNER_SUFFIX, b"\n");
            // The assembled banner around a sample signal number reads
            // correctly end to end.
            let mut buf = [0u8; U32_MAX_DIGITS];
            let num = fmt_u32_decimal(libc::SIGSEGV as u32, &mut buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(num);
            line.extend_from_slice(BANNER_SUFFIX);
            let text = String::from_utf8(line).unwrap();
            assert_eq!(text, format!("aterm: fatal signal {}\n", libc::SIGSEGV));
        }

        #[test]
        fn install_is_idempotent_and_arms_a_handler_for_sigsegv() {
            // Calling twice must not panic and must not re-open the marker.
            install_signal_handlers(crate::crash_signal::Arming::Other);
            install_signal_handlers(crate::crash_signal::Arming::Other);

            // Read back the current SIGSEGV disposition with a null `act`; a
            // handler should now be installed (neither SIG_DFL nor SIG_IGN).
            // We do NOT raise the signal.
            //
            // SAFETY: `old` is a zeroed, correctly-sized `sigaction`; passing a
            // null `act` makes `sigaction` a pure query of the current
            // disposition, which it writes into `old`. Called at test (non-
            // signal) time.
            let installed = unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                let rc = libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut old);
                rc == 0 && old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN
            };
            assert!(
                installed,
                "SIGSEGV should have a custom handler after install"
            );
        }
    }
}

/// The marker lifecycle's rules, each against real files, real `flock`s and
/// real processes in a scratch dir — never the user's log dir.
#[cfg(all(test, unix))]
mod marker_tests {
    use super::MarkerOwner;
    use super::markers::{self, Owner};
    use std::path::Path;

    /// A scratch log dir.
    fn scratch(tag: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir()
            .expect("scratch dir")
    }

    /// An EMPTY marker nobody holds — what a dead owner leaves.
    fn dead_marker(dir: &Path, pid: u32, owner: MarkerOwner) -> std::path::PathBuf {
        let path = dir.join(markers::file_name(pid, 7, owner));
        std::fs::write(&path, b"").unwrap();
        path
    }

    /// A pid that WAS a process and is not one any more: a reaped child's.
    fn reaped_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    }

    /// The sweep as it was before the lifecycle: every empty `crash-*` marker
    /// goes, owner alive or not. The negative control the tests below run
    /// against the same files.
    fn sweep_as_before(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("crash-")
                && name.ends_with(".log")
                && entry.metadata().is_ok_and(|m| m.len() == 0)
            {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
    }

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        let name = markers::file_name(4242, 1_790_309_523_499_676_000, MarkerOwner::App);
        assert_eq!(name, "crash-marker-4242-1790309523499676000-app.log");
        assert_eq!(
            markers::parse(&name),
            Some(markers::MarkerName {
                pid: 4242,
                nanos: 1_790_309_523_499_676_000,
                owner: MarkerOwner::App
            })
        );
        let other = markers::file_name(1, 2, MarkerOwner::Other);
        assert_eq!(markers::parse(&other).unwrap().owner, MarkerOwner::Other);
        for stranger in [
            "crash-signal-4242-7.log",
            "crash-marker-4242-7-app.log.seen",
            ".crash-marker-4242-7-app.log.pending",
            "crash-marker-x-7-app.log",
            "crash-marker-4242--app.log",
            "crash-marker-4242-7-root.log",
            "crash-4242.log",
            "aterm.log",
        ] {
            assert_eq!(markers::parse(stranger), None, "{stranger}");
        }
        let path = Path::new("/l").join(&other);
        assert_eq!(
            markers::renamed_for(&path, MarkerOwner::App).unwrap(),
            Path::new("/l/crash-marker-1-2-app.log")
        );
    }

    /// THE MEASURED DEFECT: a second start's sweep unlinked the live daily
    /// driver's marker. A marker whose owner holds its lock (here through a
    /// SECOND open file description in this very process — `flock` conflicts
    /// across descriptions) survives the sweep; the dead one beside it goes.
    /// NEGATIVE CONTROL: the sweep as it was removes both.
    #[test]
    fn a_sweep_keeps_a_live_owners_marker_and_removes_a_dead_ones() {
        let dir = scratch("marker-live");
        let own = std::process::id();
        let (fd, live) =
            markers::create_locked(dir.path(), &markers::file_name(own, 1, MarkerOwner::Other))
                .expect("arm a locked marker");
        let dead = dead_marker(dir.path(), reaped_pid(), MarkerOwner::Other);
        assert_eq!(markers::probe(&live), Owner::Live);
        assert_eq!(markers::probe(&dead), Owner::Dead);

        markers::sweep(dir.path(), own.wrapping_add(1));
        assert!(
            live.exists(),
            "a live owner's marker must survive the sweep"
        );
        assert!(!dead.exists(), "a dead owner's empty marker is swept");

        sweep_as_before(dir.path());
        assert!(
            !live.exists(),
            "control: the old sweep unlinks the live marker the new one keeps"
        );
        // SAFETY: closes the descriptor `create_locked` returned, once.
        unsafe { libc::close(fd) };
    }

    /// `arm` takes the locked lifecycle when it can: the marker is named for its
    /// owner and its lock is held. When the locked arming cannot be made — here
    /// a stale pending entry the exclusive create cannot replace, standing in
    /// for a filesystem without `flock` or a refused rename — the launch still
    /// gets a marker, so a native crash still leaves its banner on disk; it is
    /// UNLOCKED and so named `other` even for an `app` launch, and is therefore
    /// never evidence of a killed daily driver. NEGATIVE CONTROL: the locked
    /// arming alone (what `open_marker_fd` called before) leaves no marker at all
    /// in that state.
    #[test]
    fn a_launch_that_cannot_lock_still_gets_an_unlocked_marker_that_is_never_news() {
        let dir = scratch("marker-arm");
        let own = std::process::id();
        let armed = markers::arm(dir.path(), own + 11, 5, MarkerOwner::App).expect("armed");
        assert!(armed.locked);
        assert_eq!(
            armed.path,
            dir.path()
                .join(markers::file_name(own + 11, 5, MarkerOwner::App))
        );
        assert_eq!(markers::probe(&armed.path), Owner::Live);
        // Its clean exit: unlock, then remove.
        // SAFETY: closes the descriptor `arm` returned, once.
        unsafe { libc::close(armed.fd) };
        std::fs::remove_file(&armed.path).unwrap();

        let name = markers::file_name(own + 12, 6, MarkerOwner::App);
        std::fs::create_dir(dir.path().join(format!(".{name}.pending"))).unwrap();
        assert!(
            markers::create_locked(dir.path(), &name).is_none(),
            "control: the locked arming alone leaves this launch without a marker"
        );
        let fallback = markers::arm(dir.path(), own + 12, 6, MarkerOwner::App).expect("fallback");
        assert!(!fallback.locked);
        assert_eq!(
            fallback.path,
            dir.path()
                .join(markers::file_name(own + 12, 6, MarkerOwner::Other)),
            "unlocked, so never named for the daily driver"
        );
        // The handler's target: a banner written to the fd lands in the file.
        // SAFETY: `fallback.fd` is the open descriptor `arm` returned.
        let wrote = unsafe { libc::write(fallback.fd, b"x".as_ptr().cast(), 1) };
        assert_eq!(wrote, 1);
        assert_eq!(std::fs::read(&fallback.path).unwrap(), b"x");
        std::fs::write(&fallback.path, b"").unwrap();
        assert!(
            markers::dead_app_markers(dir.path(), own).is_empty(),
            "an empty unlocked marker is not a killed daily driver"
        );
        // SAFETY: closes the descriptor `arm` returned, once.
        unsafe { libc::close(fallback.fd) };
    }

    /// A dead `app` marker is news: the sweep leaves it for the windowed
    /// launch, which finds it. A dead `other` marker, a live `app` one, and
    /// this process's OWN previous image's `app` marker (same pid: a boot
    /// re-exec) are not.
    #[test]
    fn only_a_dead_app_marker_of_another_process_is_evidence() {
        let dir = scratch("marker-news");
        let own = std::process::id();
        let gone = reaped_pid();
        let killed = dead_marker(dir.path(), gone, MarkerOwner::App);
        let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
        let previous_image = dead_marker(dir.path(), own, MarkerOwner::App);
        let (fd, live_app) = markers::create_locked(
            dir.path(),
            &markers::file_name(own + 7, 3, MarkerOwner::App),
        )
        .unwrap();

        assert_eq!(
            markers::dead_app_markers(dir.path(), own),
            vec![killed.clone()]
        );
        markers::sweep(dir.path(), own);
        assert!(killed.exists(), "left for the report");
        assert!(!test_start.exists(), "a test start's corpse is not news");
        assert!(
            !previous_image.exists(),
            "this process's own previous image"
        );
        assert!(live_app.exists());
        // A non-empty marker is a crash record: never swept, never "killed".
        std::fs::write(&killed, b"aterm: fatal signal 11").unwrap();
        markers::sweep(dir.path(), own);
        assert!(killed.exists());
        assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        // SAFETY: closes the descriptor `create_locked` returned, once.
        unsafe { libc::close(fd) };
    }

    /// The sweep ANSWERS the corpses it removed: a dead `other` start's empty
    /// marker is no row, but it is the evidence the crash journal reopens that
    /// start's layout by, and the sweep that removes it runs before the claim.
    /// This process's own previous image, a live marker and a crash record are
    /// not in the answer. NEGATIVE CONTROL: the marker the answer names is gone
    /// from the directory, so a claim that read only the directory would find
    /// no evidence at all.
    #[test]
    fn the_sweep_answers_the_dead_starts_it_removed() {
        let dir = scratch("marker-swept");
        let own = std::process::id();
        let gone = reaped_pid();
        let test_start = dead_marker(dir.path(), gone, MarkerOwner::Other);
        let previous_image = dead_marker(dir.path(), own, MarkerOwner::Other);
        let record = dir.path().join(markers::file_name(
            gone.wrapping_add(1),
            7,
            MarkerOwner::Other,
        ));
        std::fs::write(&record, b"aterm: fatal signal 11").unwrap();
        let (fd, live) = markers::create_locked(
            dir.path(),
            &markers::file_name(own + 3, 3, MarkerOwner::Other),
        )
        .unwrap();

        let swept = markers::sweep(dir.path(), own);
        assert_eq!(
            swept,
            vec![markers::MarkerName {
                pid: gone,
                nanos: 7,
                owner: MarkerOwner::Other
            }]
        );
        assert!(
            !test_start.exists(),
            "control: the evidence left the directory"
        );
        assert!(!previous_image.exists());
        assert!(record.exists() && live.exists());
        // SAFETY: closes the descriptor `create_locked` returned, once.
        unsafe { libc::close(fd) };
    }

    /// A build that predates the lifecycle never locked its `crash-signal-*`
    /// marker, so its pid is the only liveness there is: a live pid keeps its
    /// marker, a gone one does not. A pending arming is kept while locked.
    #[test]
    fn legacy_markers_follow_their_pid_and_pending_ones_their_lock() {
        let dir = scratch("marker-legacy");
        let own = std::process::id();
        let alive = dir.path().join(format!("crash-signal-{own}-5.log"));
        let gone = dir
            .path()
            .join(format!("crash-signal-{}-5.log", reaped_pid()));
        let record = dir
            .path()
            .join(format!("crash-signal-{}-6.log", reaped_pid()));
        std::fs::write(&alive, b"").unwrap();
        std::fs::write(&gone, b"").unwrap();
        std::fs::write(&record, b"aterm: fatal signal 6").unwrap();
        let name = markers::file_name(reaped_pid(), 9, MarkerOwner::Other);
        let stale_pending = dir.path().join(format!(".{name}.pending"));
        std::fs::write(&stale_pending, b"").unwrap();

        markers::sweep(dir.path(), own.wrapping_add(1));
        assert!(
            alive.exists(),
            "an older build still running keeps its marker"
        );
        assert!(!gone.exists());
        assert!(record.exists(), "a crash record is never swept");
        assert!(!stale_pending.exists(), "an arming that died mid-way");
    }

    /// Two real processes: a child arms a marker the way `install` does and
    /// is SIGKILLed — its lock dies with it, so the marker reads Dead; while
    /// it lived, the same marker read Live from here.
    #[test]
    fn a_killed_owner_releases_its_lock() {
        use std::io::Read as _;
        let dir = scratch("marker-kill");
        // The child: take the lock the way `create_locked` does (python is not
        // assumed; `flock(1)` is not on macOS), so this uses a perl one-liner
        // when perl is present and skips otherwise.
        let Ok(perl) = std::process::Command::new("/usr/bin/perl")
            .arg("-e")
            .arg(
                "use Fcntl qw(:flock); open(my $f, '>', $ARGV[0]) or die; \
                 flock($f, LOCK_EX|LOCK_NB) or die; $|=1; print \"locked\\n\"; sleep 60",
            )
            .arg(dir.path().join(markers::file_name(1, 1, MarkerOwner::App)))
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("SKIP: /usr/bin/perl is not available");
            return;
        };
        let mut child = perl;
        let mut ready = [0u8; 7];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .expect("the child locked its marker");
        let path = dir.path().join(markers::file_name(1, 1, MarkerOwner::App));
        assert_eq!(markers::probe(&path), Owner::Live);
        assert!(markers::dead_app_markers(dir.path(), 2).is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(markers::probe(&path), Owner::Dead);
        assert_eq!(markers::dead_app_markers(dir.path(), 2), vec![path]);
    }
}

/// THE CLEAN-EXIT LAW, gated in source: every `_exit` in this crate that is not
/// a crash goes through [`clean_exit_now`] (or [`crate::exit_without_process_teardown`],
/// which does), so its marker is removed. A bare `libc::_exit` left on a clean path
/// leaves an empty marker the next windowed launch of the installed app reports as
/// "aterm was killed" — a false alarm in the one place a true one must be believed.
/// The allowed bare sites are FORKED CHILDREN (the PTY spawn's exec-failure exit,
/// test fixtures), which never armed a marker of their own.
#[cfg(all(test, unix))]
mod exit_gate {
    /// Every `.rs` file under `dir`, recursively.
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The call the gate counts, spelled in two halves so this file's own gate
    /// code is not one of the sites it counts.
    const NEEDLE: &str = concat!("libc::", "_exit(");

    #[test]
    fn no_clean_exit_path_leaves_its_marker_behind() {
        // (file, bare `_exit` sites it may keep): the forked children — two
        // of them in the harness host's test fixture of a shell's job
        // (`harness_host::tests::pty_job`), one the rendezvous listener
        // fixture in a test — and `clean_exit_now` itself.
        let allowed = [
            ("spawn.rs", 1usize),
            ("app_update_handoff.rs", 1),
            ("harness_host.rs", 2),
            ("handoff_rendezvous.rs", 1),
            ("crash_signal.rs", 1),
        ];
        let mut files = Vec::new();
        sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.len() > 50, "the walk found the crate's sources");
        for path in files {
            let src = std::fs::read_to_string(&path).expect("read source");
            let bare = src
                .lines()
                .filter(|line| {
                    let code = line.trim_start();
                    !code.starts_with("//") && code.contains(NEEDLE)
                })
                .count();
            let file = path.file_name().unwrap().to_string_lossy();
            let may = allowed
                .iter()
                .find(|(name, _)| *name == file)
                .map_or(0, |(_, n)| *n);
            assert_eq!(
                bare,
                may,
                "{}: a bare `_exit` outside a forked child — spell a clean exit \
                 `crate::crash_signal::clean_exit_now(code)` so the marker goes with it",
                path.display()
            );
        }
    }
}

/// The Windows crash lane: an unhandled-exception filter
/// (`SetUnhandledExceptionFilter`) writing a crash artifact. Native fatal
/// exceptions dispatched through the OS exception machinery — access violation
/// `0xC0000005`, illegal instruction, stack overflow, etc. — bypass the Rust
/// panic hook the way POSIX fatal signals do, and this filter catches them
/// under the unix module's discipline: everything non-trivial (path
/// resolution, `open`, allocation) happens ONCE at install time, and the
/// filter itself does no allocation (stack hex buffers + pre-built banner
/// bytes + `WriteFile` loops), because it may run with a corrupted heap.
///
/// It does NOT mirror the unix `SIGABRT` coverage. Rust's `abort_internal` on
/// Windows is `__fastfail` (`int 0x29`), which the kernel terminates on
/// *without* dispatching to any in-process filter — so OOM/`process::abort`/
/// double-panic never reach here. See [`install_signal_handlers`] for the full
/// asymmetry and the WER-LocalDumps escape hatch.
#[cfg(windows)]
mod imp_windows {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(
            filter: extern "system" fn(*mut ExceptionPointers) -> i32,
        ) -> *mut core::ffi::c_void;
        fn GetStdHandle(which: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn WriteFile(
            handle: isize,
            data: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }

    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;
    /// Hand the exception on to the default WER/termination path after the
    /// marker is written — the analogue of the unix restore-default-and-raise.
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    /// Minimal `EXCEPTION_POINTERS`: only `ExceptionRecord` is followed.
    #[repr(C)]
    struct ExceptionPointers {
        exception_record: *mut ExceptionRecord,
        context_record: *mut core::ffi::c_void,
    }

    /// `EXCEPTION_RECORD` with its documented fixed layout. `#[repr(C)]` with
    /// the native field types reproduces the OS struct (including the
    /// alignment padding before `exception_information` on 64-bit), so we can
    /// read `ExceptionAddress` (the faulting PC) and, for an access violation,
    /// the read/write flag + faulting address from `ExceptionInformation` — all
    /// allocation-free, straight out of the record the filter is handed.
    #[repr(C)]
    struct ExceptionRecord {
        exception_code: u32,
        // Present only to place the fields after them at their native offsets;
        // we never read the flags or the nested-record pointer.
        exception_flags: u32,
        exception_record: *mut ExceptionRecord,
        exception_address: *mut core::ffi::c_void,
        number_parameters: u32,
        exception_information: [usize; 15],
    }

    /// `STATUS_ACCESS_VIOLATION`: the one code whose `ExceptionInformation`
    /// layout (op at [0], faulting address at [1]) we decode.
    const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;

    /// Static, pre-built banner fragments written around the hex fields, so the
    /// filter does no string work at exception time (same shape as the unix
    /// banner; `\u{2014}` is the em dash, UTF-8 `E2 80 94`). The assembled line
    /// is `PREFIX <code> AT <pc> [AV_OP <op> AV_AT <addr>] SUFFIX`. The same
    /// line goes to stderr and to the marker, whether or not a marker was
    /// opened, so it states only the exception.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal exception 0x";
    const BANNER_AT: &[u8] = b" at 0x";
    const BANNER_AV_OP: &[u8] = " \u{2014} access violation ".as_bytes();
    const BANNER_AV_AT: &[u8] = b" @ 0x";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// Raw HANDLE of the pre-opened crash-marker file, or `-1` when none was
    /// opened (no writable log dir at install time). `AtomicIsize` so the
    /// filter reads it without a lock; written once during `install`, swapped
    /// back to `-1` by `release_marker` at a clean exit (`pub(super)` so the
    /// shared clean-exit test can see that it was).
    pub(super) static MARKER_HANDLE: AtomicIsize = AtomicIsize::new(-1);

    /// This launch's marker path, parked at install time by
    /// `open_marker_handle`, so [`release_marker`] can find the file without
    /// re-deriving anything (the nanos salt in the name cannot be recomputed).
    /// Set only when the open succeeded: no marker, no path. (The unix lane
    /// parks its own, signal-safe, in `imp::MARKER_PATH`.) `pub(super)` for the
    /// same shared test as [`MARKER_HANDLE`].
    pub(super) static MARKER_PATH: std::sync::OnceLock<std::path::PathBuf> =
        std::sync::OnceLock::new();

    /// Tripped once the filter is registered, so a second `install` call
    /// (defensive) does not re-open the marker handle.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Format `value` as exactly 8 uppercase hex digits into `buf`, returning
    /// the filled slice. ALLOCATION-FREE (the sibling of the unix module's
    /// `fmt_u32_decimal`), so it is callable from the exception filter.
    /// Fixed-width on purpose: NTSTATUS codes read conventionally as 8 digits
    /// (`0xC0000005`).
    fn fmt_u32_hex(value: u32, buf: &mut [u8; 8]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((7 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    /// Format `value` as exactly 16 uppercase hex digits — the pointer-width
    /// sibling of [`fmt_u32_hex`], for `ExceptionAddress` and the faulting
    /// address. Also allocation-free and callable from the filter.
    fn fmt_u64_hex(value: u64, buf: &mut [u8; 16]) -> &[u8] {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((value >> ((15 - i) * 4)) & 0xF) as usize];
        }
        &buf[..]
    }

    /// Allocation-free `WriteFile` loop over short writes — the filter-side
    /// analogue of the unix `write_all_signal_safe`. Errors are swallowed: a
    /// filter that cannot write its marker must still return so the default
    /// termination proceeds.
    fn write_all_handle(handle: isize, bytes: &[u8]) {
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            return;
        }
        let mut off = 0usize;
        while off < bytes.len() {
            let mut written = 0u32;
            let len = u32::try_from(bytes.len() - off).unwrap_or(u32::MAX);
            // SAFETY: `bytes[off..]` is a live slice for the call and `written`
            // is a valid out-param; `WriteFile` allocates nothing on our side,
            // so calling it with a possibly-corrupt heap is sound.
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes[off..].as_ptr(),
                    len,
                    &mut written,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                break;
            }
            off += written as usize;
        }
    }

    /// The unhandled-exception filter. EVERYTHING here is allocation-free: a
    /// stack hex buffer, the pre-built banner bytes, and `WriteFile` loops to
    /// the stderr + marker handles. Returns `EXCEPTION_CONTINUE_SEARCH` so the
    /// default WER/termination path still runs.
    extern "system" fn handle_fatal_exception(info: *mut ExceptionPointers) -> i32 {
        let mut code = 0u32;
        let mut address = 0u64;
        let mut av: Option<(&[u8], u64)> = None;
        // SAFETY: the OS passes a valid `EXCEPTION_POINTERS`; both pointers are
        // guarded anyway (a null record just formats code 0). The record has
        // the documented fixed `#[repr(C)]` layout, so reading `number_parameters`
        // before indexing `exception_information` stays in bounds.
        unsafe {
            if !info.is_null() {
                let rec = (*info).exception_record;
                if !rec.is_null() {
                    code = (*rec).exception_code;
                    address = (*rec).exception_address as usize as u64;
                    if code == EXCEPTION_ACCESS_VIOLATION && (*rec).number_parameters >= 2 {
                        // ExceptionInformation[0]: 0 read, 1 write, 8 execute.
                        // ExceptionInformation[1]: the inaccessible address.
                        let op: &[u8] = match (*rec).exception_information[0] {
                            0 => b"reading",
                            1 => b"writing",
                            8 => b"executing",
                            _ => b"accessing",
                        };
                        av = Some((op, (*rec).exception_information[1] as u64));
                    }
                }
            }
        }
        let mut code_digits = [0u8; 8];
        let code_hex = fmt_u32_hex(code, &mut code_digits);
        let mut pc_digits = [0u8; 16];
        let pc_hex = fmt_u64_hex(address, &mut pc_digits);
        let mut av_digits = [0u8; 16];
        let av_hex = av.map(|(_, addr)| fmt_u64_hex(addr, &mut av_digits));

        // Banner -> stderr, then the same banner -> the pre-opened marker.
        // SAFETY: `GetStdHandle` is a plain handle query, callable anywhere.
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let marker = MARKER_HANDLE.load(Ordering::Relaxed);
        for h in [stderr, marker] {
            write_all_handle(h, BANNER_PREFIX);
            write_all_handle(h, code_hex);
            write_all_handle(h, BANNER_AT);
            write_all_handle(h, pc_hex);
            if let (Some((op, _)), Some(av_hex)) = (av, av_hex) {
                write_all_handle(h, BANNER_AV_OP);
                write_all_handle(h, op);
                write_all_handle(h, BANNER_AV_AT);
                write_all_handle(h, av_hex);
            }
            write_all_handle(h, BANNER_SUFFIX);
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    /// Open a launch-unique crash-marker file and return its raw HANDLE, or `-1`
    /// when no log dir is available. Sweeps stale empty markers first. Done AT
    /// INSTALL TIME — path work and `open` must not happen inside the filter.
    fn open_marker_handle() -> isize {
        use std::os::windows::io::IntoRawHandle;
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        super::sweep_stale_markers(&dir);
        let path = super::marker_path(&dir);
        match open_marker_file(&path) {
            // Leak the `File` into a raw handle: the marker must stay open for
            // the whole process so the filter can write to it. The OS reclaims
            // it at exit — or `release_marker` does, at a clean one, which is
            // what the parked path is for.
            Ok(f) => {
                let _ = MARKER_PATH.set(path);
                f.into_raw_handle() as isize
            }
            Err(_) => -1,
        }
    }

    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`: everything `std`'s default share
    /// mode grants EXCEPT `FILE_SHARE_DELETE`.
    const MARKER_SHARE_MODE: u32 = 0x1 | 0x2;

    /// Create (truncating) the marker at `path`, HELD AGAINST DELETION for as
    /// long as the returned handle is open. `std` opens with `FILE_SHARE_DELETE`,
    /// which let another launch's [`super::sweep_stale_markers`] delete a live
    /// instance's marker out from under it (the measurement is on that sweep).
    /// Without it, every delete or rename of the file fails with a sharing
    /// violation (error 32) until the handle closes, and the kernel closes it
    /// when the process dies, however it dies — the one owner lock that needs
    /// nothing from a process that is crashing. Readers are unaffected: the
    /// crash scan and a user's editor still open it (read and write sharing are
    /// granted).
    pub(super) fn open_marker_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .share_mode(MARKER_SHARE_MODE)
            .open(path)
    }

    /// Clean-exit release of the marker ([`super::remove_marker_on_clean_exit`]):
    /// swap the handle out FIRST so a fault racing this writes to stderr only,
    /// close it, then remove the file if it is still empty. The close MUST come
    /// first: the handle does not share delete ([`open_marker_file`]), so a
    /// delete while it is open fails with a sharing violation even from this
    /// process. Idempotent — a second call finds no handle and no file.
    pub(super) fn release_marker() {
        let handle = MARKER_HANDLE.swap(-1, Ordering::SeqCst);
        if handle != INVALID_HANDLE_VALUE && handle != 0 {
            // SAFETY: `handle` was leaked out of a `File` by
            // `open_marker_handle`, is owned by this module alone, and the swap
            // above guarantees it is closed exactly once.
            unsafe {
                CloseHandle(handle);
            }
        }
        if let Some(path) = MARKER_PATH.get() {
            super::remove_marker_if_empty(path);
        }
    }

    /// Arm the unhandled-exception filter. Idempotent: the marker handle is
    /// opened (and the filter registered) only on the first call.
    pub(crate) fn install() {
        if ARMED.swap(true, Ordering::SeqCst) {
            return; // already armed
        }
        MARKER_HANDLE.store(open_marker_handle(), Ordering::SeqCst);
        // SAFETY: registers a plain `extern "system"` function pointer with the
        // documented signature. The previous filter is deliberately dropped —
        // we are the outermost crash reporter, and we CONTINUE_SEARCH so the
        // default handling still runs after us.
        unsafe {
            SetUnhandledExceptionFilter(handle_fatal_exception);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Render `value` via the allocation-free hex helper (mirrors the unix
        /// module's decimal-helper tests).
        fn render(value: u32) -> String {
            let mut buf = [0u8; 8];
            String::from_utf8(fmt_u32_hex(value, &mut buf).to_vec()).unwrap()
        }

        #[test]
        fn hex_formatter_matches_std_for_many_values() {
            for v in 0u32..2000 {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v}");
            }
            for v in [
                0,
                1,
                0x8000_0003, // breakpoint
                0xC000_0005, // access violation — the classic
                0xC000_00FD, // stack overflow
                u32::MAX,
            ] {
                assert_eq!(render(v), format!("{v:08X}"), "mismatch at {v:#010X}");
            }
        }

        #[test]
        fn hex64_formatter_matches_std() {
            for v in [0u64, 1, 0xDEAD_BEEF, 0x7FF6_1234_5678_ABCD, u64::MAX] {
                let mut buf = [0u8; 16];
                assert_eq!(
                    String::from_utf8(fmt_u64_hex(v, &mut buf).to_vec()).unwrap(),
                    format!("{v:016X}"),
                    "mismatch at {v:#018X}"
                );
            }
        }

        #[test]
        fn banner_bytes_are_the_expected_marker_text() {
            // Non-AV assembly: code + faulting PC + suffix.
            let mut code_buf = [0u8; 8];
            let mut pc_buf = [0u8; 16];
            let code = fmt_u32_hex(0x8000_0003, &mut code_buf);
            let pc = fmt_u64_hex(0x7FF6_1234_5678_ABCD, &mut pc_buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(code);
            line.extend_from_slice(BANNER_AT);
            line.extend_from_slice(pc);
            line.extend_from_slice(BANNER_SUFFIX);
            assert_eq!(
                String::from_utf8(line).unwrap(),
                "aterm: fatal exception 0x80000003 at 0x7FF612345678ABCD\n"
            );

            // Access-violation assembly threads the op + faulting address in.
            let av_pc = fmt_u64_hex(0x7FF6_0000_0100, &mut pc_buf);
            let mut av_buf = [0u8; 16];
            let av_addr = fmt_u64_hex(0, &mut av_buf);
            let mut code_buf = [0u8; 8];
            let av_code = fmt_u32_hex(0xC000_0005, &mut code_buf);
            let mut line = Vec::new();
            line.extend_from_slice(BANNER_PREFIX);
            line.extend_from_slice(av_code);
            line.extend_from_slice(BANNER_AT);
            line.extend_from_slice(av_pc);
            line.extend_from_slice(BANNER_AV_OP);
            line.extend_from_slice(b"writing");
            line.extend_from_slice(BANNER_AV_AT);
            line.extend_from_slice(av_addr);
            line.extend_from_slice(BANNER_SUFFIX);
            assert_eq!(
                String::from_utf8(line).unwrap(),
                "aterm: fatal exception 0xC0000005 at 0x00007FF600000100 \u{2014} access violation writing @ 0x0000000000000000\n"
            );
        }

        #[cfg(target_pointer_width = "64")]
        #[test]
        fn exception_record_layout_matches_native_offsets() {
            // On x64 the `#[repr(C)]` struct must reproduce the OS field
            // offsets, including the 4 bytes of alignment padding after
            // NumberParameters before ExceptionInformation.
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_code), 0);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_flags), 4);
            assert_eq!(core::mem::offset_of!(ExceptionRecord, exception_record), 8);
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_address),
                16
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, number_parameters),
                24
            );
            assert_eq!(
                core::mem::offset_of!(ExceptionRecord, exception_information),
                32
            );
        }

        #[test]
        fn install_is_idempotent() {
            let _one_at_a_time = super::super::PROCESS_MARKER_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Calling twice must not panic and must not re-open the marker.
            install();
            install();
            assert!(ARMED.load(Ordering::SeqCst));
        }

        /// THE LOCK: a marker opened by [`open_marker_file`] survives a sibling
        /// launch's sweep and any other delete while its handle is open, and is
        /// an ordinary empty leftover once released. The control is the open
        /// this replaced — `std`'s default share mode — whose live marker the
        /// sweep takes, name and all, so the pass is never vacuous.
        #[test]
        fn a_held_marker_survives_the_sweep_and_goes_once_released() {
            let dir =
                std::env::temp_dir().join(format!("aterm-marker-lock-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            let held = dir.join("crash-signal-1-1.log");
            let file = open_marker_file(&held).expect("marker opened");
            super::super::sweep_stale_markers(&dir);
            assert!(held.exists(), "a live marker survives a sibling's sweep");
            let refused = std::fs::remove_file(&held).expect_err("no delete while held");
            assert_eq!(refused.raw_os_error(), Some(32), "a sharing violation");
            let readable = std::fs::read(&held).expect("the crash scan can still read it");
            assert!(readable.is_empty());
            drop(file);
            super::super::sweep_stale_markers(&dir);
            assert!(!held.exists(), "released, it is an ordinary empty leftover");

            let unheld = dir.join("crash-signal-2-2.log");
            let default_open = std::fs::File::create(&unheld).expect("default open");
            super::super::sweep_stale_markers(&dir);
            assert!(
                !unheld.exists(),
                "control: the default share mode lets a sweep unlink a live marker"
            );
            drop(default_open);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// This test process has ONE marker (`imp_windows`' statics) and two tests
/// drive it: `install_is_idempotent` arms it, the clean-exit test releases it.
/// In parallel, the release could land between the arming and its handle
/// store, and the "nothing is held after a release" assertion would read the
/// handle stored after it. They take this in turn. Each test spells the
/// `.lock()` itself rather than calling a guard-returning helper: the
/// lock-order census (tools/freeze-safety-gate, OB-7) sees a hold only where
/// the acquisition token is written, and refuses an unregistered helper.
#[cfg(all(test, windows))]
static PROCESS_MARKER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The Windows clean-exit release, tested at the seam both platforms share:
/// the rule, the removal (plain `std`, so every host runs it), and the harmless
/// no-op when nothing was armed.
#[cfg(all(test, any(unix, windows)))]
mod clean_exit_tests {
    use super::*;

    #[test]
    fn a_marker_is_a_clean_leftover_exactly_when_it_is_empty() {
        assert!(marker_is_clean_leftover(0));
        assert!(!marker_is_clean_leftover(1));
        // The 47-byte launch-fatal banner of the 2026-09-16 incident: a record.
        assert!(!marker_is_clean_leftover(47));
    }

    #[test]
    fn an_empty_marker_is_removed_at_once_and_a_crash_record_is_kept() {
        let dir = std::env::temp_dir().join(format!("aterm-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("crash-signal-1-1.log");
        let crashed = dir.join("crash-signal-2-2.log");
        std::fs::write(&clean, b"").unwrap();
        std::fs::write(&crashed, "aterm: fatal signal 11\n").unwrap();

        assert!(remove_marker_if_empty(&clean), "an empty marker is removed");
        assert!(!clean.exists(), "gone at once, not merely delete-pending");
        assert!(!remove_marker_if_empty(&crashed), "a crash record is kept");
        assert!(crashed.exists());
        // Nothing there: nothing removed, no panic.
        assert!(!remove_marker_if_empty(&dir.join("crash-signal-3-3.log")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seam is reached on every clean exit whether or not a marker was
    /// ever opened (no log dir, a failed open), and must tolerate being reached
    /// twice. Whatever the install tests in this process did before, after the
    /// release nothing is held on Windows; on unix the seam is a no-op (the
    /// lane's `atexit` removal owns it) and must stay harmless.
    #[test]
    fn release_without_an_armed_marker_is_a_harmless_no_op() {
        #[cfg(windows)]
        let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        remove_marker_on_clean_exit();
        remove_marker_on_clean_exit();
        #[cfg(windows)]
        assert_eq!(
            imp_windows::MARKER_HANDLE.load(std::sync::atomic::Ordering::SeqCst),
            -1
        );
    }

    /// The clean exit end to end, armed for real (the install every launch
    /// runs, in the real log dir): while armed the marker is held against a
    /// sibling's sweep, and the release closes the handle and takes the file
    /// with it. The marker may already be released by the no-op test above
    /// (one marker per process, and `install` never re-opens it) — then there
    /// is nothing held and nothing on disk, which the end state checks alike.
    #[cfg(windows)]
    #[test]
    fn the_clean_exit_takes_the_armed_marker_with_it() {
        use std::sync::atomic::Ordering;
        let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        imp_windows::install();
        let path = imp_windows::MARKER_PATH.get().cloned();
        if imp_windows::MARKER_HANDLE.load(Ordering::SeqCst) != -1 {
            let path = path.as_ref().expect("an open marker has its path parked");
            assert!(path.exists(), "armed: {}", path.display());
            assert!(
                std::fs::remove_file(path).is_err(),
                "held against a sibling's sweep"
            );
        }
        remove_marker_on_clean_exit();
        assert_eq!(imp_windows::MARKER_HANDLE.load(Ordering::SeqCst), -1);
        if let Some(path) = path {
            assert!(
                !path.exists(),
                "the clean exit took its own marker: {}",
                path.display()
            );
        }
    }
}
