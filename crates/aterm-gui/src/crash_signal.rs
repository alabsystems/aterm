// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Async-signal-safe capture of native fatal signals.
//!
//! The panic hook in [`crate::logging`] fires only for Rust unwinds. Native fatal signals
//! (`SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, `SIGFPE`) skip the panic machinery and leave nothing
//! on disk for a windowed app whose stderr nobody reads, so a `sigaction(2)` handler writes a
//! crash marker before the process dies.
//!
//! A signal handler may interrupt any instruction, including the middle of `malloc` or a held
//! lock, so it may call only async-signal-safe functions (`signal-safety(7)`): here `write(2)`,
//! `sigaction(2)` and `raise(3)`. Everything else (resolving and opening the marker, allocating,
//! formatting) is done once at install time and parked in `static`s. The handler allocates
//! nothing and takes no lock: it writes a pre-built banner and the signal number to stderr and
//! the marker fd, then restores the default disposition and re-raises the signal, so the process
//! still dies with the conventional status.

/// Arm async-signal-safe fatal-signal capture (`SIGSEGV`/`SIGABRT`/`SIGBUS`/`SIGILL`/`SIGFPE`).
/// `arming` says what kind of start this is, which decides whether this process's marker, left
/// empty by a death that ran no exit path, is news for the next windowed launch
/// ([`Arming::for_launch`]; the Windows lane reports crashes only and ignores it). Call alongside
/// the panic hook so both crash paths are covered. On Windows the analogue is an
/// unhandled-exception filter (`SetUnhandledExceptionFilter`) under the same discipline. A no-op
/// on targets with neither lane.
///
/// Coverage differs by platform. The unix lane traps `SIGABRT`, where Rust's abort paths land
/// (allocation failure, a panic while panicking, an explicit process abort), so those leave a
/// marker. On Windows Rust aborts by `__fastfail` (`int 0x29`), which ends the process without
/// running any in-process exception handler, so an OOM abort or a double panic leaves no marker
/// from this lane (the panic hook still wrote `crash-<pid>.log` for the first panic). Capturing a
/// bare `__fastfail` takes WER LocalDumps
/// (`HKCU\Software\Microsoft\Windows\Windows Error Reporting\LocalDumps\<exe>`), a deployment
/// concern this lane cannot arm.
pub(crate) fn install_signal_handlers_as(arming: Arming) {
    #[cfg(unix)]
    imp::install_signal_handlers(arming);
    #[cfg(not(unix))]
    let _ = arming;
    #[cfg(windows)]
    imp_windows::install();
}

/// Whose marker this is, which decides whether its empty corpse means anything.
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
/// it is, and only [`MarkerOwner::App`] is ever reported — but for the lab's own
/// desk ([`MarkerOwner::Lab`]), whose marker is news to its own scratch root's
/// scan alone ([`MarkerOwner::is_news_to`]). Unix only: the Windows
/// lane keeps its `crash-signal-*` markers — held against deletion by the
/// owner's open handle rather than an owner-named lock — and reports crashes
/// alone.
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "the owner lives in the unix marker name only")
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerOwner {
    /// The installed, updater-owned `.app` bundle running windowed and, when it arrived through
    /// an update handoff, past its Commit: the daily driver.
    App,
    /// Everything else: a test binary, a `target/` or store binary, a dev-marked
    /// bundle, a DMG or translocated launch, a `--headless` start, and a handoff
    /// candidate its parent may still reject (and reap with SIGKILL).
    Other,
    /// A LAB DESK's (session log Phase 6, step 6C-4, ruling U9): a headless
    /// instance under the lab's desk seam in a development build, under a
    /// private state root ([`crate::desk::Desk::LabHeadless`]), whose log
    /// directory is that root's. Its dead, empty marker is news to the next
    /// lab desk's scan of the same root, so the lab's recovery census reads a
    /// SIGKILLed run as killed and its node log's `prev_end=` is real; to every
    /// other scan it is an `other` marker. The daily driver's directory never
    /// holds one: the seam counts only under a private state root.
    Lab,
}

impl MarkerOwner {
    /// The token in the marker's file name.
    #[cfg(unix)]
    const fn tag(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Other => "other",
            Self::Lab => "lab",
        }
    }

    /// Whether this owner's dead, empty marker is NEWS to a sweep run by a
    /// launch whose own marker is `scan`'s ([`markers::sweep`]): an `app` one
    /// to every sweep (the windowed launch reports it), a `lab` one to a lab
    /// desk's sweep alone, and an `other` one to none. What is news is left in
    /// place for its reader; anything else dead is removed.
    #[cfg(unix)]
    pub(crate) const fn is_news_to(self, scan: MarkerOwner) -> bool {
        match self {
            Self::App => true,
            Self::Lab => matches!(scan, Self::Lab),
            Self::Other => false,
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
    /// As [`MarkerOwner::Other`] until [`adopt_app_identity`]: an installed, windowed handoff
    /// candidate. Its parent rejects a candidate by SIGKILLing its process group (see
    /// `main_entry`'s process-group containment), which is no news; the same process after its
    /// Commit is the daily driver, as every daily driver is after a machine's first update.
    AppAfterCommit,
    /// As [`MarkerOwner::Lab`], for good: a lab desk
    /// ([`crate::desk::Desk::LabHeadless`]), whose launch also runs the
    /// recovery census on its own private root (`recovery_census::cold_census`).
    Lab,
}

impl Arming {
    /// The arming of a launch: `installed_bundle` is the updater's own
    /// classification of the running copy (`which_copy::Running::InstalledApp` —
    /// an owned `.app`, not dev-marked, not on a disk image, not translocated),
    /// `headless` whether the launch arms headless mode, `handoff_candidate`
    /// whether it arrived through a validated update handoff, and `lab_desk`
    /// whether its desk is the lab's (`desk::Desk::of(headless) ==
    /// Desk::LabHeadless`: the seam, read only by a development build, under a
    /// private state root), which arms as [`Arming::Lab`] whatever else holds.
    #[must_use]
    pub(crate) const fn for_launch(
        installed_bundle: bool,
        headless: bool,
        handoff_candidate: bool,
        lab_desk: bool,
    ) -> Self {
        if lab_desk {
            Self::Lab
        } else if !installed_bundle || headless {
            Self::Other
        } else if handoff_candidate {
            Self::AppAfterCommit
        } else {
            Self::App
        }
    }

    /// The owner the marker is created under.
    #[cfg(unix)]
    const fn initial_owner(self) -> MarkerOwner {
        match self {
            Self::App => MarkerOwner::App,
            Self::Other | Self::AppAfterCommit => MarkerOwner::Other,
            Self::Lab => MarkerOwner::Lab,
        }
    }
}

/// This process is about to end cleanly: remove its own marker, so the empty marker of a process
/// that did not end cleanly stays meaningful.
///
/// Async-signal-safe (one `getpid`, one atomic swap, one `unlink(2)` of a path built at install
/// time), because the exits that need it are the `_exit` ones (see [`clean_exit_now`]). Every
/// `exit(3)` (`std::process::exit`, a return from `main`, AppKit's terminate) reaches it through
/// the `atexit` handler the install registers. A no-op in any process but the one that armed the
/// marker (a forked child that calls `exit` must not remove its parent's), and after the first
/// call.
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

/// Where this process's crash marker is (its directory and the start it is named with) while it
/// is still linked: `None` before the install armed one and after the clean exit released it.
/// The PTY keeper is told this at HELLO (`keeper_link`) and, after this window's death, reads the
/// marker's absence as its exit path having run.
#[cfg(unix)]
#[must_use]
pub(crate) fn own_marker_place() -> Option<(std::path::PathBuf, u128)> {
    imp::own_marker_place()
}

/// The handoff candidate this process was is now the daily driver (its Commit arrived): rename
/// its marker as [`MarkerOwner::App`]. A no-op unless the launch armed
/// [`Arming::AppAfterCommit`], and after the first call. `true` exactly when this call is that
/// promotion, which the recovery census records once (`recovery_census`).
#[cfg(unix)]
pub(crate) fn adopt_app_identity() -> bool {
    imp::adopt_app_identity()
}

/// Leave the process with `code` NOW, through `_exit(2)`, after removing this
/// process's marker ([`release_own_marker`]). THE spelling of a CLEAN `_exit` in
/// this crate: a bare `libc::_exit` on a path that is not a crash leaves an empty
/// marker behind, which the next windowed launch of the installed app reports as
/// "aterm was killed".
#[cfg(unix)]
pub(crate) fn clean_exit_now(code: i32) -> ! {
    // `_exit` runs no `exit(3)` handler: the queued `aterm.log` lines land here.
    let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
    release_own_marker();
    // SAFETY: async-signal-safe immediate exit of the calling process. It ends
    // this process and nothing else, runs no handler and no destructor, and
    // cannot return.
    unsafe { libc::_exit(code) }
}

/// Release this launch's crash marker at a clean exit (Windows): close the pre-opened handle and
/// delete the file if it is still empty. A clean run never writes its marker, so without this
/// every clean exit would leave a zero-byte marker for the next launch's `sweep_stale_markers`,
/// which stays the backstop for kills and crashes. A marker with bytes in it is a crash record and
/// is kept.
///
/// Called from the graceful-exit seam after the event loop returns, and from the Windows
/// launch-fatal terminator (`exit_without_process_teardown`). Once the handle is swapped out, a
/// fault still writes its banner to stderr, just not to a file. No-op when no marker was opened,
/// and after the first call.
///
/// A no-op on unix: that lane removes its own marker at every clean exit (`release_own_marker`,
/// from its `atexit` handler and from `clean_exit_now`), and the `atexit` one runs last, after
/// every static destructor, so a crash during `exit` still lands in a linked file.
pub(crate) fn remove_marker_on_clean_exit() {
    #[cfg(windows)]
    imp_windows::release_marker();
}

/// Launch-unique marker path: `crash-signal-<pid>-<nanos>.log`. Windows recycles PIDs
/// aggressively; the startup timestamp keeps a later launch with a crashed instance's PID from
/// truncating its marker. Called at install time.
#[cfg(windows)]
fn marker_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!(
        "crash-signal-{}-{}.log",
        std::process::id(),
        startup_nanos()
    ))
}

/// The one rule shared by the Windows clean-exit release and the next-launch sweep: a marker is a
/// removable clean-run leftover exactly when it is empty; any byte makes it a crash record.
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
    aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
}

/// Delete zero-length `crash-signal-*.log` files left in `dir` (Windows). A clean exit removes
/// its own ([`remove_marker_on_clean_exit`]), but a kill or a crash that wrote nothing cannot, so
/// without this sweep they accumulate. A non-empty marker is a crash record and is kept
/// ([`marker_is_clean_leftover`]).
///
/// A live instance's marker is never swept: its owner holds it open without `FILE_SHARE_DELETE`
/// (`imp_windows::open_marker_file`), so `remove_file` fails with a sharing violation while the
/// owner lives, however it then dies. This is the Windows form of the unix owner lock
/// (`markers`): `DeleteFileW` removes the name at once even while a handle is open, so a sweep
/// would otherwise leave a live instance's fault banner in a file nothing can find.
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

/// The unix marker's lifecycle: named for its owner, locked for its owner's life, removed by its
/// owner's clean exit, so another aterm start can tell a live marker from a dead one (a
/// lock-blind sweep would unlink the live daily driver's marker, leaving its crash banner to an
/// unlinked inode), and a dead empty one from a clean run.
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
///   launch that reports it (`logging::take_kill_evidence`), as a lab desk's
///   sweep leaves a dead [`super::MarkerOwner::Lab`] one for its own census
///   (`logging::take_lab_kill_evidence`; [`super::MarkerOwner::is_news_to`]). A dead marker named
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
            "lab" => MarkerOwner::Lab,
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

    /// Probe `path`'s owner lock without waiting: `open` it read-only, not following a symlink
    /// and not blocking on a FIFO (a file in the log dir is not necessarily one aterm wrote),
    /// then `flock(LOCK_EX|LOCK_NB)`, then close. `flock` binds to the open file description,
    /// so this conflicts even with a lock this process holds through another open.
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
        #[expect(
            clippy::disallowed_methods,
            reason = "a probe: a lock it takes is let go by LOCK_UN below, before the close"
        )]
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        let err = std::io::Error::last_os_error().raw_os_error();
        // A lock the probe took is released by LOCK_UN, not the close alone: `flock` binds to
        // the open file description, and a child this process forks meanwhile (a shell, in the
        // app) holds a copy until it execs, so the next probe of the same dead marker would read
        // it as live. LOCK_UN strips the lock from the description, copies and all.
        if locked {
            // SAFETY: `fd` is the descriptor opened above; LOCK_UN never waits.
            #[expect(
                clippy::disallowed_methods,
                reason = "the release: LOCK_UN before the close"
            )]
            unsafe {
                libc::flock(fd, libc::LOCK_UN)
            };
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

    /// An empty regular file (not a symlink, FIFO or directory): the only shape a clean-run
    /// marker has.
    fn empty_regular_file(entry: &std::fs::DirEntry) -> bool {
        // `DirEntry::metadata` does not traverse a symlink on unix.
        entry
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() == 0)
    }

    /// Remove the markers in `dir` whose owners are gone and whose emptiness is
    /// not news to `scan` (the rules on this module;
    /// [`MarkerOwner::is_news_to`]). `own_pid` is the sweeping process's pid,
    /// `scan` the owner its own marker is created under. Called AT INSTALL
    /// TIME, before this process's own marker exists.
    ///
    /// Answers the dead, empty markers of OTHER processes it removed: each is a
    /// run that ended with no signal and no exit path, which is no row for a
    /// test or dev start but is still the evidence the crash journal needs to
    /// reopen that run's layout (`crate::crash_journal::classify_death`). The
    /// install keeps the answer ([`swept_at_install`]); this process's own
    /// previous image is not in it — a boot re-exec is not a death.
    pub(crate) fn sweep(dir: &Path, own_pid: u32, scan: MarkerOwner) -> Vec<MarkerName> {
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
                let news = marker.owner.is_news_to(scan) && marker.pid != own_pid;
                if !news
                    && probe(&path) == Owner::Dead
                    && std::fs::remove_file(&path).is_ok()
                    && marker.pid != own_pid
                {
                    swept.push(marker);
                }
            } else if legacy_pid(name).is_some_and(|pid| !aterm_uds::process::pid_alive(pid)) {
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
    /// The windowed launch's scan reads them through
    /// `logging::consume_dead_markers`; the tests read them here.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn dead_app_markers(dir: &Path, own_pid: u32) -> Vec<PathBuf> {
        dead_markers_of(dir, own_pid, MarkerOwner::App)
    }

    /// The dead, empty markers of `owner` in `dir` another process left
    /// ([`dead_app_markers`]'s rule, for any owner: a lab desk reads its own,
    /// [`MarkerOwner::Lab`]).
    #[must_use]
    pub(crate) fn dead_markers_of(dir: &Path, own_pid: u32, owner: MarkerOwner) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                name.to_str()
                    .and_then(parse)
                    .is_some_and(|marker| marker.owner == owner && marker.pid != own_pid)
                    && empty_regular_file(entry)
                    && probe(&entry.path()) == Owner::Dead
            })
            .map(|entry| entry.path())
            .collect()
    }

    /// Create `dir/<name>` `0600`, locked before it is visible under that name: created
    /// exclusively as `.<name>.pending` (which no sweep removes while it is locked), `flock`ed,
    /// then renamed into place. Returns the fd (close-on-exec, lock held) and the final path, or
    /// `None` when any step fails; the pending file is then removed and [`arm`] falls back to an
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
        #[expect(
            clippy::disallowed_methods,
            reason = "the marker's lock, held for life; a failed arming lets go by LOCK_UN below"
        )]
        let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !locked || std::fs::rename(&pending, &path).is_err() {
            let _ = std::fs::remove_file(&pending);
            // SAFETY: the descriptor opened above, unlocked (a no-op when the lock was never
            // taken; LOCK_UN never waits) and then closed, exactly once; let go by LOCK_UN, not
            // the close alone, as [`probe`] does.
            #[expect(
                clippy::disallowed_methods,
                reason = "the release: LOCK_UN before the close"
            )]
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
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

    /// Arm this launch's marker in `dir`: [`create_locked`] under `owner`'s name. When that
    /// fails (a filesystem without `flock`, a refused rename, a stale pending name), an unlocked
    /// marker is created directly under its final name, so a native crash still leaves its
    /// banner on disk. That fallback is always named [`MarkerOwner::Other`]: an unlocked marker
    /// cannot tell a live owner from a dead one, so it must never read as "the daily driver was
    /// killed". Without `flock` another start's [`probe`] answers `Unknown` and leaves it alone;
    /// where `flock` works a sweep may remove it while its owner lives. `None` only when neither
    /// file can be created.
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

    /// Pre-built banner halves around the signal number, so the handler does no string work at
    /// signal time. The same line goes to stderr and the marker, so it states only the signal.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal signal ";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// fd of the pre-opened crash-marker file, or `-1` when none was opened
    /// (no writable private dir at install time). `AtomicI32` so the handler
    /// reads it without a lock; written once during `install`.
    static MARKER_FD: AtomicI32 = AtomicI32::new(-1);

    /// Tripped once we have armed the `sigaction` handlers, so a second
    /// `install` call (defensive) does not re-open the marker fd.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// The marker's path as a leaked NUL-terminated string for the clean-exit `unlink`
    /// ([`release_own_marker`]), or null when there is none or it was released. A raw pointer
    /// in an atomic so the release is one swap: async-signal-safe, no lock, no allocation.
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

    /// The `atexit` half of [`release_own_marker`]: every `exit(3)` of this process removes its
    /// marker. Registered first (at `logging::init`, before any library is loaded), so it runs
    /// last: a static destructor that crashes during `exit` still writes its banner into a
    /// linked file the next launch reads.
    extern "C" fn release_at_exit() {
        release_own_marker();
    }

    unsafe extern "C" {
        /// `stdlib.h`: register a handler for `exit(3)`. Declared here, as the
        /// launch-fatal fixture in `lib.rs` declares it, rather than added to
        /// `aterm-libc`: this marker's removal and `aterm.log`'s queue flush
        /// (`logging::flush_at_exit`, registered later, so run first) are the
        /// handlers the product registers, and they run on `exit(3)` only —
        /// every `_exit` path (the launch-fatal law) does both itself, through
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

    /// Maximum decimal digits of a `u32` (`4294967295`).
    const U32_MAX_DIGITS: usize = 10;

    /// Format `value` as decimal ASCII into the tail of `buf`, returning the filled sub-slice.
    /// Allocation-free and async-signal-safe (it only writes into the caller's buffer), so the
    /// signal handler can call it.
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
            // SAFETY: `bytes[off..]` is live for the call and `fd` is non-negative; `write` is
            // async-signal-safe.
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
            // Nothing written or an error: retry only on EINTR, else give up so the signal is
            // still re-raised.
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

    /// The fatal-signal handler, installed without `SA_SIGINFO` (it gets only the signal
    /// number). Async-signal-safe throughout: a stack buffer, the allocation-free decimal
    /// helper, `write(2)`, `sigaction(2)` and `raise(3)`.
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

        // Restore the default disposition for this signal and re-raise it, so the process
        // dies the conventional way (core dump, exit status).
        //
        // SAFETY: `act` is a zeroed `sigaction` with `sa_sigaction = SIG_DFL`; `sigaction` and
        // `raise` are async-signal-safe. Return values are ignored: if restoring fails it still
        // raises, and a handler must not unwind.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &act, std::ptr::null_mut());
            #[expect(
                clippy::disallowed_methods,
                reason = "a real crash: its own fatal signal re-raised with the default action, as if never trapped"
            )]
            libc::raise(sig);
        }
    }

    /// Open this launch's crash marker (`0600`, locked for the process's life: [`super::markers`])
    /// and return its raw fd, or `-1` when there is no log dir. Sweeps dead owners' markers
    /// first. Done at install time: `open` and path work are not signal-safe.
    fn open_marker_fd(owner: super::MarkerOwner) -> i32 {
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        let pid = std::process::id();
        super::markers::keep_swept(super::markers::sweep(&dir, pid, owner));
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
                // SAFETY: the descriptor `arm` returned, unlocked (a no-op
                // for the unlocked fallback; LOCK_UN never waits) and then
                // closed, once — let go by LOCK_UN, not the close alone.
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the release: LOCK_UN before the close"
                )]
                unsafe {
                    libc::flock(armed.fd, libc::LOCK_UN);
                    libc::close(armed.fd);
                }
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
        // Pre-open the marker fd now (`open` is not signal-safe); `-1` means the handler
        // writes to stderr only.
        let fd = open_marker_fd(arming.initial_owner());
        MARKER_FD.store(fd, Ordering::SeqCst);
        if fd >= 0 {
            // SAFETY: registers a plain `extern "C"` function with no captured
            // state; a failure to register only costs the clean-exit removal.
            unsafe { atexit(release_at_exit) };
        }

        for &sig in &FATAL_SIGNALS {
            // SAFETY: `act` is a zeroed `sigaction` with a valid fn pointer in `sa_sigaction`,
            // an empty signal mask and standard flags, passed outside any signal context. The
            // result is ignored: failing to arm one signal must not stop startup.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                // The `sa_sigaction` field is the function-pointer slot; libc
                // types it as `usize`, so cast via a thin pointer (not a direct
                // fn-item-to-int cast, which lints).
                act.sa_sigaction = handle_fatal_signal as *const () as usize;
                // SA_RESTART restarts interrupted syscalls where possible; SA_NODEFER is not
                // set, so the signal stays masked while the handler runs. SA_ONSTACK: a SIGSEGV
                // from a stack overflow has no stack to run a handler on, so without the
                // alternate stack the handler itself faults and the process dies with no banner
                // or marker. Rust's runtime installs a sigaltstack on the main thread and every
                // std::thread, and replacing its SIGSEGV/SIGBUS handler keeps those stacks.
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
    }
}

/// The marker lifecycle's rules, each against real files, real `flock`s and
/// real processes in a scratch dir — never the user's log dir.
#[cfg(all(test, unix))]
mod marker_tests {
    use super::MarkerOwner;
    use super::markers::{self, Owner};
    use crate::crash_journal::tests::{reaped_pid, scratch};
    use std::path::Path;

    /// Let go of a locked marker's descriptor as its owner does: `LOCK_UN`,
    /// then the close — never by the close alone (the flock release rule).
    fn let_go(fd: i32) {
        // SAFETY: `fd` is a descriptor `create_locked`/`arm` returned and the
        // caller still owns; LOCK_UN never waits.
        #[expect(
            clippy::disallowed_methods,
            reason = "the release: LOCK_UN before the close"
        )]
        unsafe {
            libc::flock(fd, libc::LOCK_UN)
        };
        // SAFETY: the same descriptor, closed exactly once.
        unsafe { libc::close(fd) };
    }

    /// An empty marker nobody holds: what a dead owner leaves.
    fn dead_marker(dir: &Path, pid: u32, owner: MarkerOwner) -> std::path::PathBuf {
        let path = dir.join(markers::file_name(pid, 7, owner));
        std::fs::write(&path, b"").unwrap();
        path
    }

    /// The negative control: a lock-blind sweep that removes every empty `crash-*` marker,
    /// owner alive or not.
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
        let lab = markers::file_name(1, 2, MarkerOwner::Lab);
        assert_eq!(lab, "crash-marker-1-2-lab.log");
        assert_eq!(markers::parse(&lab).unwrap().owner, MarkerOwner::Lab);
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

    /// A marker whose owner holds its lock (here through a second open file description in this
    /// process: `flock` conflicts across descriptions) survives the sweep; the dead one beside it
    /// goes. Control: the lock-blind sweep removes both.
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

        markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
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
        let_go(fd);
    }

    /// `arm` takes the locked lifecycle when it can. When it cannot (here a stale pending entry
    /// the exclusive create cannot replace, standing in for a filesystem without `flock` or a
    /// refused rename), the launch still gets a marker, so a native crash still leaves its
    /// banner; it is unlocked and so named `other` even for an `app` launch, never evidence of a
    /// killed daily driver. Control: the locked arming alone leaves no marker in that state.
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
        let_go(armed.fd);
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

    /// A dead `app` marker is news: the sweep leaves it for the windowed launch, which finds it.
    /// A dead `other` marker, a live `app` one, and this process's own previous image's `app`
    /// marker (same pid: a boot re-exec) are not.
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
        markers::sweep(dir.path(), own, MarkerOwner::Other);
        assert!(killed.exists(), "left for the report");
        assert!(!test_start.exists(), "a test start's corpse is not news");
        assert!(
            !previous_image.exists(),
            "this process's own previous image"
        );
        assert!(live_app.exists());
        // A non-empty marker is a crash record: never swept, never "killed".
        std::fs::write(&killed, b"aterm: fatal signal 11").unwrap();
        markers::sweep(dir.path(), own, MarkerOwner::Other);
        assert!(killed.exists());
        assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        let_go(fd);
    }

    /// A LAB DESK'S MARKER IS NEWS TO A LAB DESK'S SWEEP ALONE (session log
    /// Phase 6, step 6C-4, ruling U9): under a lab desk's sweep a dead `lab`
    /// marker of another process is left for that desk's census (and its own
    /// kill scan finds it), while a dead `other` one beside it goes and this
    /// process's own previous image's `lab` one goes too; under the installed
    /// app's sweep, or any other start's, the same `lab` marker is not news and
    /// goes, and no windowed launch's kill scan ever reports one. NEGATIVE
    /// CONTROL (scratch revert): a `lab` marker read as an `other` one is
    /// swept under the lab desk's sweep too.
    #[test]
    fn a_lab_marker_is_news_to_a_lab_desks_sweep_alone() {
        for scan in [MarkerOwner::Lab, MarkerOwner::App, MarkerOwner::Other] {
            let dir = scratch("marker-lab");
            let own = std::process::id();
            let gone = reaped_pid();
            let lab = dead_marker(dir.path(), gone, MarkerOwner::Lab);
            let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
            let previous_image = dead_marker(dir.path(), own, MarkerOwner::Lab);
            assert!(markers::dead_app_markers(dir.path(), own).is_empty());
            markers::sweep(dir.path(), own, scan);
            let news = scan == MarkerOwner::Lab;
            assert_eq!(
                lab.exists(),
                news,
                "a dead lab marker under a {scan:?} sweep"
            );
            assert!(!test_start.exists(), "a test start's corpse is never news");
            assert!(
                !previous_image.exists(),
                "this process's own previous image"
            );
            assert_eq!(
                markers::dead_markers_of(dir.path(), own, MarkerOwner::Lab),
                if news { vec![lab.clone()] } else { Vec::new() }
            );
            assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        }
    }

    /// The sweep answers the dead starts it removed: a dead `other` start's empty marker is no
    /// row, but it is the evidence the crash journal reopens that start's layout by, and the
    /// sweep runs before the claim. This process's own previous image, a live marker and a crash
    /// record are not in the answer. Control: the named marker is gone from the directory, so a
    /// claim that read only the directory would find no evidence.
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

        let swept = markers::sweep(dir.path(), own, MarkerOwner::Other);
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
        let_go(fd);
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

        markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
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
        // assumed; `flock(1)` is not on macOS), so this uses a perl one-liner,
        // and a host without perl COULD NOT RUN it.
        let Ok(perl) = aterm_uds::spawnfd::command("/usr/bin/perl")
            .arg("-e")
            .arg(
                "use Fcntl qw(:flock); open(my $f, '>', $ARGV[0]) or die; \
                 flock($f, LOCK_EX|LOCK_NB) or die; $|=1; print \"locked\\n\"; sleep 60",
            )
            .arg(dir.path().join(markers::file_name(1, 1, MarkerOwner::App)))
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
        else {
            aterm_spec::host::could_not_run("/usr/bin/perl is not available to hold the lock");
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

/// The Windows crash lane: an unhandled-exception filter (`SetUnhandledExceptionFilter`) for
/// native fatal exceptions (access violation `0xC0000005`, illegal instruction, stack overflow),
/// which bypass the panic hook as POSIX fatal signals do. Path work, `open` and allocation happen
/// once at install time; the filter allocates nothing, since it may run on a corrupted heap.
/// Rust's aborts (`__fastfail`) never reach it: see [`super::install_signal_handlers_as`].
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

    /// `EXCEPTION_RECORD` with its documented fixed layout: `#[repr(C)]` with the native field
    /// types reproduces the OS struct, including the padding before `exception_information` on
    /// 64-bit.
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

    /// Pre-built banner fragments around the hex fields, so the filter does no string work:
    /// `PREFIX <code> AT <pc> [AV_OP <op> AV_AT <addr>] SUFFIX`. The same line goes to stderr and
    /// the marker, so it states only the exception.
    const BANNER_PREFIX: &[u8] = b"aterm: fatal exception 0x";
    const BANNER_AT: &[u8] = b" at 0x";
    const BANNER_AV_OP: &[u8] = " \u{2014} access violation ".as_bytes();
    const BANNER_AV_AT: &[u8] = b" @ 0x";
    const BANNER_SUFFIX: &[u8] = b"\n";

    /// Raw HANDLE of the pre-opened crash-marker file, or `-1` when none was opened. `AtomicIsize`
    /// so the filter reads it without a lock; stored once by `install`, swapped back to `-1` by
    /// `release_marker` at a clean exit. `pub(super)` for the shared clean-exit test.
    pub(super) static MARKER_HANDLE: AtomicIsize = AtomicIsize::new(-1);

    /// This launch's marker path, parked by `open_marker_handle` when the open succeeded, so
    /// [`release_marker`] can find the file (the nanos salt in its name cannot be recomputed).
    /// `pub(super)` for the shared clean-exit test.
    pub(super) static MARKER_PATH: std::sync::OnceLock<std::path::PathBuf> =
        std::sync::OnceLock::new();

    /// Tripped once the filter is registered, so a second `install` call
    /// (defensive) does not re-open the marker handle.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Format `value` as exactly 8 uppercase hex digits into `buf`, returning the filled slice.
    /// Allocation-free, so the filter can call it; fixed width because NTSTATUS codes read as 8
    /// digits (`0xC0000005`).
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

    /// Allocation-free `WriteFile` loop over short writes. Errors are swallowed: the filter must
    /// still return so the default termination proceeds.
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

    /// The unhandled-exception filter, allocation-free: a stack hex buffer, the pre-built banner
    /// bytes, and `WriteFile` loops to stderr and the marker. Returns `EXCEPTION_CONTINUE_SEARCH`
    /// so the default WER/termination path still runs.
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

    /// Open a launch-unique crash-marker file and return its raw HANDLE, or `-1` when no log dir
    /// is available. Sweeps stale empty markers first. Done at install time: path work and
    /// `open` must not happen inside the filter.
    fn open_marker_handle() -> isize {
        use std::os::windows::io::IntoRawHandle;
        let Some(dir) = crate::logging::log_dir() else {
            return -1;
        };
        super::sweep_stale_markers(&dir);
        let path = super::marker_path(&dir);
        match open_marker_file(&path) {
            // Leaked into a raw handle: the marker stays open for the filter. The OS reclaims it
            // at exit, or `release_marker` (by the parked path) at a clean one.
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

    /// Create (truncating) the marker at `path`, held against deletion while the returned handle
    /// is open. `std` opens with `FILE_SHARE_DELETE`, which would let another launch's
    /// [`super::sweep_stale_markers`] delete a live marker. Without it every delete or rename
    /// fails with a sharing violation (error 32) until the handle closes, and the kernel closes
    /// it when the process dies, however it dies: an owner lock that needs nothing from a
    /// crashing process. Read and write sharing stay granted, so the crash scan can still read it.
    pub(super) fn open_marker_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .share_mode(MARKER_SHARE_MODE)
            .open(path)
    }

    /// Clean-exit release ([`super::remove_marker_on_clean_exit`]): swap the handle out first, so
    /// a racing fault writes to stderr only; close it; then remove the file if still empty. The
    /// close must come first: the handle does not share delete ([`open_marker_file`]), so a
    /// delete while it is open fails even from this process. Idempotent.
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
        // SAFETY: registers a plain `extern "system"` fn with the documented signature. The
        // previous filter is dropped on purpose: this is the outermost crash reporter, and it
        // returns CONTINUE_SEARCH so the default handling still runs.
        unsafe {
            SetUnhandledExceptionFilter(handle_fatal_exception);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Render `value` via the allocation-free hex helper.
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

        /// A marker opened by [`open_marker_file`] survives a sibling's sweep and any delete
        /// while its handle is open, and is an ordinary empty leftover once released. Control:
        /// `std`'s default share mode lets the sweep take a live marker.
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
/// lock-order census (`aterm-census --locks`, OB-7) sees a hold only where
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

    /// The seam runs on every clean exit, armed or not, and may run twice: afterwards nothing is
    /// held on Windows, and on unix it is a harmless no-op.
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

    /// The clean exit end to end, armed for real in the real log dir: while armed the marker is
    /// held against deletion, and the release closes the handle and removes the file. The no-op
    /// test may already have released the process's one marker (`install` never re-opens it);
    /// the end state holds either way.
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
