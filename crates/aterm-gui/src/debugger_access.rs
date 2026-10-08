// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S DEBUGGER-ACCESS CHANGE (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
//! §3.4, §3.5, §5.1; owner rulings O7 and O8, 2026-09-29).
//!
//! The owner presses *Allow debugger access…* (or *Turn off debugger access…*)
//! on Settings ▸ Security. aterm's own `confirm_owner` alert states the
//! whole-Mac scope and defaults to Cancel. On a yes the Settings reducer
//! records the change on its view; the park drains it into
//! `App::begin_debugger_access_change`, which re-checks its gates and starts
//! ONE detached `consent-devtools` worker through the injected
//! `ConsentGestures::run_devtools` arm. That worker takes the Mac-wide change
//! lock ([`CHANGE_LOCK`]), re-reads `DevToolsSecurity -status` and runs
//! `-enable` / `-disable` UNPRIVILEGED only if the read still shows the
//! change's precondition (`aterm_containment::consent::run_devtools_change`).
//! macOS should then ask for an administrator in its own dialog — what it
//! shows for a child of a GUI app is spike D2's to measure — and aterm never
//! sees or answers it. Then aterm re-probes and reports only what it reads.
//!
//! This module holds the parts that decide, so every one is testable without a
//! window, a process or a dialog:
//!
//! * [`DebuggerAccessFlow`] — the pure reducer the Tier-1 bind drives against
//!   the derived `DebuggerAccessChange` model (see `conformance` below).
//! * [`ChangeSlot`] — the shared result slot the worker publishes into. The
//!   result lives HERE, never in the wake: an uncommitted incoming handoff drops
//!   every wake but Commit (the 2026-09-10 swallowed-verdict shape), so the park
//!   re-reads the slot every [`RESULT_RECHECK`] while a change is under way.
//! * [`DriftWatch`] — the `privacy-debugger-observed` record and the drift
//!   record (§3.5). It never re-enables anything.
//! * [`DebuggerChangeLane`] — the `App`-owned bundle of the three, with the
//!   predecessor's `privacy-debugger-change` marker (§3.4 step 6).
//! * [`take_change_lock`] — the Mac-wide change lock, so no two aterms sharing
//!   a config (a successor, or a development build beside the installed app,
//!   which O8 lets offer the gesture too) ever run two change children.
//! * [`DebuggerFacts`] — what the Security page renders.
//!
//! What it never does: answer SecurityAgent, retry a change the re-probe reads
//! unchanged, re-enable after drift, latch "allowed" from an exit code, or start
//! a change from anywhere but the owner's confirmed press. There is no control
//! verb, config key, environment variable or band capsule for it, and the two
//! action ids sit under `MACOS_ACCESS_GESTURE_PREFIX`, which `app act` refuses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DevtoolsChangeRun, DevtoolsChild, ProbeGate, ProcessBirth, SelfHardening,
};

/// How often the park re-reads the shared result slot while a change is under
/// way (design §3.4 step 4). The worker's wake is a nudge only; this re-read is
/// what makes an exited change ALWAYS read (the model's `WF(Notice)`).
pub(crate) const RESULT_RECHECK: Duration = Duration::from_secs(5);

/// The marker beside `aterm.toml` naming a change child in flight: its
/// direction, pid, birth and when it started (design §3.4 step 3.3). A
/// successor that finds its child still running keeps the button disabled.
pub(crate) const CHANGE_MARKER: &str = "privacy-debugger-change";

/// The marker beside `aterm.toml` holding the last observed mode, when aterm
/// first saw it, and who set it (design §3.5).
pub(crate) const OBSERVED_MARKER: &str = "privacy-debugger-observed";

/// THE MAC-WIDE CHANGE LOCK beside `aterm.toml`. Every aterm sharing the
/// config — a successor mid-handoff, or a second windowed aterm such as a
/// development build beside the installed app (O8) — takes it before its
/// worker re-reads `-status`, holds it until the child exits, and writes or
/// removes [`CHANGE_MARKER`] only while holding it. `flock` binds to the open
/// file description, so a second worker in THIS process is refused too, and
/// the kernel drops the lock when its holder dies: a crash never wedges it.
pub(crate) const CHANGE_LOCK: &str = "privacy-debugger-change.lock";

/// Both markers hold one short line; anything bigger is not a record this
/// build wrote and reads as nothing.
pub(crate) const MAX_MARKER_BYTES: usize = 96;

/// Wall-clock seconds since the epoch: the markers' and the page's clock.
/// `Instant` stops while the Mac sleeps, and a change can wait out a night.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// The markers
// ---------------------------------------------------------------------------

/// The `privacy-debugger-change` record: `<on|off> <pid> <secs>.<usecs> <since>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChangeMarker {
    pub(crate) change: DebuggerChange,
    pub(crate) child: DevtoolsChild,
    /// When the child started, in wall seconds.
    pub(crate) since: u64,
}

impl ChangeMarker {
    /// A record for `child`, or `None` when its birth could not be read at
    /// spawn: a successor could never prove such a child running, so a marker
    /// naming it would only ever read as stale.
    pub(crate) fn new(change: DebuggerChange, child: DevtoolsChild, since: u64) -> Option<Self> {
        child.birth.map(|_| Self {
            change,
            child,
            since,
        })
    }

    pub(crate) fn encode(&self) -> String {
        let birth = self.child.birth.unwrap_or(ProcessBirth {
            seconds: 0,
            microseconds: 0,
        });
        format!(
            "{} {} {}.{:06} {}",
            self.change.as_str(),
            self.child.pid,
            birth.seconds,
            birth.microseconds,
            self.since
        )
    }

    /// Strict: exactly the shape [`Self::encode`] writes, or nothing.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let change = DebuggerChange::from_token(words.next()?)?;
        let pid = words.next()?.parse::<u32>().ok()?;
        let (seconds, microseconds) = words.next()?.split_once('.')?;
        let birth = ProcessBirth {
            seconds: seconds.parse().ok()?,
            microseconds: microseconds.parse().ok()?,
        };
        let since = words.next()?.parse().ok()?;
        if words.next().is_some() || pid == 0 {
            return None;
        }
        Some(Self {
            change,
            child: DevtoolsChild {
                pid,
                birth: Some(birth),
            },
            since,
        })
    }
}

/// Who set the mode an [`ObservedMarker`] records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The owner's change from aterm, observed by its re-probe.
    Aterm,
    /// A transition aterm observed with no change of its own under way.
    Outside,
    /// First sight: aterm did not see it change.
    Unknown,
}

impl Origin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Outside => "outside",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "aterm" => Some(Self::Aterm),
            "outside" => Some(Self::Outside),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// The `privacy-debugger-observed` record: `<enabled|disabled> <since> <origin>`.
/// Only the two modes a change moves between are recorded; `unrecognized` and
/// `unknown` are not drift and write nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservedMarker {
    pub(crate) mode: DebuggerMode,
    /// When aterm first observed this mode, in wall seconds.
    pub(crate) since: u64,
    pub(crate) origin: Origin,
}

impl ObservedMarker {
    pub(crate) fn encode(&self) -> String {
        format!(
            "{} {} {}",
            self.mode.as_str(),
            self.since,
            self.origin.as_str()
        )
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let mode = match words.next()? {
            "enabled" => DebuggerMode::Enabled,
            "disabled" => DebuggerMode::Disabled,
            _ => return None,
        };
        let since = words.next()?.parse().ok()?;
        let origin = Origin::from_token(words.next()?)?;
        if words.next().is_some() {
            return None;
        }
        Some(Self {
            mode,
            since,
            origin,
        })
    }
}

fn marker_path(config_path: &Path, name: &str) -> Option<PathBuf> {
    config_path.parent().map(|dir| dir.join(name))
}

/// Every debugger marker write in this process holds this, so the worker's
/// change marker and the park's observed marker never interleave with a
/// removal of the same file.
static DEBUGGER_MARKS: Mutex<()> = Mutex::new(());

fn write_debugger_marker(config_path: &Path, name: &str, text: &str) -> std::io::Result<()> {
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path, name) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record in",
        ));
    };
    crate::config_marker::write_marker(&path, text, MAX_MARKER_BYTES)
}

fn remove_debugger_marker(config_path: &Path, name: &str) -> std::io::Result<()> {
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    marker_path(config_path, name).map_or(Ok(()), |path| crate::config_marker::remove_marker(&path))
}

fn read_debugger_marker(config_path: &Path, name: &str) -> Option<String> {
    marker_path(config_path, name)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_MARKER_BYTES))
}

/// The held change lock, released by `LOCK_UN` when this drops
/// ([`HeldAdvisoryLock`]) — not by the close alone — or by the kernel when its
/// process dies. A child any thread of this process forks while the lock is
/// held (a new tab's shell; in the test suite, every sibling's spawn) holds a
/// copy of its description until that child execs: released by the close
/// alone, the lock stayed taken in that copy, and the park's marker removal,
/// the records reader or the owner's next press read a lock nobody held as a
/// peer's change. `LOCK_UN` releases the description itself, the child's copy
/// included, so a lock dropped here is free at once.
///
/// [`HeldAdvisoryLock`]: crate::native_document_host::HeldAdvisoryLock
#[derive(Debug)]
pub(crate) struct ChangeLock {
    _held: crate::native_document_host::HeldAdvisoryLock,
}

/// What [`take_change_lock`] found.
#[derive(Debug)]
pub(crate) enum LockTry {
    /// This caller holds it now.
    Held(ChangeLock),
    /// Another worker — in this process or another aterm — holds it.
    Busy,
    /// The lock file could not be opened (no config directory, a planted link,
    /// an unwritable folder): fail closed.
    Unavailable(std::io::Error),
}

/// [`take_change_lock`] in ONE try, never waiting: what a test reads a lock
/// released by `LOCK_UN` with.
#[cfg(test)]
pub(crate) fn try_change_lock(config_path: &Path) -> LockTry {
    take_change_lock(config_path, Duration::ZERO)
}

/// How long [`take_change_lock`] waits out a busy lock. Nothing of this
/// process's own is waited for: a [`ChangeLock`] here lets go by `LOCK_UN`,
/// so it is free the moment it drops, whatever this process forked while it
/// was held. What a second rides out is a SHORT hold, here or in a peer aterm
/// — [`settle_markers`] holds it while it writes the observed record, reads
/// the change marker and unlinks it; the records reader for a read and at
/// most an unlink — and a peer's copy no `LOCK_UN` of ours can reach: a child
/// forked by a peer that died, or by a build that let the lock go by the
/// close, holds it until that child execs. A real change holder — a worker
/// whose child waits on macOS's administrator dialog — keeps it for seconds
/// to hours, and reads `Busy` once this is spent.
pub(crate) const CHANGE_LOCK_PATIENCE: Duration = Duration::from_secs(1);

/// Take the change lock beside `config_path`, waiting out a busy lock for up
/// to `patience` through the crate's one advisory-lock retry
/// ([`acquire_advisory_lock_within`]: the first try before any sleep, then a
/// backoff that never overshoots `patience`). Worker threads only: it sleeps.
/// The file is opened owner-only and never through a final-component link.
///
/// [`acquire_advisory_lock_within`]: crate::native_document_host::acquire_advisory_lock_within
pub(crate) fn take_change_lock(config_path: &Path, patience: Duration) -> LockTry {
    let Some(path) = marker_path(config_path, CHANGE_LOCK) else {
        return LockTry::Unavailable(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to lock in",
        ));
    };
    #[cfg(unix)]
    {
        use crate::native_document_host::{AdvisoryLockRefusal, acquire_advisory_lock_within};
        use std::os::unix::fs::OpenOptionsExt as _;
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            Err(error) => return LockTry::Unavailable(error),
        };
        match acquire_advisory_lock_within(file, patience) {
            Ok(held) => LockTry::Held(ChangeLock { _held: held }),
            Err(AdvisoryLockRefusal::Busy) => LockTry::Busy,
            Err(AdvisoryLockRefusal::Failed(error)) => LockTry::Unavailable(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, patience);
        LockTry::Unavailable(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "debugger access is a macOS setting",
        ))
    }
}

/// Write the observed record, THEN let go of an ended change's marker — in
/// that order, under the change lock, and the marker only if it still names
/// `release`'s child. A peer that reads the records in between finds the
/// marker (its child dead) and re-reads the change as aterm's; one that finds
/// no marker reads the record it left — the records reader reads the change
/// marker first and the observed record after. A lock someone else holds past
/// its patience means a newer change may be writing its own marker: that one
/// is left alone, and the record is written regardless.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "PeerSettle",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
pub(crate) fn settle_markers(
    config_path: &Path,
    observed: Option<ObservedMarker>,
    release: Option<&DevtoolsChild>,
) {
    let lock = release.map(|_| take_change_lock(config_path, CHANGE_LOCK_PATIENCE));
    if let Some(marker) = observed
        && let Err(error) = write_debugger_marker(config_path, OBSERVED_MARKER, &marker.encode())
    {
        // Only the next launch's tell between drift and first sight is lost.
        aterm_log::warn!("debugger access: observed mode not recorded: {error}");
    }
    let Some(child) = release else {
        return;
    };
    match lock {
        Some(LockTry::Held(_lock)) => {
            let names_it = read_debugger_marker(config_path, CHANGE_MARKER)
                .and_then(|text| ChangeMarker::parse(&text))
                .is_some_and(|marker| marker.child == *child);
            if names_it && let Err(error) = remove_debugger_marker(config_path, CHANGE_MARKER) {
                aterm_log::warn!("debugger access: change marker not removed: {error}");
            }
        }
        Some(LockTry::Unavailable(error)) => {
            aterm_log::warn!("debugger access: change marker not removed: {error}");
        }
        Some(LockTry::Busy) | None => {}
    }
}

/// [`settle_markers`] off the UI thread (two tiny files, still disk I/O, and
/// a lock it may wait a second for).
pub(crate) fn settle_markers_detached(
    config_path: PathBuf,
    observed: Option<ObservedMarker>,
    release: Option<DevtoolsChild>,
) {
    let spawned = std::thread::Builder::new()
        .name("aterm-debugger-marker".into())
        .spawn(move || {
            // Background: it takes no lock the UI thread contends (only this
            // module's own marker mutex and the change lock).
            crate::qos::set_self(crate::qos::Role::Background);
            settle_markers(&config_path, observed, release.as_ref());
        });
    if let Err(error) = spawned {
        aterm_log::warn!("debugger access: markers not settled: {error}");
    }
}

/// What the records reader found beside `aterm.toml`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecordsRead {
    /// A predecessor's change child, still running.
    pub(crate) inherited: Option<ChangeMarker>,
    /// A change marker naming a child that is gone: its change was never
    /// judged (aterm quit while the child ran or before its re-probe, a frozen
    /// predecessor's worker finished before this process read the marker, or
    /// a peer aterm sits between taking its result and recording what its
    /// re-probe read). The reader leaves it on disk: the lane re-reads the
    /// change as this aterm's own, so the re-probe that follows is attributed
    /// to aterm rather than filed as a change made outside it, and removes the
    /// marker once that re-probe is recorded ([`DebuggerChangeLane::poll`]).
    pub(crate) stale: Option<ChangeMarker>,
    /// The last observed mode.
    pub(crate) observed: Option<ObservedMarker>,
    /// No change marker, but the change lock was held past its patience: a
    /// worker sits between its fresh `-status` re-read and its spawn (it
    /// writes its marker only once the child starts — a predecessor's mid
    /// handoff, or a peer's), or holds a child whose birth it could not
    /// record. "Nothing in flight" is conclusive only under the lock, so this
    /// read settles nothing and the lane reads again after [`RESULT_RECHECK`].
    pub(crate) unsettled: bool,
}

/// Read both markers, off the UI thread, and judge a change marker's child by
/// pid AND birth (`alive`): a pid alone may be reused. A stale change marker
/// is reported and left for the lane ([`RecordsRead::stale`]); only a marker
/// this build cannot parse is removed here, under the change lock and only if
/// it still reads the same. No marker means nothing is in flight only under
/// the lock: every worker holds it from its re-read on and writes its marker
/// while holding it, so a lock held with no marker yet is
/// [`RecordsRead::unsettled`].
///
/// The change marker is read BEFORE the observed record: an aterm ending a
/// change writes its record before it removes its marker ([`settle_markers`]),
/// so a read that finds no marker finds the record that change left.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "Adopt",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "Follow",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
pub(crate) fn read_records(config_path: &Path, alive: fn(&DevtoolsChild) -> bool) -> RecordsRead {
    let mut read = RecordsRead::default();
    let change = match read_debugger_marker(config_path, CHANGE_MARKER) {
        Some(text) => Some(text),
        // No lock file: no worker ever took it here, so none holds it now —
        // and a launch does not leave one beside `aterm.toml`.
        None if !marker_path(config_path, CHANGE_LOCK)
            .is_some_and(|path| path.symlink_metadata().is_ok()) =>
        {
            None
        }
        None => match take_change_lock(config_path, CHANGE_LOCK_PATIENCE) {
            // A marker written between the read above and the lock was written
            // by a worker that has let go since: judge it like any other.
            LockTry::Held(_lock) => read_debugger_marker(config_path, CHANGE_MARKER),
            LockTry::Busy => {
                read.unsettled = true;
                return read;
            }
            // No lock to read under is no lock a worker could take either: it
            // refuses to start without one (`WorkerRefusal::NoLock`).
            LockTry::Unavailable(_) => None,
        },
    };
    if let Some(text) = change {
        match ChangeMarker::parse(&text) {
            Some(marker) if alive(&marker.child) => read.inherited = Some(marker),
            Some(marker) => read.stale = Some(marker),
            None => match take_change_lock(config_path, CHANGE_LOCK_PATIENCE) {
                LockTry::Held(_lock) => {
                    if read_debugger_marker(config_path, CHANGE_MARKER).as_deref()
                        == Some(text.as_str())
                        && let Err(error) = remove_debugger_marker(config_path, CHANGE_MARKER)
                    {
                        aterm_log::warn!(
                            "debugger access: unreadable change marker not removed: {error}"
                        );
                    }
                }
                // A newer change holds the lock and overwrites it.
                LockTry::Busy => {}
                LockTry::Unavailable(error) => {
                    aterm_log::warn!(
                        "debugger access: unreadable change marker not removed: {error}"
                    );
                }
            },
        }
    }
    read.observed = read_debugger_marker(config_path, OBSERVED_MARKER)
        .and_then(|text| ObservedMarker::parse(&text));
    read
}

/// Start the one records reader. `false` when its thread could not start (the
/// park then settles on "no records" rather than waiting forever).
pub(crate) fn spawn_records_reader(
    config_path: PathBuf,
    into: Arc<Mutex<Option<RecordsRead>>>,
    alive: fn(&DevtoolsChild) -> bool,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> bool {
    std::thread::Builder::new()
        .name("aterm-debugger-records".into())
        .spawn(move || {
            // Responsive, not Background: it publishes under a mutex the UI
            // thread's park takes, the documented floor for such a holder. It
            // reads two tiny files and is gone.
            crate::qos::set_self(crate::qos::Role::Responsive);
            let read = read_records(&config_path, alive);
            *into.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
            wake();
        })
        .is_ok()
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

/// Where the owner's change stands. The numbers are the model's `phase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing under way (0).
    Idle,
    /// aterm's alert is up for this change (1).
    Confirming(DebuggerChange),
    /// The owner said yes; the worker is re-reading `-status` (2). The model's
    /// phase 3 (the re-read showed the precondition) lives inside the worker,
    /// between the re-read and the spawn.
    Rechecking(DebuggerChange),
    /// The child is in flight (4): macOS's dialog may be up, for hours.
    /// `inherited` is a predecessor's child, read from its marker.
    InFlight {
        change: DebuggerChange,
        child: DevtoolsChild,
        since: u64,
        inherited: bool,
    },
    /// The child exited, or its result was lost (5): only a fresh probe says
    /// what the rule is now.
    Exited(DebuggerChange),
}

/// What a change came to. Only the re-probe decides it; an exit code never
/// does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The re-probe reads the change's target.
    Changed(DebuggerChange),
    /// The re-probe reads the precondition still: Cancel in macOS's dialog, a
    /// wrong password, or a write that failed. Counted as a decline, never
    /// retried.
    NotChanged(DebuggerChange),
    /// The re-probe reads a rule Apple's tool does not recognise.
    Unrecognized(DebuggerChange),
    /// The re-probe gave no answer (refused, failed, timed out).
    Unobserved(DebuggerChange),
    /// The worker's fresh re-read no longer showed the precondition, so
    /// nothing was spawned; it read this.
    NotDue(DebuggerChange, DebuggerMode),
    /// No child was started: the tool is absent, could not be spawned, the
    /// change lock could not be opened, or the worker ended before it got that
    /// far.
    NotStarted(DebuggerChange),
    /// Another aterm's change holds the change lock: nothing was read or
    /// spawned here.
    PeerInFlight(DebuggerChange),
}

impl Outcome {
    /// Whether this outcome counts as the owner's decline for `OwnerQuiet`.
    pub(crate) const fn counts_as_decline(self) -> bool {
        matches!(self, Self::NotChanged(_))
    }
}

/// Why a press, or the host's start of a confirmed one, did nothing — in the
/// owner's words, for "Nothing was changed: {why}.".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    NoWindow,
    NotMacos,
    PrivacyOff,
    /// A predecessor may have a change in flight, and its marker is unread.
    RecordsUnread,
    /// A change child is in flight (this process's or a predecessor's).
    InFlight,
    /// The owner did not say yes in aterm's alert.
    NotConfirmed,
    /// The alert is up, the worker is re-reading, or the last change's result
    /// has not been re-read yet.
    Busy,
    /// The cached probe does not show the change's precondition.
    State(DebuggerChange, DebuggerMode),
    /// Its worker could not start.
    CouldNotStart,
}

impl Refusal {
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::NoWindow => "this instance has no window to ask the owner in",
            Self::NotMacos => "debugger access is a macOS setting",
            Self::PrivacyOff => "aterm's privacy checks are switched off in aterm.toml",
            Self::RecordsUnread => "aterm has not read its record of an earlier change yet",
            Self::InFlight => "a change is already under way and DevToolsSecurity has not finished",
            Self::NotConfirmed => "the owner did not confirm it in aterm's alert",
            Self::Busy => "aterm is still reading the result of the last change",
            Self::State(_, DebuggerMode::Unknown) => "aterm has not read the debugger rule yet",
            Self::State(_, DebuggerMode::Unrecognized) => {
                "macOS reports the rule in a shape aterm does not recognise, and aterm leaves it alone"
            }
            Self::State(DebuggerChange::On, _) => "debugger access is already allowed",
            Self::State(DebuggerChange::Off, _) => "debugger access is already off",
            Self::CouldNotStart => "its worker could not start",
        }
    }
}

/// How the owner answered aterm's alert, as the reducer took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// No alert was up: nothing changed.
    NotAsked,
    /// Cancel, or nobody could be asked: nothing is spawned.
    Declined,
    /// "Allow…" / "Turn Off…": the worker may re-read and spawn.
    Consented,
}

/// THE REDUCER. Pure: it performs nothing, reads no clock and asks no OS; the
/// host feeds it what happened and does what it allows. Its refusals are the
/// model's guards, checked step by step by the Tier-1 bind.
///
/// O8 (owner, 2026-09-29): nothing here reads aterm's own hardening. An
/// unhardened aterm offers the gesture too; the alert says what that exposes.
#[derive(Clone, Debug)]
pub(crate) struct DebuggerAccessFlow {
    phase: Phase,
    /// Whether a predecessor's change marker has been read (or there was
    /// nothing to read). Until then a press is refused: the predecessor's
    /// child may still have macOS's dialog up.
    records_read: bool,
    /// The last completed probe this reducer was handed. `Unknown` is "not
    /// read". What a surface says comes from here, never from an exit code.
    observed: DebuggerMode,
    /// The change the projection normalises by: the last one pressed.
    direction: DebuggerChange,
    /// The owner's "Allow…" answers in this process.
    consents: u32,
    /// Change children started in this process.
    launches: u32,
    last: Option<Outcome>,
}

impl Default for DebuggerAccessFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl DebuggerAccessFlow {
    pub(crate) const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            records_read: false,
            observed: DebuggerMode::Unknown,
            direction: DebuggerChange::On,
            consents: 0,
            launches: 0,
            last: None,
        }
    }

    pub(crate) const fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(test)]
    pub(crate) const fn observed(&self) -> DebuggerMode {
        self.observed
    }

    #[cfg(test)]
    pub(crate) const fn direction(&self) -> DebuggerChange {
        self.direction
    }

    pub(crate) const fn last(&self) -> Option<Outcome> {
        self.last
    }

    pub(crate) const fn records_read(&self) -> bool {
        self.records_read
    }

    /// A surface may say the change's target holds: only when a probe read it.
    /// (The page reads the probe itself; this is the model's `claim`.)
    #[cfg(test)]
    pub(crate) fn claim(&self) -> bool {
        self.observed == self.direction.target()
    }

    /// Anything under way, or the predecessor's records unread: the buttons
    /// are disabled.
    pub(crate) fn busy(&self) -> bool {
        !self.records_read || self.phase != Phase::Idle
    }

    /// The change child in flight, if any.
    pub(crate) const fn in_flight(&self) -> Option<(DebuggerChange, DevtoolsChild, u64, bool)> {
        match self.phase {
            Phase::InFlight {
                change,
                child,
                since,
                inherited,
            } => Some((change, child, since, inherited)),
            _ => None,
        }
    }

    /// The shared records were read — at launch, or again mid-session.
    /// `inherited` is a change child proven running by pid and birth (a
    /// predecessor's, or a peer aterm's): the change is in flight here too,
    /// and no press can start a second child. `stale` is a change whose child
    /// is gone and whose re-probe nobody has recorded — its result unread (a
    /// predecessor that quit), or taken by a peer that has yet to record what
    /// its re-probe read: only a fresh probe says what it did, and that probe
    /// ends it as this aterm's change. `true` when `stale` was taken so; a
    /// flow with something under way takes neither.
    pub(crate) fn on_records(
        &mut self,
        inherited: Option<ChangeMarker>,
        stale: Option<DebuggerChange>,
    ) -> bool {
        self.records_read = true;
        if self.phase != Phase::Idle {
            return false;
        }
        if let Some(marker) = inherited {
            self.direction = marker.change;
            self.phase = Phase::InFlight {
                change: marker.change,
                child: marker.child,
                since: marker.since,
                inherited: true,
            };
        } else if let Some(change) = stale {
            self.direction = change;
            self.phase = Phase::Exited(change);
            return true;
        }
        false
    }

    /// Whether a probe is taken now: idle, or after the child exited (the
    /// model's `Probe` guard). While the alert is up, the worker re-reads or
    /// the child is in flight, the page's cached reads are not this flow's.
    pub(crate) fn accepts_probe(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Exited(_))
    }

    /// A completed probe. After an exit it ends the change and says what it
    /// came to; idle, it only refreshes what the next press is checked
    /// against. A pending probe is no probe.
    pub(crate) fn on_probe(&mut self, probe: DebuggerProbe) -> Option<Outcome> {
        if probe.label == DebuggerProbeLabel::Pending || !self.accepts_probe() {
            return None;
        }
        self.observed = probe.mode;
        let Phase::Exited(change) = self.phase else {
            return None;
        };
        self.phase = Phase::Idle;
        let outcome = match probe.mode {
            mode if mode == change.target() => Outcome::Changed(change),
            mode if mode == change.precondition() => Outcome::NotChanged(change),
            DebuggerMode::Unrecognized => Outcome::Unrecognized(change),
            _ => Outcome::Unobserved(change),
        };
        self.last = Some(outcome);
        Some(outcome)
    }

    /// The owner pressed the button for `change`. Refused unless the
    /// predecessor's records are read, nothing is under way, and the last
    /// probe shows the change's precondition. Never refused for aterm's own
    /// hardening (O8).
    pub(crate) fn on_press(&mut self, change: DebuggerChange) -> Result<(), Refusal> {
        if !self.records_read {
            return Err(Refusal::RecordsUnread);
        }
        match self.phase {
            Phase::Idle => {}
            Phase::Rechecking(_) | Phase::InFlight { .. } => return Err(Refusal::InFlight),
            Phase::Confirming(_) | Phase::Exited(_) => return Err(Refusal::Busy),
        }
        if self.observed != change.precondition() {
            return Err(Refusal::State(change, self.observed));
        }
        self.direction = change;
        self.phase = Phase::Confirming(change);
        Ok(())
    }

    /// The owner's answer in aterm's alert. `None` — no alert could be shown
    /// — is not a yes.
    pub(crate) fn on_owner_answer(&mut self, answer: Option<bool>) -> Answer {
        let Phase::Confirming(change) = self.phase else {
            return Answer::NotAsked;
        };
        if answer == Some(true) {
            self.consents = self.consents.saturating_add(1);
            self.phase = Phase::Rechecking(change);
            Answer::Consented
        } else {
            self.phase = Phase::Idle;
            Answer::Declined
        }
    }

    /// The worker's re-read showed the precondition and the child started.
    pub(crate) fn on_child_spawned(&mut self, child: DevtoolsChild, since: u64) -> bool {
        let Phase::Rechecking(change) = self.phase else {
            return false;
        };
        self.launches = self.launches.saturating_add(1);
        self.phase = Phase::InFlight {
            change,
            child,
            since,
            inherited: false,
        };
        true
    }

    /// The worker's re-read did NOT show the precondition: nothing was
    /// spawned. What it read is the flow's observation now.
    pub(crate) fn on_not_due(&mut self, observed: DebuggerProbe) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.observed = observed.mode;
        self.phase = Phase::Idle;
        let outcome = Outcome::NotDue(change, observed.mode);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// Another aterm holds the change lock: nothing was read or spawned for
    /// the change being re-checked.
    pub(crate) fn on_peer_busy(&mut self) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.phase = Phase::Idle;
        let outcome = Outcome::PeerInFlight(change);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// No child was started for the change being re-checked.
    pub(crate) fn on_not_started(&mut self) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.phase = Phase::Idle;
        let outcome = Outcome::NotStarted(change);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// The park took the exited child's result — it arrived, or the child is
    /// gone with no result (lost). Either way only a fresh probe says what
    /// happened. The three exits the model tells apart are macOS's and the
    /// administrator's; this takes each of them the same way.
    pub(crate) fn on_child_exit(&mut self) -> bool {
        let Phase::InFlight { change, .. } = self.phase else {
            return false;
        };
        self.phase = Phase::Exited(change);
        true
    }

    #[cfg(test)]
    pub(crate) const fn counts(&self) -> (u32, u32) {
        (self.consents, self.launches)
    }
}

/// What the host's start re-checks besides the reducer (design §3.4 step 2).
/// `self_hardened` is carried so the refusal table can be pinned: it is NOT a
/// gate (O8, owner 2026-09-29).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BeginInputs {
    pub(crate) windowed: bool,
    pub(crate) macos: bool,
    pub(crate) privacy: bool,
    pub(crate) self_hardened: SelfHardening,
}

/// The host's gates, in order: windowed, macOS, `[privacy] enabled && check`.
pub(crate) const fn admit_begin(inputs: BeginInputs) -> Result<(), Refusal> {
    if !inputs.windowed {
        return Err(Refusal::NoWindow);
    }
    if !inputs.macos {
        return Err(Refusal::NotMacos);
    }
    if !inputs.privacy {
        return Err(Refusal::PrivacyOff);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The shared result slot and the worker
// ---------------------------------------------------------------------------

/// Why a worker ran nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerRefusal {
    /// Another worker — another aterm's, or a second one here — holds the
    /// change lock, or the change marker names a child still running.
    PeerChanging,
    /// The change lock could not be opened: fail closed.
    NoLock,
}

#[derive(Debug, Default)]
struct SlotInner {
    job: u64,
    spawned: Option<DevtoolsChild>,
    result: Option<DevtoolsChangeRun>,
    refused: Option<WorkerRefusal>,
    /// The worker for `job` has ended (normally or not): a result it never
    /// published is lost.
    gone: bool,
}

/// The shared result slot (design §3.4 step 3.5). The worker publishes the
/// child as soon as it starts and the result when it exits; the park reads
/// both. Keyed by job, so a late publication from an older job is dropped.
#[derive(Debug, Default)]
pub(crate) struct ChangeSlot {
    inner: Mutex<SlotInner>,
}

/// One read of the slot for the current job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SlotRead {
    pub(crate) spawned: Option<DevtoolsChild>,
    pub(crate) result: Option<DevtoolsChangeRun>,
    pub(crate) refused: Option<WorkerRefusal>,
    pub(crate) gone: bool,
}

impl ChangeSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start a new job: everything an older one left is dropped.
    pub(crate) fn arm(&self, job: u64) {
        *self.lock() = SlotInner {
            job,
            ..SlotInner::default()
        };
    }

    pub(crate) fn publish_spawned(&self, job: u64, child: DevtoolsChild) {
        let mut inner = self.lock();
        if inner.job == job {
            inner.spawned = Some(child);
        }
    }

    pub(crate) fn publish_result(&self, job: u64, run: DevtoolsChangeRun) {
        let mut inner = self.lock();
        if inner.job == job {
            inner.result = Some(run);
        }
    }

    pub(crate) fn publish_refused(&self, job: u64, why: WorkerRefusal) {
        let mut inner = self.lock();
        if inner.job == job {
            inner.refused = Some(why);
        }
    }

    /// Whether `job`'s worker has ended with something the park has not
    /// taken yet (the model's phase 5 when a child ran). Reads, takes nothing.
    #[cfg(test)]
    pub(crate) fn unread(&self, job: u64) -> bool {
        let inner = self.lock();
        inner.job == job && (inner.result.is_some() || inner.refused.is_some() || inner.gone)
    }

    fn worker_gone(&self, job: u64) {
        let mut inner = self.lock();
        if inner.job == job {
            inner.gone = true;
        }
    }

    /// Take what the worker published for `job` since the last read. `gone`
    /// stays set.
    pub(crate) fn take(&self, job: u64) -> SlotRead {
        let mut inner = self.lock();
        if inner.job != job {
            return SlotRead::default();
        }
        SlotRead {
            spawned: inner.spawned.take(),
            result: inner.result.take(),
            refused: inner.refused.take(),
            gone: inner.gone,
        }
    }
}

/// One confirmed change: the direction, its job number and the owner's yes
/// it was confirmed with. Its fields are private to this module and only
/// [`DebuggerChangeLane::begin`] mints one, on an [`OwnerAnswer::Yes`] —
/// whose [`OwnerYes`] only `owner_confirms` mints — so a [`DevtoolsJob`], and
/// with it the worker that runs `DevToolsSecurity`, exists only after the
/// owner's yes in aterm's alert. Not `Clone`: one yes, one job.
///
/// [`OwnerAnswer::Yes`]: crate::native_settings::OwnerAnswer::Yes
/// [`OwnerYes`]: crate::native_settings::OwnerYes
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChangeTicket {
    change: DebuggerChange,
    job: u64,
    _yes: crate::native_settings::OwnerYes,
}

impl ChangeTicket {
    /// The job number the worker publishes under.
    #[cfg(test)]
    pub(crate) const fn job(&self) -> u64 {
        self.job
    }

    /// A ticket minted outside the lane, for tests only.
    #[cfg(test)]
    pub(crate) const fn for_test(change: DebuggerChange, job: u64) -> Self {
        Self {
            change,
            job,
            _yes: crate::native_settings::OwnerYes::for_test(),
        }
    }
}

/// Everything the worker needs, handed through `ConsentGestures::run_devtools`
/// (a plain function pointer, so the job carries its captures). Its fields are
/// private and [`Self::new`] takes a [`ChangeTicket`], so no job — and no
/// worker — exists without the owner's yes.
pub(crate) struct DevtoolsJob {
    change: DebuggerChange,
    /// `[privacy] enabled && check`: the worker's re-read goes through it.
    gate: ProbeGate,
    job: u64,
    slot: Arc<ChangeSlot>,
    /// The `aterm.toml` beside which the change lock is taken and the change
    /// marker written. `None` on an instance with no config location: it has
    /// neither, so nothing beyond this process's own reducer keeps a second
    /// child out.
    config_path: Option<PathBuf>,
    /// The nudge (`Wake::DebuggerAccessResult`). The result is in `slot`.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The lane's liveness check (pid AND birth): a change marker naming a
    /// child it proves running is a change in flight on this Mac, whoever
    /// started it, and the worker runs nothing over it.
    alive: fn(&DevtoolsChild) -> bool,
}

impl DevtoolsJob {
    /// The job for one confirmed change; `ticket` carries the direction, the
    /// job number and the owner's yes.
    pub(crate) fn new(
        ticket: ChangeTicket,
        gate: ProbeGate,
        slot: Arc<ChangeSlot>,
        config_path: Option<PathBuf>,
        wake: Arc<dyn Fn() + Send + Sync>,
        alive: fn(&DevtoolsChild) -> bool,
    ) -> Self {
        let ChangeTicket { change, job, .. } = ticket;
        Self {
            change,
            gate,
            job,
            slot,
            config_path,
            wake,
            alive,
        }
    }
}

impl std::fmt::Debug for DevtoolsJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevtoolsJob")
            .field("change", &self.change)
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

/// What `ConsentGestures::run_devtools` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DevtoolsSpawn {
    /// The detached worker is running.
    Started,
    /// The inert arm: nothing was started.
    Inert,
    /// The worker thread could not start: nothing ran.
    SpawnFailed,
}

/// The inert arm: a headless instance and every unit test. Starts nothing.
pub(crate) fn inert_run_devtools(_job: DevtoolsJob) -> DevtoolsSpawn {
    DevtoolsSpawn::Inert
}

/// The live arm: ONE detached `consent-devtools` worker, its handle dropped
/// (the `consent-warmup` pattern) — it may wait for hours on macOS's dialog,
/// so nothing may ever join it. Never called from a test.
pub(crate) fn spawn_devtools_worker(job: DevtoolsJob) -> DevtoolsSpawn {
    let spawned = std::thread::Builder::new()
        .name("consent-devtools".into())
        .spawn(move || {
            // Responsive: it publishes into the result slot the UI thread's
            // park reads under the same mutex, the documented floor for such a
            // holder. Nearly all of its life is a wait on one child, so the
            // rank costs nothing.
            crate::qos::set_self(crate::qos::Role::Responsive);
            run_devtools_job(job, |change, gate, on_spawn| {
                aterm_containment::consent::run_devtools_change(change, gate, on_spawn)
            });
        });
    // DETACHED ON PURPOSE: the handle is dropped here and never stored.
    match spawned {
        Ok(_handle) => DevtoolsSpawn::Started,
        Err(error) => {
            aterm_log::warn!("debugger access: the change worker could not start: {error}");
            DevtoolsSpawn::SpawnFailed
        }
    }
}

/// Sets `gone` and posts the one nudge when the worker ends, however it ends:
/// a worker that panics still tells the park its result is lost.
struct WorkerGone {
    slot: Arc<ChangeSlot>,
    job: u64,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for WorkerGone {
    fn drop(&mut self) {
        self.slot.worker_gone(self.job);
        (self.wake)();
    }
}

/// The worker's body, with the change runner injected (the live one is
/// `aterm_containment::consent::run_devtools_change`: the fresh `-status`
/// read and the spawn only over the precondition, through the private order
/// `devtools_change_with`, which the Tier-1 bind drives through
/// `devtools_change_with_for_test` with a scripted read and a stand-in child).
/// It takes the Mac-wide change lock first — a lock held elsewhere runs
/// nothing — publishes the child the moment it starts and
/// writes the change marker, publishes the result when the child exits,
/// releases the lock, and — through [`WorkerGone`] — sets `gone` and posts the
/// nudge. The marker stays until the park has taken the result, re-probed,
/// and recorded what the re-probe read ([`LaneStep::remove_change_marker`]).
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "Recheck",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "Launch",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "ChildApplied",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "ChildUnchanged",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "DebuggerAccessChange",
        action = "ChildClobbers",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
pub(crate) fn run_devtools_job<R>(job: DevtoolsJob, run: R)
where
    R: FnOnce(DebuggerChange, ProbeGate, Box<dyn FnOnce(DevtoolsChild) + '_>) -> DevtoolsChangeRun,
{
    let _gone = WorkerGone {
        slot: Arc::clone(&job.slot),
        job: job.job,
        wake: Arc::clone(&job.wake),
    };
    let (change, id) = (job.change, job.job);
    let config_path = job.config_path.as_deref();
    let (slot, wake) = (&job.slot, &job.wake);
    // Held through the re-read, the spawn and the whole wait, and released
    // BEFORE the result is published (below): the park that takes the result
    // removes the change marker once its re-probe is recorded, and must find
    // the lock free, or the removal skips and a stale marker outlives the
    // change.
    let lock = match config_path.map(|path| take_change_lock(path, CHANGE_LOCK_PATIENCE)) {
        None => None,
        Some(LockTry::Held(lock)) => Some(lock),
        Some(LockTry::Busy) => {
            slot.publish_refused(id, WorkerRefusal::PeerChanging);
            return;
        }
        Some(LockTry::Unavailable(error)) => {
            aterm_log::warn!("debugger access: the change lock could not be opened: {error}");
            slot.publish_refused(id, WorkerRefusal::NoLock);
            return;
        }
    };
    // A holder that died (an aterm that quit while its child waited) left the
    // lock free but its child running: its marker still names that child, so
    // a second child is refused here too, whoever this aterm is.
    if let Some(path) = config_path
        && read_debugger_marker(path, CHANGE_MARKER)
            .and_then(|text| ChangeMarker::parse(&text))
            .is_some_and(|marker| (job.alive)(&marker.child))
    {
        slot.publish_refused(id, WorkerRefusal::PeerChanging);
        return;
    }
    let result = run(
        change,
        job.gate,
        Box::new(|child: DevtoolsChild| {
            slot.publish_spawned(id, child);
            match (config_path, ChangeMarker::new(change, child, now_secs())) {
                (Some(path), Some(marker)) => {
                    if let Err(error) = write_debugger_marker(path, CHANGE_MARKER, &marker.encode())
                    {
                        aterm_log::warn!("debugger access: change marker not recorded: {error}");
                    }
                }
                (Some(_), None) => {
                    aterm_log::warn!(
                        "debugger access: the change child's birth could not be read; no marker"
                    );
                }
                (None, _) => {}
            }
            // The child is in flight: the page disables its button now.
            wake();
        }),
    );
    drop(lock);
    slot.publish_result(id, result);
}

// ---------------------------------------------------------------------------
// Drift and the records (§3.5, §3.8)
// ---------------------------------------------------------------------------

/// The `LogOnly` records of design §3.8, as a fact; the words are
/// `message_reporters::debugger_access_record`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebuggerRecord {
    /// Observed on after this process's change.
    On,
    /// Observed on with no change of aterm's under way.
    OnOutside,
    /// Observed off after this process's change.
    Off,
    /// Was observed on; a probe reads it off now, and aterm did not turn it
    /// off. The cause is not named: a macOS upgrade, an administrator and
    /// `-disable` all look the same.
    WasOnNowOff,
}

/// What one observation asks the host to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DriftStep {
    pub(crate) record: Option<DebuggerRecord>,
    /// The observed marker to write (only when the mode changed).
    pub(crate) write: Option<ObservedMarker>,
}

/// The observed-mode record and drift (design §3.5). It acts only on the two
/// modes a change moves between; `unrecognized` and `unknown` are not drift.
/// It never re-enables anything.
///
/// The record is SHARED: every aterm on this config writes it — a second
/// windowed aterm such as a development build beside the installed app (O8)
/// changes the rule and records it as its own. So a mode this process did not
/// change itself is judged against the marker as it is on disk NOW, never
/// against this process's copy: the watch un-seeds, keeps the mode, and the
/// lane reads the records again ([`DebuggerChangeLane::poll`]). A peer's
/// change is then already on record as aterm's, and is neither re-filed as
/// "set outside aterm" or drift nor overwritten.
#[derive(Clone, Debug, Default)]
pub(crate) struct DriftWatch {
    /// The observed marker has been read (or there was none to read), and no
    /// mode seen since waits to be judged against a fresh read.
    seeded: bool,
    /// The last recorded mode: from disk, then from this process's writes.
    recorded: Option<ObservedMarker>,
    /// The latest mode seen while unseeded, with this process's change when
    /// that probe was the re-probe that ended it.
    early: Option<(DebuggerMode, Option<DebuggerChange>)>,
    /// While the page should say "Was allowed (<date>); it is off now": when
    /// it was first seen allowed.
    drift_since: Option<u64>,
}

impl DriftWatch {
    pub(crate) const fn recorded(&self) -> Option<ObservedMarker> {
        self.recorded
    }

    pub(crate) const fn drift_since(&self) -> Option<u64> {
        self.drift_since
    }

    /// Whether the watch judges now; `false` until the marker is read, and
    /// again while a mode waits for a fresh read of it.
    pub(crate) const fn seeded(&self) -> bool {
        self.seeded
    }

    /// The marker was read. A mode seen before then is judged against it now —
    /// unless the same read put a change under way (a predecessor's child
    /// still running, or one whose result nobody took) and the mode carries no
    /// attribution: that early probe may predate the change's end, so it is
    /// dropped, and the re-probe that ends the change is the one judged, as
    /// aterm's.
    pub(crate) fn seed(
        &mut self,
        recorded: Option<ObservedMarker>,
        now: u64,
        under_way: bool,
    ) -> DriftStep {
        self.seeded = true;
        self.recorded = recorded;
        if recorded.is_some_and(|m| m.mode == DebuggerMode::Enabled || m.origin == Origin::Aterm) {
            // On now, or off by aterm's hand (a peer's): no drift to show.
            self.drift_since = None;
        }
        match self.early.take() {
            Some((mode, attributed @ Some(_))) => self.judge(mode, attributed, now),
            Some((mode, None)) if !under_way => self.judge(mode, None, now),
            _ => DriftStep::default(),
        }
    }

    /// One completed probe's mode. `attributed` is this process's change, when
    /// the probe is the re-probe that ended it. A mode the record does not
    /// hold that this process did not set waits, unseeded, for a fresh read of
    /// the shared marker (see the type's doc).
    pub(crate) fn on_observed(
        &mut self,
        mode: DebuggerMode,
        attributed: Option<DebuggerChange>,
        now: u64,
    ) -> DriftStep {
        if !matches!(mode, DebuggerMode::Enabled | DebuggerMode::Disabled) {
            return DriftStep::default();
        }
        let ours = attributed.is_some_and(|change| change.target() == mode);
        let transition = self.recorded.map(|m| m.mode) != Some(mode);
        if !self.seeded || (transition && !ours) {
            // A re-probe's attribution survives a later read of the same mode.
            let kept = self
                .early
                .and_then(|(seen, change)| change.filter(|_| seen == mode));
            self.seeded = false;
            self.early = Some((mode, attributed.filter(|_| ours).or(kept)));
            return DriftStep::default();
        }
        self.judge(mode, attributed, now)
    }

    /// Judge `mode` against the record held now.
    fn judge(
        &mut self,
        mode: DebuggerMode,
        attributed: Option<DebuggerChange>,
        now: u64,
    ) -> DriftStep {
        let previous = self.recorded;
        if previous.map(|m| m.mode) == Some(mode) {
            return DriftStep::default();
        }
        let ours = attributed.is_some_and(|change| change.target() == mode);
        let origin = if ours {
            Origin::Aterm
        } else if previous.is_some() {
            Origin::Outside
        } else {
            Origin::Unknown
        };
        let record = match mode {
            DebuggerMode::Enabled => {
                self.drift_since = None;
                if ours {
                    Some(DebuggerRecord::On)
                } else if previous.is_some() {
                    Some(DebuggerRecord::OnOutside)
                } else {
                    None
                }
            }
            _ => {
                if ours {
                    self.drift_since = None;
                    Some(DebuggerRecord::Off)
                } else if let Some(was) = previous.filter(|m| m.mode == DebuggerMode::Enabled) {
                    self.drift_since = Some(was.since);
                    Some(DebuggerRecord::WasOnNowOff)
                } else {
                    None
                }
            }
        };
        let marker = ObservedMarker {
            mode,
            since: now,
            origin,
        };
        self.recorded = Some(marker);
        DriftStep {
            record,
            write: Some(marker),
        }
    }
}

// ---------------------------------------------------------------------------
// The lane: what `App` owns
// ---------------------------------------------------------------------------

/// What one poll or observation asks the host to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneStep {
    /// Retire the cached probe: the rule may have changed under it.
    pub(crate) invalidate_probe: bool,
    /// The change marker naming this child may go: the change it names has
    /// ended, its re-probe has been judged, and the observed record that
    /// judgment writes (`drift.write`, here or in an earlier step) goes to
    /// disk first ([`settle_markers`]). Removed only if it still names it.
    pub(crate) remove_change_marker: Option<DevtoolsChild>,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) drift: DriftStep,
}

impl LaneStep {
    fn merge(&mut self, other: Self) {
        self.invalidate_probe |= other.invalidate_probe;
        self.remove_change_marker = other.remove_change_marker.or(self.remove_change_marker);
        self.outcome = other.outcome.or(self.outcome);
        self.drift.record = other.drift.record.or(self.drift.record);
        self.drift.write = other.drift.write.or(self.drift.write);
    }
}

/// The `App`-owned lane: the reducer, the shared result slot, the records
/// reader's answer and the drift watch. Pure apart from the liveness check it
/// was built with (`DevtoolsChild::is_running` for a windowed instance; a
/// headless one never has a child to ask about).
pub(crate) struct DebuggerChangeLane {
    flow: DebuggerAccessFlow,
    slot: Arc<ChangeSlot>,
    job: u64,
    /// The current job's result has been taken.
    job_done: bool,
    /// The current job's child, once the park has read it from the slot.
    child: Option<DevtoolsChild>,
    /// The child of the change that exited and waits to be judged — this
    /// process's own, a predecessor's inherited one, or one a stale marker
    /// named. Its change marker stays on disk until the re-probe has judged
    /// the change and the observed record is written: a peer aterm that
    /// re-reads the records in between finds that marker (its child dead) and
    /// re-reads the change as aterm's, where with no marker and the old record
    /// it would file it as "set outside aterm".
    settling: Option<DevtoolsChild>,
    /// The last child whose marker this lane let go. Its removal runs on
    /// another thread, so a records read can still find that marker; the
    /// lane follows it and never adopts its own finished change twice.
    released: Option<DevtoolsChild>,
    /// Whether this instance has a window to change anything from. The App's
    /// park and deadline consult this, not `App::headless`, so a test can
    /// drive the park's fairness site on a headless `App`.
    windowed: bool,
    records: Arc<Mutex<Option<RecordsRead>>>,
    /// A records read was ever asked for.
    records_asked: bool,
    /// A records read is out: asked for, and not yet taken by [`Self::poll`].
    records_out: bool,
    /// Another read is wanted from this wall second: an unsettled read is
    /// read again after [`RESULT_RECHECK`], and a mode the drift watch must
    /// judge against the shared record at once.
    records_again_at: Option<u64>,
    alive: fn(&DevtoolsChild) -> bool,
    drift: DriftWatch,
    refusal: Option<Refusal>,
}

impl std::fmt::Debug for DebuggerChangeLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebuggerChangeLane")
            .field("flow", &self.flow)
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

/// A headless instance's liveness answer: it has no child to ask about.
const fn never_alive(_child: &DevtoolsChild) -> bool {
    false
}

impl DebuggerChangeLane {
    /// A windowed instance reads liveness by pid and birth; a headless one
    /// never has a change child, and asks nothing.
    pub(crate) fn new(headless: bool) -> Self {
        let mut lane = Self::with_liveness(if headless {
            never_alive
        } else {
            DevtoolsChild::is_running
        });
        lane.windowed = !headless;
        lane
    }

    /// A windowed lane with an injected liveness check (the successor's
    /// pid-and-birth read); tests pass a stand-in.
    pub(crate) fn with_liveness(alive: fn(&DevtoolsChild) -> bool) -> Self {
        Self {
            flow: DebuggerAccessFlow::new(),
            slot: Arc::new(ChangeSlot::default()),
            job: 0,
            job_done: true,
            child: None,
            settling: None,
            released: None,
            windowed: true,
            records: Arc::new(Mutex::new(None)),
            records_asked: false,
            records_out: false,
            records_again_at: None,
            alive,
            drift: DriftWatch::default(),
            refusal: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn flow(&self) -> &DebuggerAccessFlow {
        &self.flow
    }

    pub(crate) const fn alive(&self) -> fn(&DevtoolsChild) -> bool {
        self.alive
    }

    /// Whether the park runs this lane at all (a windowed instance).
    pub(crate) const fn windowed(&self) -> bool {
        self.windowed
    }

    /// Whether the park would find something unread: the current job's worker
    /// ended with its result, refusal or loss still in the slot, or a
    /// predecessor's child is gone. The model's phase 5 when a child ran.
    #[cfg(test)]
    pub(crate) fn result_unread(&self) -> bool {
        (!self.job_done && self.slot.unread(self.job))
            || self
                .flow
                .in_flight()
                .is_some_and(|(_, child, _, inherited)| inherited && !(self.alive)(&child))
    }

    #[cfg(test)]
    pub(crate) const fn records_asked(&self) -> bool {
        self.records_asked
    }

    /// Whether the park starts a records reader now: none is out, and this is
    /// the first read or a read again is due (`now_wall` in wall seconds).
    pub(crate) fn records_due(&self, now_wall: u64) -> bool {
        !self.records_out
            && (!self.records_asked || self.records_again_at.is_some_and(|at| now_wall >= at))
    }

    /// The records reader was started, or there is nothing to read: the slot
    /// it publishes into.
    pub(crate) fn ask_records(&mut self) -> Arc<Mutex<Option<RecordsRead>>> {
        self.records_asked = true;
        self.records_out = true;
        self.records_again_at = None;
        Arc::clone(&self.records)
    }

    /// Settle the records without a reader: no marker location (headless,
    /// tests), or its thread could not start.
    pub(crate) fn settle_records(&mut self, read: RecordsRead) {
        *self.ask_records().lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
    }

    /// Record why the last confirmed press started nothing (the page shows it
    /// until the next press starts).
    pub(crate) fn note_refusal(&mut self, refusal: Option<Refusal>) {
        self.refusal = refusal;
    }

    /// Whether the park must keep re-reading: a change under way, or the
    /// records asked for and not yet read.
    pub(crate) fn needs_poll(&self) -> bool {
        (self.records_asked && !self.flow.records_read())
            || matches!(
                self.flow.phase(),
                Phase::Rechecking(_) | Phase::InFlight { .. } | Phase::Exited(_)
            )
    }

    /// Whether the park calls [`Self::poll`]: [`Self::needs_poll`] (the
    /// model's `polling`), or a records read out or wanted — also a re-read
    /// the drift watch asked for with nothing under way.
    pub(crate) fn wants_poll(&self) -> bool {
        self.needs_poll() || self.records_out || self.records_again_at.is_some()
    }

    /// The park's deadline for [`Self::poll`]: [`RESULT_RECHECK`] while it is
    /// wanted, nothing otherwise (an idle lane never wakes the loop).
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.wants_poll().then(|| now + RESULT_RECHECK)
    }

    /// Whether the flow waits for a fresh probe to end the change.
    pub(crate) fn awaiting_probe(&self) -> bool {
        matches!(self.flow.phase(), Phase::Exited(_))
    }

    /// The owner's press and their answer in aterm's alert, carried from the
    /// Settings arm that asked (`menu::confirm_owner`, which only the owner's
    /// press reaches), and — on a yes, and only then — a new job armed in the
    /// slot. The press is refused unless the records are read, nothing is
    /// under way and the last probe shows the precondition; any answer but
    /// yes starts nothing. `Err` names why nothing may start.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Press",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Consent",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Decline",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn begin(
        &mut self,
        change: DebuggerChange,
        answer: crate::native_settings::OwnerAnswer,
    ) -> Result<ChangeTicket, Refusal> {
        use crate::native_settings::OwnerAnswer;
        self.flow.on_press(change)?;
        let (said, yes) = match answer {
            OwnerAnswer::Yes(yes) => (Some(true), Some(yes)),
            OwnerAnswer::No => (Some(false), None),
            OwnerAnswer::Quiet | OwnerAnswer::NotAsked => (None, None),
        };
        let (Answer::Consented, Some(yes)) = (self.flow.on_owner_answer(said), yes) else {
            return Err(Refusal::NotConfirmed);
        };
        self.job = self.job.wrapping_add(1);
        self.job_done = false;
        self.child = None;
        self.slot.arm(self.job);
        Ok(ChangeTicket {
            change,
            job: self.job,
            _yes: yes,
        })
    }

    pub(crate) fn slot(&self) -> Arc<ChangeSlot> {
        Arc::clone(&self.slot)
    }

    /// The worker did not start: the change ends here, nothing ran.
    pub(crate) fn not_started(&mut self) -> LaneStep {
        self.job_done = true;
        LaneStep {
            outcome: self.flow.on_not_started(),
            ..LaneStep::default()
        }
    }

    /// ONE PARK RE-READ (design §3.4 steps 4 and 6): the worker's slot, then
    /// the records, then a predecessor's child's liveness. It takes an exited
    /// child's result whether or not its wake arrived (the model's `Notice`,
    /// weakly fair).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Notice",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Adopt",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Follow",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Discard",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn poll(&mut self, now_wall: u64) -> LaneStep {
        let mut step = LaneStep::default();
        // The records read is TAKEN before the worker's slot and HANDLED
        // after it. A read names a marker its worker wrote after
        // `publish_spawned`, so with the read taken first the slot take is
        // sure to hold that child, and handling the slot first means the job's
        // own child — spawned, its marker written, and gone — is followed by
        // the time the read is judged, never taken for a peer's.
        let records_read = if self.records_out {
            self.records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        } else {
            None
        };
        if !self.job_done {
            let read = self.slot.take(self.job);
            if let Some(child) = read.spawned {
                self.child = Some(child);
                self.flow.on_child_spawned(child, now_wall);
            }
            match (read.result, read.refused) {
                (_, Some(refused)) => {
                    self.job_done = true;
                    step.outcome = match refused {
                        WorkerRefusal::PeerChanging => self.flow.on_peer_busy(),
                        WorkerRefusal::NoLock => self.flow.on_not_started(),
                    };
                }
                (Some(DevtoolsChangeRun::NotDue { observed }), None) => {
                    self.job_done = true;
                    step.invalidate_probe = true;
                    step.outcome = self.flow.on_not_due(observed);
                    step.merge(LaneStep {
                        drift: self.drift_observed(observed.mode, None, now_wall),
                        ..LaneStep::default()
                    });
                }
                (Some(run), None) if run.spawned() => {
                    self.job_done = true;
                    step.invalidate_probe = true;
                    self.settling = self.child.take();
                    self.flow.on_child_exit();
                }
                (Some(_), None) => {
                    self.job_done = true;
                    step.invalidate_probe = true;
                    step.outcome = self.flow.on_not_started();
                }
                (None, None) if read.gone => {
                    // The worker ended with no result: lost. A child it
                    // started is re-read by probe; one it never started ran
                    // nothing.
                    self.job_done = true;
                    step.invalidate_probe = true;
                    self.settling = self.child.take();
                    if !self.flow.on_child_exit() {
                        step.outcome = self.flow.on_not_started();
                    }
                }
                (None, None) => {}
            }
        }
        if let Some(read) = records_read {
            self.records_out = false;
            if read.unsettled {
                // A worker holds the lock with no marker yet: read again
                // once it has written one, or let go.
                self.records_again_at = Some(now_wall.saturating_add(RESULT_RECHECK.as_secs()));
            } else {
                // A marker naming a child this lane already follows — in
                // flight, its result not yet taken, or kept until its
                // change is recorded — is not a change to re-read.
                let stale = read.stale.filter(|m| !self.follows(&m.child));
                if self
                    .flow
                    .on_records(read.inherited, stale.map(|m| m.change))
                {
                    self.settling = stale.map(|m| m.child);
                } else {
                    // Something is under way here: nobody re-reads that
                    // change, so its dead marker goes now.
                    step.remove_change_marker = stale.map(|m| m.child);
                }
                step.invalidate_probe |= stale.is_some();
                let under_way = self.flow.phase() != Phase::Idle;
                step.merge(LaneStep {
                    drift: self.drift.seed(read.observed, now_wall, under_way),
                    ..LaneStep::default()
                });
            }
        }
        if let Some((_, child, _, true)) = self.flow.in_flight()
            && !(self.alive)(&child)
        {
            // A predecessor's child exited: its worker died with that
            // process, so nothing will publish its result. Read the rule.
            self.flow.on_child_exit();
            step.invalidate_probe = true;
            self.settling = Some(child);
        }
        self.release_settled(&mut step);
        step
    }

    /// A completed probe (the cached `DebuggerSlot` answer). Ends an exited
    /// change and feeds the drift watch; while a change is under way the drift
    /// watch waits for the re-probe that can attribute it.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "Probe",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "DebuggerAccessChange",
            action = "ProbeFails",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn observe(&mut self, probe: DebuggerProbe, now_wall: u64) -> LaneStep {
        if probe.label == DebuggerProbeLabel::Pending {
            return LaneStep::default();
        }
        if !self.flow.accepts_probe() {
            return LaneStep::default();
        }
        let outcome = self.flow.on_probe(probe);
        let attributed = match outcome {
            Some(Outcome::Changed(change)) => Some(change),
            _ => None,
        };
        let mut step = LaneStep {
            outcome,
            drift: self.drift_observed(probe.mode, attributed, now_wall),
            ..LaneStep::default()
        };
        self.release_settled(&mut step);
        step
    }

    /// Whether `child` is one this lane already follows: the current job's,
    /// the flow's in flight (a predecessor's included), or the one settling.
    fn follows(&self, child: &DevtoolsChild) -> bool {
        [
            self.child,
            self.settling,
            self.released,
            self.flow.in_flight().map(|(_, child, _, _)| child),
        ]
        .contains(&Some(*child))
    }

    /// Let the settling child's marker go once its change is judged: the
    /// re-probe ended it, and the drift watch has judged that probe — its
    /// observed record is in this step (written before the removal), or was
    /// in an earlier one. A watch waiting for a fresh records read judges the
    /// probe at that read's `seed`, and the marker waits for it.
    fn release_settled(&mut self, step: &mut LaneStep) {
        if !matches!(self.flow.phase(), Phase::Exited(_)) && self.drift.seeded() {
            // A dead marker this step read off disk names what is there now.
            let settled = self.settling.take();
            self.released = settled.or(self.released);
            step.remove_change_marker = step.remove_change_marker.or(settled);
        }
    }

    /// Feed the drift watch. A mode it must judge against the shared record
    /// as it is on disk now (one this process did not set) asks for a records
    /// read at once; the read's `seed` judges it.
    fn drift_observed(
        &mut self,
        mode: DebuggerMode,
        attributed: Option<DebuggerChange>,
        now_wall: u64,
    ) -> DriftStep {
        let step = self.drift.on_observed(mode, attributed, now_wall);
        if !self.drift.seeded()
            && self.records_asked
            && !self.records_out
            && self.records_again_at.is_none()
        {
            self.records_again_at = Some(now_wall);
        }
        step
    }

    /// What the Security page renders, from the cached probe and this lane.
    pub(crate) fn facts(
        &self,
        enabled: bool,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
    ) -> DebuggerFacts {
        DebuggerFacts {
            enabled,
            probe,
            self_hardened,
            in_flight: self
                .flow
                .in_flight()
                .map(|(change, _, since, inherited)| InFlightFacts {
                    change,
                    since,
                    inherited,
                }),
            busy: self.flow.busy(),
            recorded: self.drift.recorded(),
            drift_since: self.drift.drift_since(),
            last_outcome: self.flow.last(),
            refusal: self.refusal,
        }
    }
}

// ---------------------------------------------------------------------------
// What the Security page renders
// ---------------------------------------------------------------------------

/// A change child in flight, for the row's "waiting" line (since <time>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InFlightFacts {
    pub(crate) change: DebuggerChange,
    /// When the child started, in wall seconds.
    pub(crate) since: u64,
    /// A predecessor's child, read from its marker.
    pub(crate) inherited: bool,
}

/// The debugger row's facts (design §3.3). Pure data: the copy is a total
/// function of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DebuggerFacts {
    /// `[privacy] enabled && check`. `false`: aterm did not look, and offers
    /// nothing.
    pub(crate) enabled: bool,
    /// The cached probe: the rule's mode, the account's membership, and why.
    /// The ONLY source of what the row says the rule is.
    pub(crate) probe: DebuggerProbe,
    /// aterm's own hardening. Not a gate (O8): when it is not `Hardened` the
    /// row and the alert say what that exposes.
    pub(crate) self_hardened: SelfHardening,
    /// A change child in flight, this process's or a predecessor's.
    pub(crate) in_flight: Option<InFlightFacts>,
    /// Anything under way (the alert, the re-read, the child, the re-probe)
    /// or the predecessor's records unread: no button is live.
    pub(crate) busy: bool,
    /// The last recorded mode, when aterm first saw it, and who set it: "turned
    /// on from aterm on <date>" / "set outside aterm".
    pub(crate) recorded: Option<ObservedMarker>,
    /// Drift: allowed since this time, off now (in this process).
    pub(crate) drift_since: Option<u64>,
    /// What the last change in this process came to.
    pub(crate) last_outcome: Option<Outcome>,
    /// Why the last confirmed press started nothing.
    pub(crate) refusal: Option<Refusal>,
}

impl Default for DebuggerFacts {
    /// The honest zero state: aterm did not look, nothing is under way that it
    /// knows of, and nothing is offered.
    fn default() -> Self {
        Self {
            enabled: false,
            probe: DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled),
            self_hardened: SelfHardening::Unknown,
            in_flight: None,
            busy: true,
            recorded: None,
            drift_since: None,
            last_outcome: None,
            refusal: None,
        }
    }
}

impl DebuggerFacts {
    /// Whether the button for `change` is drawn live: aterm looked, nothing is
    /// under way, the probe shows the change's precondition, and — for *Allow*
    /// — the account is one the switch would help. aterm's own hardening is
    /// not consulted (O8).
    pub(crate) fn offers(&self, change: DebuggerChange) -> bool {
        self.enabled
            && !self.busy
            && self.probe.mode == change.precondition()
            && (change == DebuggerChange::Off || self.probe.member != DebuggerMember::NotMember)
    }

    /// Whether the row and the alert must add that this aterm is not
    /// hardened, so any program the owner runs could take control of it.
    pub(crate) fn not_hardened(&self) -> bool {
        self.self_hardened != SelfHardening::Hardened
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_settings::OwnerAnswer;

    fn probe(mode: DebuggerMode) -> DebuggerProbe {
        DebuggerProbe {
            mode,
            member: DebuggerMember::Admin,
            label: if mode == DebuggerMode::Unknown {
                DebuggerProbeLabel::StatusUnrecognized
            } else {
                DebuggerProbeLabel::StatusOk
            },
        }
    }

    fn child(pid: u32) -> DevtoolsChild {
        DevtoolsChild {
            pid,
            birth: Some(ProcessBirth {
                seconds: 1_790_000_000,
                microseconds: 42,
            }),
        }
    }

    fn ready_flow(mode: DebuggerMode) -> DebuggerAccessFlow {
        let mut flow = DebuggerAccessFlow::new();
        flow.on_records(None, None);
        assert_eq!(flow.on_probe(probe(mode)), None);
        flow
    }

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-debugger-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn job(change: DebuggerChange, id: u64, slot: &Arc<ChangeSlot>) -> DevtoolsJob {
        DevtoolsJob::new(
            ChangeTicket::for_test(change, id),
            ProbeGate::on(),
            Arc::clone(slot),
            None,
            Arc::new(|| {}),
            |_| false,
        )
    }

    #[test]
    fn markers_round_trip_and_refuse_what_this_build_did_not_write() {
        let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 1_790_000_005).unwrap();
        assert_eq!(marker.encode(), "on 4242 1790000000.000042 1790000005");
        assert_eq!(ChangeMarker::parse(&marker.encode()), Some(marker));
        assert!(marker.encode().len() <= MAX_MARKER_BYTES);
        let off = ChangeMarker::new(
            DebuggerChange::Off,
            DevtoolsChild {
                pid: u32::MAX,
                birth: Some(ProcessBirth {
                    seconds: u64::MAX,
                    microseconds: 999_999,
                }),
            },
            u64::MAX,
        )
        .unwrap();
        assert!(off.encode().len() <= MAX_MARKER_BYTES, "{}", off.encode());
        assert_eq!(ChangeMarker::parse(&off.encode()), Some(off));
        for bad in [
            "",
            "enable 1 2.3 4",
            "on 0 1.2 3",
            "on 1 2 3",
            "on 1 2.3",
            "on 1 2.3 4 5",
            "on x 2.3 4",
        ] {
            assert_eq!(ChangeMarker::parse(bad), None, "{bad:?}");
        }
        // A child whose birth was not read gets no marker at all.
        assert_eq!(
            ChangeMarker::new(
                DebuggerChange::On,
                DevtoolsChild {
                    pid: 7,
                    birth: None
                },
                1
            ),
            None
        );
        let observed = ObservedMarker {
            mode: DebuggerMode::Enabled,
            since: 1_790_000_000,
            origin: Origin::Aterm,
        };
        assert_eq!(observed.encode(), "enabled 1790000000 aterm");
        assert_eq!(ObservedMarker::parse(&observed.encode()), Some(observed));
        for bad in [
            "",
            "unknown 1 aterm",
            "unrecognized 1 aterm",
            "enabled x aterm",
            "enabled 1 someone",
            "enabled 1 aterm extra",
        ] {
            assert_eq!(ObservedMarker::parse(bad), None, "{bad:?}");
        }
    }

    /// A held change lock is waited out for the patience given and no longer;
    /// one released inside it is taken.
    #[test]
    fn a_busy_change_lock_is_waited_out_only_for_its_patience() {
        let dir = scratch("patience");
        let config = dir.join("aterm.toml");
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("a fresh lock is free");
        };
        // Held throughout: the patience runs out and the answer is Busy.
        let started = std::time::Instant::now();
        assert!(matches!(
            take_change_lock(&config, std::time::Duration::from_millis(120)),
            LockTry::Busy
        ));
        assert!(started.elapsed() >= std::time::Duration::from_millis(120));
        // Released inside the patience (a minute: a hang detector, never a
        // budget): the waiter takes it.
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(held);
        });
        assert!(matches!(
            take_change_lock(&config, std::time::Duration::from_secs(60)),
            LockTry::Held(_)
        ));
        releaser.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The change lock is exclusive — across open descriptions, so a second
    /// worker in this process is refused too — released on drop (free at once,
    /// in one try), and never taken through a planted link.
    #[test]
    fn the_change_lock_is_exclusive_released_on_drop_and_never_followed() {
        let dir = scratch("lock");
        let config = dir.join("aterm.toml");
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("a free lock is taken");
        };
        assert!(matches!(try_change_lock(&config), LockTry::Busy));
        drop(held);
        assert!(matches!(try_change_lock(&config), LockTry::Held(_)));
        #[cfg(unix)]
        {
            let linked = scratch("lock-link");
            std::os::unix::fs::symlink(dir.join("elsewhere"), linked.join(CHANGE_LOCK)).unwrap();
            assert!(matches!(
                try_change_lock(&linked.join("aterm.toml")),
                LockTry::Unavailable(_)
            ));
            assert!(
                !dir.join("elsewhere").exists(),
                "nothing was created through the link"
            );
            let _ = std::fs::remove_dir_all(&linked);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE WORKER'S LOCK IS FREE THE MOMENT IT ENDS, even while a child forked
    /// under it has yet to exec. The worker holds the lock through its child's
    /// whole run, and a child any other thread forks meanwhile — a new tab's
    /// shell, a sibling test's spawn — holds a copy of its description until
    /// it execs. [`ChangeLock`] lets go by `LOCK_UN`, so the park's marker
    /// removal that follows the result finds the lock free in its first try
    /// and removes the marker. The child is made for real inside the worker's
    /// hold and parked short of its exec through the removal, so nothing is
    /// waited out. NEGATIVE CONTROL: the same lock let go by the close alone
    /// stays taken in the parked child's copy — what `CHANGE_LOCK_PATIENCE`
    /// used to wait out, for a second, and a background-band child outlasts.
    #[cfg(unix)]
    #[test]
    fn the_workers_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
        use crate::parked_fork::ParkedChild;
        let dir = scratch("fork-window");
        let config = dir.join("aterm.toml");
        let slot = Arc::new(ChangeSlot::default());
        slot.arm(1);
        let mut marked = job(DebuggerChange::On, 1, &slot);
        marked.config_path = Some(config.clone());
        let mut forked = None;
        run_devtools_job(marked, |_, _, on_spawn| {
            on_spawn(child(77));
            forked = Some(ParkedChild::fork());
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        let forked = forked.expect("the runner forked under the worker's lock");
        assert!(dir.join(CHANGE_MARKER).exists(), "the worker's marker");
        assert!(
            matches!(try_change_lock(&config), LockTry::Held(_)),
            "the ended worker's lock is free in one try"
        );
        settle_markers(&config, None, Some(&child(77)));
        assert!(
            !dir.join(CHANGE_MARKER).exists(),
            "the park's removal took the lock and removed the marker"
        );
        assert!(
            forked.still_parked(),
            "…while the child forked under the lock had yet to exec"
        );
        forked.release();

        // NEGATIVE CONTROL: released by the close alone, the lock lives on in
        // the parked child's copy of the description, until that child execs.
        let closed_only = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(CHANGE_LOCK))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the negative control: a lock let go by the close alone"
        )]
        closed_only.try_lock().unwrap();
        let forked = ParkedChild::fork();
        drop(closed_only);
        assert!(
            matches!(try_change_lock(&config), LockTry::Busy),
            "control: closed, not unlocked, the child's copy holds it"
        );
        forked.release();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_records_reader_keeps_a_live_child_names_a_stale_one_and_drops_an_unreadable_one() {
        let dir = scratch("records");
        let config = dir.join("aterm.toml");
        let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        write_debugger_marker(&config, CHANGE_MARKER, &marker.encode()).unwrap();
        let observed = ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 5,
            origin: Origin::Unknown,
        };
        write_debugger_marker(&config, OBSERVED_MARKER, &observed.encode()).unwrap();
        let alive = read_records(&config, |_| true);
        assert_eq!(alive.inherited, Some(marker));
        assert_eq!(alive.stale, None);
        assert_eq!(alive.observed, Some(observed));
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "a live child's marker stays"
        );
        // A dead child's marker: named stale, whole, and left on disk for the
        // lane that re-reads its change to remove once that is recorded.
        let dead = read_records(&config, |_| false);
        assert_eq!(dead.inherited, None);
        assert_eq!(dead.stale, Some(marker), "its direction and child are kept");
        assert_eq!(dead.observed, Some(observed));
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "a stale marker waits for its change to be recorded"
        );
        // A marker this build cannot parse is nothing, and goes — never from
        // under a holder, who overwrites it.
        write_debugger_marker(&config, CHANGE_MARKER, "on not-a-pid").unwrap();
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("free");
        };
        assert_eq!(read_records(&config, |_| false).stale, None);
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "not removed from under a holder"
        );
        drop(held);
        assert_eq!(
            read_records(&config, |_| true),
            RecordsRead {
                observed: Some(observed),
                ..RecordsRead::default()
            }
        );
        assert!(
            !dir.join(CHANGE_MARKER).exists(),
            "an unreadable marker is removed"
        );
        // Only the marker naming the child goes.
        write_debugger_marker(&config, CHANGE_MARKER, &marker.encode()).unwrap();
        settle_markers(&config, None, Some(&child(9)));
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "a marker naming another child stays"
        );
        settle_markers(&config, None, Some(&child(4242)));
        assert!(!dir.join(CHANGE_MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A successor reads the records while its predecessor's worker holds the
    /// change lock in its fresh `-status` re-read, before the child (and its
    /// marker) exist; the handoff then commits and orphans the child with
    /// macOS's dialog up. "No marker" under a held lock settles nothing: the
    /// lane keeps its buttons off and reads again, and the second read finds
    /// the child in flight — never a live button, a peer's-change refusal, or
    /// the change filed as "set outside aterm".
    #[test]
    fn a_change_lock_held_before_its_marker_is_written_is_read_again() {
        let dir = scratch("unsettled");
        let config = dir.join("aterm.toml");
        assert_eq!(read_records(&config, |_| true), RecordsRead::default());
        assert!(
            !dir.join(CHANGE_LOCK).exists(),
            "a read where no change ever ran leaves no lock file"
        );
        let LockTry::Held(worker) = try_change_lock(&config) else {
            panic!("free");
        };
        let early = read_records(&config, |_| true);
        assert!(early.unsettled, "a held lock with no marker yet");
        let mut lane = DebuggerChangeLane::with_liveness(|_| true);
        assert!(lane.records_due(100), "the launch read");
        lane.settle_records(early);
        assert!(!lane.records_due(100), "one read out at a time");
        assert_eq!(lane.poll(100), LaneStep::default());
        assert!(!lane.flow().records_read(), "nothing was settled");
        assert!(lane.flow().busy(), "so no button is live");
        assert_eq!(
            lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                .map(|_| ()),
            Err(Refusal::RecordsUnread)
        );
        assert!(lane.needs_poll(), "the park keeps its RESULT_RECHECK");
        assert!(!lane.records_due(104), "not before RESULT_RECHECK");
        assert!(lane.records_due(105), "read again after it");
        // The worker spawned its child and recorded it; its aterm is gone.
        let marker = ChangeMarker::new(DebuggerChange::On, child(77), 101).unwrap();
        write_debugger_marker(&config, CHANGE_MARKER, &marker.encode()).unwrap();
        drop(worker);
        let again = read_records(&config, |_| true);
        assert_eq!(again.inherited, Some(marker));
        assert!(!again.unsettled);
        lane.settle_records(again);
        lane.poll(105);
        assert_eq!(
            lane.flow()
                .in_flight()
                .map(|(change, _, _, inherited)| (change, inherited)),
            Some((DebuggerChange::On, true)),
            "the orphaned child is this aterm's change in flight"
        );
        assert_eq!(
            lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                .map(|_| ()),
            Err(Refusal::InFlight)
        );
        assert!(!lane.records_due(200), "settled: never read again");
        // A lock let go with no marker (the re-read found nothing to do)
        // settles on nothing in flight.
        std::fs::remove_file(dir.join(CHANGE_MARKER)).unwrap();
        assert_eq!(read_records(&config, |_| true), RecordsRead::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The press guard: the probe's precondition, nothing under way, the
    /// records read — and NOT aterm's hardening (O8).
    #[test]
    fn a_press_is_refused_unless_the_probe_shows_its_precondition_and_nothing_is_under_way() {
        let mut unread = DebuggerAccessFlow::new();
        assert_eq!(
            unread.on_press(DebuggerChange::On),
            Err(Refusal::RecordsUnread)
        );
        let mut unknown = ready_flow(DebuggerMode::Unknown);
        assert_eq!(
            unknown.on_press(DebuggerChange::On),
            Err(Refusal::State(DebuggerChange::On, DebuggerMode::Unknown))
        );
        let mut on = ready_flow(DebuggerMode::Enabled);
        assert_eq!(
            on.on_press(DebuggerChange::On),
            Err(Refusal::State(DebuggerChange::On, DebuggerMode::Enabled))
        );
        assert_eq!(on.on_press(DebuggerChange::Off), Ok(()));
        let mut custom = ready_flow(DebuggerMode::Unrecognized);
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            assert!(custom.on_press(change).is_err(), "{change:?}");
        }
        let mut flow = ready_flow(DebuggerMode::Disabled);
        assert_eq!(flow.on_press(DebuggerChange::On), Ok(()));
        assert_eq!(flow.on_press(DebuggerChange::On), Err(Refusal::Busy));
        assert_eq!(flow.on_owner_answer(Some(true)), Answer::Consented);
        assert_eq!(flow.on_press(DebuggerChange::On), Err(Refusal::InFlight));
        assert!(flow.on_child_spawned(child(9), 1));
        assert_eq!(flow.on_press(DebuggerChange::On), Err(Refusal::InFlight));
        assert!(flow.on_child_exit());
        assert_eq!(flow.on_press(DebuggerChange::On), Err(Refusal::Busy));
    }

    #[test]
    fn a_decline_or_nobody_asked_spawns_nothing() {
        for answer in [Some(false), None] {
            let mut flow = ready_flow(DebuggerMode::Disabled);
            flow.on_press(DebuggerChange::On).unwrap();
            assert_eq!(flow.on_owner_answer(answer), Answer::Declined);
            assert_eq!(flow.phase(), Phase::Idle);
            assert!(
                !flow.on_child_spawned(child(1), 1),
                "no child without a yes"
            );
            assert_eq!(flow.counts(), (0, 0));
        }
        let mut idle = ready_flow(DebuggerMode::Disabled);
        assert_eq!(idle.on_owner_answer(Some(true)), Answer::NotAsked);
        assert_eq!(idle.counts(), (0, 0));
    }

    /// The lane arms a job ONLY on the owner's yes, carried from the alert:
    /// a request recorded under any other answer launches nothing.
    #[test]
    fn the_lane_arms_a_job_only_on_the_owners_yes() {
        let declines: [fn() -> OwnerAnswer; 3] = [
            || OwnerAnswer::No,
            || OwnerAnswer::Quiet,
            || OwnerAnswer::NotAsked,
        ];
        for decline in declines {
            let answer = format!("{:?}", decline());
            let mut lane = DebuggerChangeLane::with_liveness(|_| false);
            lane.settle_records(RecordsRead::default());
            lane.poll(1);
            lane.observe(probe(DebuggerMode::Disabled), 1);
            assert_eq!(
                lane.begin(DebuggerChange::On, decline()),
                Err(Refusal::NotConfirmed),
                "{answer:?}"
            );
            assert!(!lane.needs_poll(), "{answer:?}: nothing armed");
            assert_eq!(lane.flow().counts(), (0, 0), "{answer:?}");
            assert_eq!(lane.flow().phase(), Phase::Idle, "{answer:?}");
        }
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead::default());
        lane.poll(1);
        lane.observe(probe(DebuggerMode::Disabled), 1);
        assert_eq!(
            lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                .map(|t| t.job()),
            Ok(1)
        );
        assert!(lane.needs_poll());
    }

    /// Only the probe decides: an exit, whatever its code, claims nothing; an
    /// unchanged re-probe is NotChanged, a decline, and nothing retries it.
    #[test]
    fn only_the_re_probe_decides_and_an_unchanged_result_is_a_decline_never_retried() {
        let mut flow = ready_flow(DebuggerMode::Disabled);
        flow.on_press(DebuggerChange::On).unwrap();
        flow.on_owner_answer(Some(true));
        flow.on_child_spawned(child(3), 10);
        flow.on_child_exit();
        assert!(!flow.claim(), "an exit is not an observation");
        assert_eq!(flow.phase(), Phase::Exited(DebuggerChange::On));
        let pending = DebuggerProbe::pending();
        assert_eq!(flow.on_probe(pending), None, "a pending probe is no probe");
        assert_eq!(flow.phase(), Phase::Exited(DebuggerChange::On));
        let outcome = flow.on_probe(probe(DebuggerMode::Disabled)).unwrap();
        assert_eq!(outcome, Outcome::NotChanged(DebuggerChange::On));
        assert!(outcome.counts_as_decline());
        assert_eq!(flow.phase(), Phase::Idle);
        assert_eq!(flow.counts(), (1, 1), "not retried");
        // Changed, and each other reading, by name.
        for (read, want) in [
            (DebuggerMode::Enabled, Outcome::Changed(DebuggerChange::On)),
            (
                DebuggerMode::Unrecognized,
                Outcome::Unrecognized(DebuggerChange::On),
            ),
            (
                DebuggerMode::Unknown,
                Outcome::Unobserved(DebuggerChange::On),
            ),
        ] {
            let mut flow = ready_flow(DebuggerMode::Disabled);
            flow.on_press(DebuggerChange::On).unwrap();
            flow.on_owner_answer(Some(true));
            flow.on_child_spawned(child(3), 10);
            flow.on_child_exit();
            assert_eq!(flow.on_probe(probe(read)), Some(want));
            assert!(!want.counts_as_decline() || read == DebuggerMode::Disabled);
            assert_eq!(flow.claim(), read == DebuggerMode::Enabled);
        }
    }

    #[test]
    fn the_host_gates_do_not_require_a_hardened_aterm() {
        let open = BeginInputs {
            windowed: true,
            macos: true,
            privacy: true,
            self_hardened: SelfHardening::Hardened,
        };
        for hardening in [
            SelfHardening::Hardened,
            SelfHardening::NotHardened,
            SelfHardening::Unknown,
        ] {
            assert_eq!(
                admit_begin(BeginInputs {
                    self_hardened: hardening,
                    ..open
                }),
                Ok(()),
                "O8: {hardening:?}"
            );
        }
        assert_eq!(
            admit_begin(BeginInputs {
                windowed: false,
                ..open
            }),
            Err(Refusal::NoWindow)
        );
        assert_eq!(
            admit_begin(BeginInputs {
                macos: false,
                ..open
            }),
            Err(Refusal::NotMacos)
        );
        assert_eq!(
            admit_begin(BeginInputs {
                privacy: false,
                ..open
            }),
            Err(Refusal::PrivacyOff)
        );
    }

    #[test]
    fn the_inert_arm_starts_nothing() {
        let slot = Arc::new(ChangeSlot::default());
        slot.arm(1);
        let mut inert = job(DebuggerChange::On, 1, &slot);
        inert.wake = Arc::new(|| panic!("the inert arm posts nothing"));
        let gestures = crate::native_settings::ConsentGestures::for_instance(true);
        assert_eq!(gestures.run_devtools(inert), DevtoolsSpawn::Inert);
        assert_eq!(slot.take(1), SlotRead::default());
        let mut inert = job(DebuggerChange::Off, 1, &slot);
        inert.wake = Arc::new(|| panic!("the inert arm posts nothing"));
        assert_eq!(
            crate::native_settings::ConsentGestures::default().run_devtools(inert),
            DevtoolsSpawn::Inert
        );
    }

    /// The worker body, with a stand-in runner: it holds the change lock for
    /// the whole run, publishes the child the moment it starts (and marks
    /// it), then the result, leaves the marker for the park, and nudges once
    /// per event — a runner that panics still reads as gone, and a lock or a
    /// live marker held elsewhere runs nothing at all.
    #[test]
    fn the_worker_holds_the_lock_publishes_the_child_then_the_result_and_leaves_the_marker() {
        let dir = scratch("worker");
        let config = dir.join("aterm.toml");
        let slot = Arc::new(ChangeSlot::default());
        slot.arm(3);
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let mut marked = job(DebuggerChange::On, 3, &slot);
        marked.config_path = Some(config.clone());
        marked.wake = Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let seen = std::cell::Cell::new((false, false));
        run_devtools_job(marked, |change, gate, on_spawn| {
            assert_eq!((change, gate), (DebuggerChange::On, ProbeGate::on()));
            let locked = matches!(try_change_lock(&config), LockTry::Busy);
            on_spawn(child(77));
            let names = read_debugger_marker(&config, CHANGE_MARKER)
                .and_then(|t| ChangeMarker::parse(&t))
                .is_some_and(|m| m.child == child(77) && m.change == DebuggerChange::On);
            seen.set((locked, names));
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        assert_eq!(
            seen.get(),
            (true, true),
            "locked, and the marker names the child"
        );
        assert!(
            matches!(try_change_lock(&config), LockTry::Held(_)),
            "the lock is free, in one try, once the worker ends"
        );
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "the marker waits for the park to take the result"
        );
        let read = slot.take(3);
        assert_eq!(read.spawned, Some(child(77)));
        assert!(matches!(
            read.result,
            Some(DevtoolsChangeRun::Exited { .. })
        ));
        assert!(read.gone);
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A live child named by that marker (a holder that quit mid-change):
        // the next worker runs nothing.
        slot.arm(4);
        let mut over_live = job(DebuggerChange::On, 4, &slot);
        over_live.config_path = Some(config.clone());
        over_live.alive = |c| c.pid == 77;
        run_devtools_job(over_live, |_, _, _| {
            panic!("nothing runs over a live change")
        });
        assert_eq!(slot.take(4).refused, Some(WorkerRefusal::PeerChanging));

        // A lock held elsewhere: nothing runs.
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("free");
        };
        slot.arm(5);
        let mut blocked = job(DebuggerChange::On, 5, &slot);
        blocked.config_path = Some(config.clone());
        run_devtools_job(blocked, |_, _, _| panic!("nothing runs under a held lock"));
        let refused = slot.take(5);
        assert_eq!(refused.refused, Some(WorkerRefusal::PeerChanging));
        assert!(refused.gone && refused.spawned.is_none() && refused.result.is_none());
        drop(held);

        // A later job's slot drops an older job's publications.
        slot.arm(6);
        slot.publish_result(5, DevtoolsChangeRun::SpawnFailed);
        slot.publish_refused(5, WorkerRefusal::NoLock);
        assert_eq!(slot.take(6), SlotRead::default());
        // A worker that panics mid-run still sets `gone` for the park.
        slot.arm(7);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_devtools_job(job(DebuggerChange::Off, 7, &slot), |_, _, _| {
                panic!("stand-in runner fails")
            });
        }));
        assert!(panicked.is_err());
        assert_eq!(
            slot.take(7),
            SlotRead {
                gone: true,
                ..SlotRead::default()
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_records_only_transitions_names_no_cause_and_never_asks_to_re_enable() {
        let mut watch = DriftWatch::default();
        // Seen before the marker is read: judged once it is.
        assert_eq!(
            watch.on_observed(DebuggerMode::Disabled, None, 1),
            DriftStep::default()
        );
        let was_on = ObservedMarker {
            mode: DebuggerMode::Enabled,
            since: 100,
            origin: Origin::Aterm,
        };
        let step = watch.seed(Some(was_on), 200, false);
        assert_eq!(step.record, Some(DebuggerRecord::WasOnNowOff));
        assert_eq!(step.write.map(|m| m.mode), Some(DebuggerMode::Disabled));
        assert_eq!(watch.drift_since(), Some(100));
        // The same mode again: nothing, no write.
        assert_eq!(
            watch.on_observed(DebuggerMode::Disabled, None, 300),
            DriftStep::default()
        );
        // Unrecognized and unknown are not drift.
        for mode in [DebuggerMode::Unrecognized, DebuggerMode::Unknown] {
            assert_eq!(watch.on_observed(mode, None, 301), DriftStep::default());
        }
        // Turned on from aterm: "Debugger access on", origin aterm, drift cleared.
        let on = watch.on_observed(DebuggerMode::Enabled, Some(DebuggerChange::On), 400);
        assert_eq!(on.record, Some(DebuggerRecord::On));
        assert_eq!(on.write.map(|m| m.origin), Some(Origin::Aterm));
        assert_eq!(watch.drift_since(), None);
        // Turned off from aterm: "Debugger access off", not drift.
        let off = watch.on_observed(DebuggerMode::Disabled, Some(DebuggerChange::Off), 500);
        assert_eq!(off.record, Some(DebuggerRecord::Off));
        assert_eq!(watch.drift_since(), None);
        // Turned on elsewhere: judged once the shared record is read again,
        // and still off there: "Debugger access on, set outside aterm".
        let on_disk = watch.recorded();
        assert_eq!(
            watch.on_observed(DebuggerMode::Enabled, None, 600),
            DriftStep::default()
        );
        assert!(!watch.seeded(), "it waits for a fresh read");
        let outside = watch.seed(on_disk, 601, false);
        assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
        assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
        // First sight, no marker: recorded, no record (no transition was seen).
        let mut fresh = DriftWatch::default();
        assert_eq!(fresh.seed(None, 1, false), DriftStep::default());
        assert_eq!(
            fresh.on_observed(DebuggerMode::Enabled, None, 2),
            DriftStep::default()
        );
        let first = fresh.seed(None, 3, false);
        assert_eq!(first.record, None);
        assert_eq!(first.write.map(|m| m.origin), Some(Origin::Unknown));
    }

    /// Two windowed aterms share one config (O8: the installed app and a
    /// development build). A change made from one is recorded by it as
    /// aterm's; the other, idle, reads the new mode and must judge it against
    /// that shared record — never file it as "set outside aterm" or drift,
    /// nor overwrite the peer's `aterm` origin. A change neither of them made
    /// is still recorded as outside.
    #[test]
    fn a_peer_aterms_change_is_judged_against_the_shared_record() {
        let dir = scratch("peer-drift");
        let config = dir.join("aterm.toml");
        let write = |marker: ObservedMarker| {
            write_debugger_marker(&config, OBSERVED_MARKER, &marker.encode()).unwrap();
        };
        let reread = |lane: &mut DebuggerChangeLane, now: u64| {
            assert!(lane.records_due(now), "a fresh read of the record is asked");
            lane.settle_records(read_records(&config, |_| false));
            lane.poll(now)
        };
        write(ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 100,
            origin: Origin::Unknown,
        });
        let mut b = DebuggerChangeLane::with_liveness(|_| false);
        reread(&mut b, 200);
        assert_eq!(
            b.observe(probe(DebuggerMode::Disabled), 201),
            LaneStep::default()
        );
        assert!(!b.records_due(201), "nothing new: nothing to read");
        // The peer turns it on and records it (its re-probe's write).
        write(ObservedMarker {
            mode: DebuggerMode::Enabled,
            since: 302,
            origin: Origin::Aterm,
        });
        let seen = b.observe(probe(DebuggerMode::Enabled), 400);
        assert_eq!(
            seen.drift,
            DriftStep::default(),
            "not judged from a stale copy"
        );
        assert!(b.wants_poll(), "the park takes the read it asks for");
        assert_eq!(
            reread(&mut b, 400).drift,
            DriftStep::default(),
            "already on record"
        );
        let facts = b.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
        assert_eq!(facts.recorded.map(|m| m.origin), Some(Origin::Aterm));
        assert_eq!(
            read_debugger_marker(&config, OBSERVED_MARKER)
                .and_then(|text| ObservedMarker::parse(&text))
                .map(|m| (m.since, m.origin)),
            Some((302, Origin::Aterm)),
            "the peer's record stands"
        );
        // The peer turns it off again: aterm's change, not drift.
        write(ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 600,
            origin: Origin::Aterm,
        });
        assert_eq!(
            b.observe(probe(DebuggerMode::Disabled), 700).drift,
            DriftStep::default()
        );
        assert_eq!(reread(&mut b, 700).drift, DriftStep::default());
        let facts = b.facts(true, probe(DebuggerMode::Disabled), SelfHardening::Hardened);
        assert_eq!(facts.drift_since, None, "no \"it is off now\" line");
        // Turned on outside every aterm: the record still says off.
        assert_eq!(
            b.observe(probe(DebuggerMode::Enabled), 800).drift,
            DriftStep::default()
        );
        let outside = reread(&mut b, 800).drift;
        assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
        assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE RESULT-TO-RECORD WINDOW. Aterm A's change child exits; A takes the
    /// result, re-probes, and only then records what the re-probe read. Peer
    /// B, idle on the same config, sees the new mode inside that window and
    /// re-reads the shared records. It must find A's change marker (its child
    /// dead) and re-read the change as aterm's — never no marker beside the
    /// old record, which it would file as "set outside aterm" and keep
    /// showing. The marker goes once the change is recorded.
    #[test]
    fn a_peer_reading_between_a_changes_result_and_its_record_files_it_as_aterms() {
        let dir = scratch("result-window");
        let config = dir.join("aterm.toml");
        // What `App::apply_debugger_step` does with a step's markers.
        let apply = |step: &LaneStep| {
            settle_markers(
                &config,
                step.drift.write,
                step.remove_change_marker.as_ref(),
            );
        };
        let reread = |lane: &mut DebuggerChangeLane, now: u64| {
            lane.settle_records(read_records(&config, |_| false));
            let step = lane.poll(now);
            apply(&step);
            step
        };
        let before = ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 100,
            origin: Origin::Unknown,
        };
        write_debugger_marker(&config, OBSERVED_MARKER, &before.encode()).unwrap();
        let mut a = DebuggerChangeLane::with_liveness(|_| false);
        let mut b = DebuggerChangeLane::with_liveness(|_| false);
        for lane in [&mut a, &mut b] {
            reread(lane, 200);
            apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
        }
        // A's change: the real worker body writes the marker under the lock,
        // and its child exits with the rule turned on.
        let ticket = a
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        let worker = DevtoolsJob::new(
            ticket,
            ProbeGate::on(),
            a.slot(),
            Some(config.clone()),
            Arc::new(|| {}),
            |_| false,
        );
        run_devtools_job(worker, |_, _, on_spawn| {
            on_spawn(child(41));
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        apply(&a.poll(300));
        assert!(a.awaiting_probe(), "A took the result; its re-probe is out");

        // THE WINDOW: B sees the rule on and judges it against a fresh read.
        apply(&b.observe(probe(DebuggerMode::Enabled), 301));
        assert!(b.records_due(301), "B asks for the shared records");
        let read = reread(&mut b, 301);
        assert_ne!(
            read.drift.record,
            Some(DebuggerRecord::OnOutside),
            "A's change is not \"set outside aterm\""
        );
        assert!(
            b.awaiting_probe(),
            "B found A's marker, its child gone, and re-reads that change as aterm's"
        );
        let b_ended = b.observe(probe(DebuggerMode::Enabled), 302);
        apply(&b_ended);
        assert_eq!(b_ended.drift.record, Some(DebuggerRecord::On));
        let a_ended = a.observe(probe(DebuggerMode::Enabled), 303);
        apply(&a_ended);
        assert_eq!(a_ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
        assert_eq!(a_ended.remove_change_marker, Some(child(41)));
        for lane in [&a, &b] {
            let facts = lane.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
            assert_eq!(facts.recorded.map(|m| m.origin), Some(Origin::Aterm));
        }
        assert_eq!(
            read_debugger_marker(&config, OBSERVED_MARKER)
                .and_then(|text| ObservedMarker::parse(&text))
                .map(|m| (m.mode, m.origin)),
            Some((DebuggerMode::Enabled, Origin::Aterm))
        );
        assert!(
            !dir.join(CHANGE_MARKER).exists(),
            "the marker went once the change was recorded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A released change is not adopted again. The re-probe ends the change
    /// and lets its marker go, but the removal runs on another thread; a
    /// records read in between still finds this aterm's own dead marker. The
    /// lane follows the child it released, so it neither re-opens the change
    /// (a second outcome, a second decline, a second re-probe) nor asks to
    /// remove it twice.
    #[test]
    fn a_released_changes_marker_read_before_its_removal_is_not_adopted_again() {
        let dir = scratch("released-marker");
        let config = dir.join("aterm.toml");
        let apply = |step: &LaneStep| {
            settle_markers(
                &config,
                step.drift.write,
                step.remove_change_marker.as_ref(),
            );
        };
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &ObservedMarker {
                mode: DebuggerMode::Disabled,
                since: 100,
                origin: Origin::Unknown,
            }
            .encode(),
        )
        .unwrap();
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(read_records(&config, |_| false));
        apply(&lane.poll(200));
        apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
        let ticket = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        let worker = DevtoolsJob::new(
            ticket,
            ProbeGate::on(),
            lane.slot(),
            Some(config.clone()),
            Arc::new(|| {}),
            |_| false,
        );
        run_devtools_job(worker, |_, _, on_spawn| {
            on_spawn(child(81));
            // The owner cancelled macOS's dialog: nothing changed.
            DevtoolsChangeRun::Exited {
                code: Some(1),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        apply(&lane.poll(300));
        let ended = lane.observe(probe(DebuggerMode::Disabled), 301);
        assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
        assert_eq!(ended.remove_change_marker, Some(child(81)));
        // The removal has not run yet: a records read finds the marker.
        let out = lane.ask_records();
        let read = read_records(&config, |_| false);
        assert_eq!(read.stale.map(|m| m.child), Some(child(81)));
        *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
        let again = lane.poll(302);
        assert_eq!(again.outcome, None, "no second outcome");
        assert_eq!(
            again.remove_change_marker, None,
            "the removal already asked for is not asked again"
        );
        assert!(
            !lane.awaiting_probe(),
            "the finished change is not re-opened"
        );
        apply(&ended);
        assert!(!dir.join(CHANGE_MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A records read out while this aterm's own change runs can find that
    /// change's marker, its child already gone, before the park has read the
    /// child from the result slot (a drift re-read asked just before the
    /// press, and a `DevToolsSecurity` that exits at once). One park re-read
    /// takes both: the marker is this change's own, so it stays until the
    /// re-probe is recorded — never removed as a stranger's dead marker.
    #[test]
    fn a_records_read_naming_the_jobs_own_exited_child_keeps_its_marker() {
        let dir = scratch("own-marker");
        let config = dir.join("aterm.toml");
        let apply = |step: &LaneStep| {
            settle_markers(
                &config,
                step.drift.write,
                step.remove_change_marker.as_ref(),
            );
        };
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &ObservedMarker {
                mode: DebuggerMode::Disabled,
                since: 100,
                origin: Origin::Unknown,
            }
            .encode(),
        )
        .unwrap();
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(read_records(&config, |_| false));
        apply(&lane.poll(200));
        apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
        // A records re-read is asked; its reader runs after the worker.
        let out = lane.ask_records();
        let ticket = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        let worker = DevtoolsJob::new(
            ticket,
            ProbeGate::on(),
            lane.slot(),
            Some(config.clone()),
            Arc::new(|| {}),
            |_| false,
        );
        run_devtools_job(worker, |_, _, on_spawn| {
            on_spawn(child(71));
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        let read = read_records(&config, |_| false);
        assert_eq!(read.stale.map(|m| m.child), Some(child(71)));
        *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);

        let step = lane.poll(300);
        apply(&step);
        assert_eq!(
            step.remove_change_marker, None,
            "the change's own marker is not a stranger's"
        );
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "it outlives the taken result"
        );
        assert!(lane.awaiting_probe(), "the change waits for its re-probe");
        let ended = lane.observe(probe(DebuggerMode::Enabled), 301);
        apply(&ended);
        assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
        assert_eq!(ended.drift.record, Some(DebuggerRecord::On));
        assert_eq!(ended.remove_change_marker, Some(child(71)));
        assert!(
            !dir.join(CHANGE_MARKER).exists(),
            "and goes once the re-probe is recorded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A re-probe that lands while the drift watch waits for a fresh records
    /// read is judged at that read, and the change's marker waits with it: a
    /// peer re-reading the records in between finds the marker and files the
    /// change as aterm's, where the record the peer itself last wrote (the
    /// rule seen off) would have it filed as "set outside aterm".
    #[test]
    fn a_re_probe_judged_at_a_pending_records_read_keeps_the_marker_until_then() {
        let dir = scratch("drift-pending");
        let config = dir.join("aterm.toml");
        let apply = |step: &LaneStep| {
            settle_markers(
                &config,
                step.drift.write,
                step.remove_change_marker.as_ref(),
            );
        };
        let reread = |lane: &mut DebuggerChangeLane, now: u64| {
            lane.settle_records(read_records(&config, |_| false));
            let step = lane.poll(now);
            apply(&step);
            step
        };
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &ObservedMarker {
                mode: DebuggerMode::Enabled,
                since: 100,
                origin: Origin::Unknown,
            }
            .encode(),
        )
        .unwrap();
        let mut a = DebuggerChangeLane::with_liveness(|_| false);
        let mut b = DebuggerChangeLane::with_liveness(|_| false);
        for lane in [&mut a, &mut b] {
            reread(lane, 200);
            apply(&lane.observe(probe(DebuggerMode::Enabled), 201));
        }
        // The rule is turned off outside aterm; B sees it first and records it.
        apply(&b.observe(probe(DebuggerMode::Disabled), 210));
        assert!(b.records_due(210));
        assert_eq!(
            reread(&mut b, 210).drift.record,
            Some(DebuggerRecord::WasOnNowOff)
        );
        // A sees it too and asks for the records; its reader is slow.
        apply(&a.observe(probe(DebuggerMode::Disabled), 211));
        assert!(a.records_due(211), "A judges it against a fresh read");
        let out = a.ask_records();
        // Meanwhile the owner turns it back on from A.
        let ticket = a
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        let worker = DevtoolsJob::new(
            ticket,
            ProbeGate::on(),
            a.slot(),
            Some(config.clone()),
            Arc::new(|| {}),
            |_| false,
        );
        run_devtools_job(worker, |_, _, on_spawn| {
            on_spawn(child(61));
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        apply(&a.poll(300));
        let ended = a.observe(probe(DebuggerMode::Enabled), 301);
        apply(&ended);
        assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
        assert_eq!(ended.drift.write, None, "judged at the pending read");
        assert_eq!(
            ended.remove_change_marker, None,
            "the marker waits for that read too"
        );
        assert!(dir.join(CHANGE_MARKER).exists());

        // B re-reads in that window: A's marker, its child gone.
        apply(&b.observe(probe(DebuggerMode::Enabled), 302));
        let read = reread(&mut b, 302);
        assert_ne!(
            read.drift.record,
            Some(DebuggerRecord::OnOutside),
            "A's change is not \"set outside aterm\""
        );
        assert!(b.awaiting_probe(), "B re-reads it as aterm's change");
        let b_ended = b.observe(probe(DebuggerMode::Enabled), 303);
        apply(&b_ended);
        assert_eq!(b_ended.drift.record, Some(DebuggerRecord::On));

        // A's read answers: the watch judges, and the marker may go.
        *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read_records(&config, |_| false));
        let settled = a.poll(304);
        apply(&settled);
        assert_eq!(settled.remove_change_marker, Some(child(61)));
        let facts = a.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
        assert_eq!(
            facts.recorded.map(|m| (m.mode, m.origin)),
            Some((DebuggerMode::Enabled, Origin::Aterm))
        );
        assert!(!dir.join(CHANGE_MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A change whose child finished while no aterm was reading its result
    /// (aterm quit during the dialog; a frozen predecessor's worker finished
    /// before the successor read the marker) is still aterm's: the re-probe
    /// records it as the owner's change from aterm, never as a change made
    /// outside aterm or as drift — also when a probe landed before the records
    /// did (the Security page restored at launch, a `privacy` read, or a
    /// records reader that waited out a peer's lock).
    #[test]
    fn a_stale_change_marker_is_re_read_as_this_aterms_change() {
        for ((change, before, after, want), early) in [
            (
                DebuggerChange::On,
                DebuggerMode::Disabled,
                DebuggerMode::Enabled,
                DebuggerRecord::On,
            ),
            (
                DebuggerChange::Off,
                DebuggerMode::Enabled,
                DebuggerMode::Disabled,
                DebuggerRecord::Off,
            ),
        ]
        .into_iter()
        .flat_map(|case| [(case, false), (case, true)])
        {
            let mut lane = DebuggerChangeLane::with_liveness(|_| false);
            if early {
                assert_eq!(
                    lane.observe(probe(after), 150).drift,
                    DriftStep::default(),
                    "{change:?}: nothing is judged before the records"
                );
            }
            let stale = ChangeMarker::new(change, child(31), 1).unwrap();
            lane.settle_records(RecordsRead {
                stale: Some(stale),
                observed: Some(ObservedMarker {
                    mode: before,
                    since: 100,
                    origin: Origin::Unknown,
                }),
                ..RecordsRead::default()
            });
            let step = lane.poll(200);
            assert!(step.invalidate_probe, "{change:?}");
            assert_eq!(
                step.drift,
                DriftStep::default(),
                "{change:?} (early {early}): the change's own re-probe judges it, never a probe from before"
            );
            assert_eq!(
                step.remove_change_marker, None,
                "{change:?}: its marker stays until its change is recorded"
            );
            assert!(lane.awaiting_probe(), "{change:?}: only a probe ends it");
            assert!(lane.needs_poll(), "{change:?}");
            let ended = lane.observe(probe(after), 300);
            assert_eq!(ended.outcome, Some(Outcome::Changed(change)));
            assert_eq!(
                ended.remove_change_marker,
                Some(child(31)),
                "{change:?}: and goes after the record that step writes"
            );
            assert_eq!(ended.drift.record, Some(want), "{change:?} (early {early})");
            assert_eq!(
                ended.drift.write.map(|m| m.origin),
                Some(Origin::Aterm),
                "{change:?} (early {early})"
            );
            assert_eq!(lane.drift.drift_since(), None, "{change:?}: no drift line");
        }
        // Read unchanged: the owner's decline, never retried.
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead {
            stale: ChangeMarker::new(DebuggerChange::On, child(32), 1),
            ..RecordsRead::default()
        });
        lane.poll(1);
        let ended = lane.observe(probe(DebuggerMode::Disabled), 2);
        assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
    }

    /// THE `WF(Notice)` SITE, lane level (design §5.4
    /// `a_lost_debugger_result_is_re_read`): while a child is in flight the
    /// lane keeps the park re-reading; a result published after the child was
    /// taken, whose wake was dropped, is found by the next re-read; and a
    /// predecessor's child that died with an empty slot is re-read too.
    #[test]
    fn a_lost_debugger_result_is_re_read() {
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead::default());
        assert!(lane.needs_poll(), "the records are waiting");
        assert_eq!(lane.poll(1), LaneStep::default());
        assert!(!lane.needs_poll());
        assert_eq!(
            lane.next_deadline(Instant::now()),
            None,
            "idle parks forever"
        );
        lane.observe(probe(DebuggerMode::Disabled), 2);
        // First sight is judged against a fresh read of the shared record.
        assert!(lane.records_due(2));
        lane.settle_records(RecordsRead::default());
        assert_eq!(
            lane.poll(2).drift.write.map(|m| m.mode),
            Some(DebuggerMode::Disabled)
        );
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        let now = Instant::now();
        assert_eq!(lane.next_deadline(now), Some(now + RESULT_RECHECK));
        // The child started and the park read it: in flight, and STILL
        // re-reading every RESULT_RECHECK — the wake of its exit may never come.
        let slot = lane.slot();
        slot.publish_spawned(job, child(11));
        assert_eq!(lane.poll(3), LaneStep::default());
        assert!(lane.flow().in_flight().is_some());
        let now = Instant::now();
        assert_eq!(
            lane.next_deadline(now),
            Some(now + RESULT_RECHECK),
            "a child in flight keeps the park re-reading"
        );
        assert!(!lane.result_unread());
        // The child exited and the worker published; no wake arrived.
        slot.publish_result(
            job,
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            },
        );
        slot.worker_gone(job);
        assert!(lane.result_unread(), "the result waits unread");
        let step = lane.poll(4);
        assert!(
            step.invalidate_probe,
            "the rule may have changed under the cache"
        );
        assert_eq!(
            step.remove_change_marker, None,
            "the marker outlives the taken result until the change is recorded"
        );
        assert!(lane.awaiting_probe(), "only a probe ends it");
        assert!(
            lane.needs_poll(),
            "and the park keeps re-reading until it does"
        );
        let ended = lane.observe(probe(DebuggerMode::Enabled), 5);
        assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
        assert_eq!(ended.drift.record, Some(DebuggerRecord::On));
        assert_eq!(
            ended.remove_change_marker,
            Some(child(11)),
            "it goes with the record, and only if it names the child"
        );
        assert!(!lane.needs_poll());

        // A predecessor's child, alive at launch, dead now, nothing in the slot.
        static ALIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        let mut successor =
            DebuggerChangeLane::with_liveness(|_| ALIVE.load(std::sync::atomic::Ordering::SeqCst));
        let marker = ChangeMarker::new(DebuggerChange::On, child(12), 5).unwrap();
        let was_off = ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 1,
            origin: Origin::Unknown,
        };
        successor.settle_records(RecordsRead {
            inherited: Some(marker),
            observed: Some(was_off),
            ..RecordsRead::default()
        });
        successor.poll(6);
        assert_eq!(
            successor
                .flow()
                .in_flight()
                .map(|(c, _, _, inherited)| (c, inherited)),
            Some((DebuggerChange::On, true))
        );
        // …and a press is refused while it is: the cross-process OneChild.
        assert_eq!(
            successor.begin(DebuggerChange::On, OwnerAnswer::yes_for_test()),
            Err(Refusal::InFlight)
        );
        assert!(
            !successor.poll(7).invalidate_probe,
            "still running: still in flight"
        );
        ALIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        let step = successor.poll(8);
        assert!(step.invalidate_probe);
        assert_eq!(step.remove_change_marker, None);
        assert!(successor.awaiting_probe());
        let ended = successor.observe(probe(DebuggerMode::Disabled), 9);
        assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
        assert_eq!(ended.remove_change_marker, Some(child(12)));
    }

    #[test]
    fn a_worker_that_ends_without_a_result_is_lost_and_a_not_due_read_is_observed() {
        // Gone before spawning: nothing ran.
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead {
            observed: Some(ObservedMarker {
                mode: DebuggerMode::Disabled,
                since: 1,
                origin: Origin::Unknown,
            }),
            ..RecordsRead::default()
        });
        lane.poll(1);
        lane.observe(probe(DebuggerMode::Disabled), 1);
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        lane.slot().worker_gone(job);
        let step = lane.poll(2);
        assert_eq!(step.outcome, Some(Outcome::NotStarted(DebuggerChange::On)));
        assert_eq!(lane.flow().phase(), Phase::Idle);
        // Gone after spawning: the child is re-read by probe.
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        lane.slot().publish_spawned(job, child(5));
        lane.poll(3);
        lane.slot().worker_gone(job);
        assert_eq!(lane.poll(4).remove_change_marker, None);
        assert!(lane.awaiting_probe());
        assert_eq!(
            lane.observe(probe(DebuggerMode::Disabled), 5)
                .remove_change_marker,
            Some(child(5))
        );
        // Another aterm's change holds the lock: nothing ran here, said so.
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        lane.slot()
            .publish_refused(job, WorkerRefusal::PeerChanging);
        assert_eq!(
            lane.poll(6).outcome,
            Some(Outcome::PeerInFlight(DebuggerChange::On))
        );
        assert_eq!(lane.flow().phase(), Phase::Idle);
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        lane.slot().publish_refused(job, WorkerRefusal::NoLock);
        assert_eq!(
            lane.poll(7).outcome,
            Some(Outcome::NotStarted(DebuggerChange::On))
        );
        // NotDue: the worker's fresh read showed a customised rule; it is the
        // flow's observation now, and the drift watch sees it.
        let job = lane
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap()
            .job();
        lane.slot().publish_result(
            job,
            DevtoolsChangeRun::NotDue {
                observed: probe(DebuggerMode::Unrecognized),
            },
        );
        let step = lane.poll(8);
        assert_eq!(
            step.outcome,
            Some(Outcome::NotDue(
                DebuggerChange::On,
                DebuggerMode::Unrecognized
            ))
        );
        assert!(step.invalidate_probe);
        assert_eq!(lane.flow().observed(), DebuggerMode::Unrecognized);
        assert_eq!(
            lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                .map(|_| ()),
            Err(Refusal::State(
                DebuggerChange::On,
                DebuggerMode::Unrecognized
            ))
        );
    }

    #[test]
    fn the_facts_offer_a_button_only_where_pressing_can_help() {
        let zero = DebuggerFacts::default();
        assert!(!zero.offers(DebuggerChange::On) && !zero.offers(DebuggerChange::Off));
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        let disabled = probe(DebuggerMode::Disabled);
        let unread = lane.facts(true, disabled, SelfHardening::Hardened);
        assert!(
            unread.busy && !unread.offers(DebuggerChange::On),
            "records unread"
        );
        lane.settle_records(RecordsRead::default());
        lane.poll(1);
        let facts = lane.facts(true, disabled, SelfHardening::NotHardened);
        assert!(
            facts.offers(DebuggerChange::On),
            "O8: offered unhardened too"
        );
        assert!(facts.not_hardened());
        assert!(!facts.offers(DebuggerChange::Off));
        assert!(
            !lane
                .facts(false, disabled, SelfHardening::Hardened)
                .offers(DebuggerChange::On)
        );
        let outsider = DebuggerProbe {
            member: DebuggerMember::NotMember,
            ..disabled
        };
        assert!(
            !lane
                .facts(true, outsider, SelfHardening::Hardened)
                .offers(DebuggerChange::On)
        );
        let enabled = probe(DebuggerMode::Enabled);
        let on = lane.facts(
            true,
            DebuggerProbe {
                member: DebuggerMember::NotMember,
                ..enabled
            },
            SelfHardening::Hardened,
        );
        assert!(
            on.offers(DebuggerChange::Off),
            "turning off only reduces exposure"
        );
        for mode in [DebuggerMode::Unrecognized, DebuggerMode::Unknown] {
            let facts = lane.facts(true, probe(mode), SelfHardening::Hardened);
            assert!(!facts.offers(DebuggerChange::On) && !facts.offers(DebuggerChange::Off));
        }
        lane.observe(disabled, 2);
        lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        lane.slot().publish_spawned(1, child(3));
        lane.poll(9);
        let waiting = lane.facts(true, disabled, SelfHardening::Hardened);
        assert!(waiting.busy && !waiting.offers(DebuggerChange::On));
        assert_eq!(
            waiting.in_flight,
            Some(InFlightFacts {
                change: DebuggerChange::On,
                since: 9,
                inherited: false
            })
        );
    }
}

// ---------------------------------------------------------------------------
// Tier-1: the real lane, worker and order against the derived model
// ---------------------------------------------------------------------------

/// TIER-1 `debugger_access_conforms_to_the_model` (design §5.4). Drives the
/// production composition — [`DebuggerChangeLane`] (`begin` with the owner's
/// answer, `poll`, `observe`, `needs_poll`), the worker body
/// [`run_devtools_job`] on its own thread with the real change lock and the
/// real change marker in a scratch config folder, and the real order
/// `aterm_containment::consent::devtools_change_with_for_test` with a scripted
/// `-status` re-read and a stand-in child that waits until the harness lets
/// it exit — and projects it, direction-normalised, onto
/// `aterm_spec::derive::debugger_access_change_model()`. At every step the
/// model's guard must equal what the composition did, and a step it took
/// must land on a successor the model admits.
///
/// Where each variable comes from: `phase`, `obs` and `polling` from the lane
/// (phase 5 is a result the lane has not taken yet); `claim` from what the
/// Security row states over the facts the lane publishes
/// (`native_settings::debugger_row_states`); `lock` from the real `flock` (and
/// the change marker naming a live child); `launches`, `inflight`, `armed`,
/// `unarmed`, `fresh` and `unconsented` from what the stand-in environment
/// SAW happen — each re-read, each spawn, each child alive, each "Allow…" the
/// harness gave — never from the lane's own counters. `world` and `clobbered`
/// are the environment's.
///
/// Two steps are compound, since nothing observable separates their parts:
/// the owner's press-and-answer runs `Press`, `Consent`/`Decline` and — on a
/// yes — the worker's `Recheck` and `Launch` up to the park reading the
/// spawned child; and a probe the lane refuses leaves the row's cached answer
/// alone (in the shipping app the page may show a fresher probe than the lane
/// took while a change is under way; both are observations).
#[cfg(test)]
pub(crate) mod conformance {
    use super::*;
    use crate::native_settings::OwnerAnswer;
    use aterm_spec::derive::{Model, debugger_access_change_model};
    use aterm_spec::interp::State;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
    use std::sync::mpsc;

    /// Normalise a mode by the change: its precondition is 0, its target 1, an
    /// unrecognised rule 2, and not read 3.
    pub(crate) fn norm(mode: DebuggerMode, change: DebuggerChange) -> i64 {
        match mode {
            m if m == change.precondition() => 0,
            m if m == change.target() => 1,
            DebuggerMode::Unrecognized => 2,
            _ => 3,
        }
    }

    /// The argv each direction must run, spelled here independently of the
    /// containment crate's private builders: the second oracle for the order.
    fn change_argv(change: DebuggerChange) -> [&'static str; 2] {
        match change {
            DebuggerChange::On => ["/usr/sbin/DevToolsSecurity", "-enable"],
            DebuggerChange::Off => ["/usr/sbin/DevToolsSecurity", "-disable"],
        }
    }

    fn denorm(world: i64, change: DebuggerChange) -> DebuggerMode {
        match world {
            0 => change.precondition(),
            1 => change.target(),
            _ => DebuggerMode::Unrecognized,
        }
    }

    /// The stand-in children alive on "this Mac", by pid: the lane's liveness
    /// check, shared with the worker threads (a plain `fn`, as the shipping
    /// `DevtoolsChild::is_running` is). Every harness uses its own pids.
    static ALIVE: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
    static NEXT_PID: AtomicU32 = AtomicU32::new(3_000_000);

    fn alive(child: &DevtoolsChild) -> bool {
        ALIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|set| set.contains(&child.pid))
    }

    fn set_alive(pid: u32, living: bool) {
        let mut set = ALIVE.lock().unwrap_or_else(|p| p.into_inner());
        let set = set.get_or_insert_with(HashSet::new);
        if living {
            set.insert(pid);
        } else {
            set.remove(&pid);
        }
    }

    fn stand_in() -> DevtoolsChild {
        DevtoolsChild {
            pid: NEXT_PID.fetch_add(1, Ordering::SeqCst),
            birth: Some(ProcessBirth {
                seconds: 1_790_000_000,
                microseconds: 1,
            }),
        }
    }

    /// What the stand-in environment saw a worker do.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        /// The order's `-status` re-read, normalised.
        Read(i64),
        /// A change child started with this argv.
        Spawn(Vec<String>),
    }

    /// The environment the worker threads share with the harness.
    struct Env {
        change: DebuggerChange,
        /// The rule, normalised.
        world: AtomicI64,
        log: Mutex<Vec<Event>>,
        /// 0 running, 1 blocked in its child, 2 ended.
        stage: AtomicI64,
    }

    /// The harness's own accounting, from what it saw and what it gave.
    #[derive(Clone, Copy, Debug, Default)]
    struct Seen {
        clobbered: i64,
        fresh: i64,
        launches: i64,
        unconsented: i64,
        armed: i64,
        unarmed: i64,
        misadopted: i64,
    }

    /// The projection's inputs besides the lane.
    pub(crate) struct Observed<'a> {
        world: i64,
        cached: DebuggerProbe,
        lock: i64,
        inflight: i64,
        /// A change marker on disk naming a dead child the lane does not
        /// follow.
        stale: i64,
        seen: &'a Seen,
    }

    /// THE PROJECTION (the anchors' `project`).
    ///
    /// The model's other actions are the environment's or the `Buggy = 1`
    /// mutants', waived here by name; each mutant's negative control runs the
    /// harness over a lane or order carrying that defect and requires it to
    /// fail (`the_harness_fails_every_faulty_composition`).
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Drift",
        reason = "Environment: an administrator, a macOS upgrade or a -disable elsewhere moves \
                  the rule back to the change's precondition. The Tier-1 harness moves `world`; \
                  the shipping drift watch only records what a probe reads and never re-enables."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SetOutside",
        reason = "Environment: the rule reaches the change's target outside any child of \
                  aterm's. The Tier-1 harness moves `world`; the worker's re-read then reads \
                  NotDue and spawns nothing."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Customize",
        reason = "Environment: a rule in a shape Apple's tool does not recognise. The Tier-1 \
                  harness moves `world`; the worker's re-read then reads NotDue and spawns nothing."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Normalize",
        reason = "Environment: a recognised rule written back. The Tier-1 harness moves `world`."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PeerLaunch",
        reason = "Environment: another aterm sharing the config takes the change lock and \
                  starts its child. The Tier-1 harness takes the real flock and names a live \
                  child in the real change marker; the lane's worker then runs nothing."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PeerExit",
        reason = "Environment: the peer's child exits and its worker releases the lock. The \
                  Tier-1 harness drops the flock."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PressWhileInFlight",
        reason = "Buggy = 1 mutant: the lane refuses a press while a child is in flight \
                  (on_press: InFlight); the negative control forces one through and the \
                  harness fails."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SkipRecheck",
        reason = "Buggy = 1 mutant: the only order, devtools_change_with, re-reads -status \
                  before it spawns; the negative control runs an order that does not, and the \
                  harness fails."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "IgnorePeerLock",
        reason = "Buggy = 1 mutant: run_devtools_job takes the change lock before the re-read \
                  and holds it through the child; the negative control runs a worker that takes \
                  no lock, and the harness fails (the projected lock is not held)."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "ClaimFromExitCode",
        reason = "Buggy = 1 mutant: the reducer's observation and the row's claim come only \
                  from a probe; the negative control makes the lane take the exit as the \
                  target, and the harness fails."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "RetryAfterCancel",
        reason = "Buggy = 1 mutant: an unchanged result ends the change (NotChanged) and the \
                  host starts a worker only after the owner's yes; the negative control retries \
                  without one, and the harness fails."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SpendBanked",
        reason = "Buggy = 1 mutant: the lane arms a job only inside begin, on the owner's yes \
                  (the_lane_arms_a_job_only_on_the_owners_yes); a worker started with no \
                  answer is the RetryAfterCancel control's shape."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "AdoptLive",
        reason = "Buggy = 1 mutant: read_records files a change marker as stale only when its \
                  child is gone by pid and birth; the negative control runs a records reader \
                  that skips that check over a peer's running child, and the harness fails."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "ResultWakeLost",
        reason = "Buggy = 1 mutant: the lane's needs_poll holds while a change is under way, \
                  whatever the wake did; the negative control drops the re-read while a child \
                  is in flight and the harness fails."
    )]
    pub(crate) fn project(lane: &DebuggerChangeLane, observed: &Observed<'_>) -> State {
        let flow = &lane.flow;
        let change = flow.direction();
        let facts = lane.facts(true, observed.cached, SelfHardening::Hardened);
        let claim = crate::native_settings::debugger_row_states(&facts) == Some(change.target());
        let mut state = State::new();
        state.insert(
            "phase",
            match flow.phase() {
                Phase::Idle => 0,
                Phase::Confirming(_) => 1,
                Phase::Rechecking(_) => 2,
                Phase::InFlight { .. } if lane.result_unread() => 5,
                Phase::InFlight { .. } => 4,
                Phase::Exited(_) => 6,
            },
        );
        state.insert("world", observed.world);
        state.insert("obs", norm(flow.observed(), change));
        state.insert("claim", i64::from(claim));
        state.insert("polling", i64::from(lane.needs_poll()));
        state.insert("lock", observed.lock);
        state.insert("fresh", observed.seen.fresh);
        state.insert("launches", observed.seen.launches);
        state.insert("unconsented", observed.seen.unconsented);
        state.insert("inflight", observed.inflight);
        state.insert("armed", observed.seen.armed);
        state.insert("unarmed", observed.seen.unarmed);
        state.insert("clobbered", observed.seen.clobbered);
        state.insert("stale", observed.stale);
        state.insert("misadopted", observed.seen.misadopted);
        state
    }

    fn scripted(mode: DebuggerMode) -> DebuggerProbe {
        DebuggerProbe {
            mode,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

    /// One defect a negative control plants in the composition.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Fault {
        None,
        /// An order that spawns on the render's reading, never re-reading.
        SkipRecheck,
        /// A worker that takes no change lock.
        IgnorePeerLock,
        /// A lane whose press guard lets a press through while in flight.
        PressWhileInFlight,
        /// A lane that takes the child's exit as the change's target.
        ClaimFromExit,
        /// A lane that stops asking the park to re-read while a child runs.
        DropPollInFlight,
        /// A host that retries an unchanged result without asking.
        RetryAfterCancel,
        /// A records reader that takes every marker's child for dead.
        AdoptLive,
    }

    /// The worker thread the harness started, as `ConsentGestures::live`
    /// would: its child's exit is the harness's to give.
    struct Worker {
        exit: mpsc::Sender<DevtoolsChangeRun>,
        handle: std::thread::JoinHandle<()>,
        env: Arc<Env>,
        child: DevtoolsChild,
    }

    /// The harness: the real lane, the environment, and the model state it
    /// must keep agreeing with.
    pub(crate) struct Rig {
        model: Model,
        lane: DebuggerChangeLane,
        change: DebuggerChange,
        fault: Fault,
        dir: PathBuf,
        config: PathBuf,
        world: i64,
        /// The row's cached probe answer (`DebuggerSlot`).
        cached: DebuggerProbe,
        seen: Seen,
        /// Events of the current worker already accounted.
        consumed: usize,
        worker: Option<Worker>,
        /// A peer aterm's held lock and its live child.
        peer: Option<(ChangeLock, DevtoolsChild)>,
        /// The last peer child that exited, whose change that peer has yet
        /// to record.
        peer_gone: Option<DevtoolsChild>,
        /// A predecessor's child, read from its marker at start.
        inherited: Option<DevtoolsChild>,
        state: State,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
                // Dropping the sender lets a blocked stand-in return.
                drop(worker.exit);
                let _ = worker.handle.join();
                set_alive(worker.child.pid, false);
            }
            if let Some((_, child)) = self.peer.take() {
                set_alive(child.pid, false);
            }
            if let Some(child) = self.inherited.take() {
                set_alive(child.pid, false);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn hang_detector(what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "hang: {what} did not happen within a minute"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-debugger-rig-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    impl Rig {
        fn bare(change: DebuggerChange, world: DebuggerMode, fault: Fault) -> Self {
            let dir = scratch();
            let config = dir.join("aterm.toml");
            let mut lane = DebuggerChangeLane::with_liveness(alive);
            lane.flow.direction = change;
            Self {
                model: debugger_access_change_model(),
                lane,
                change,
                fault,
                dir,
                config,
                world: norm(world, change),
                cached: DebuggerProbe::pending(),
                seen: Seen::default(),
                consumed: 0,
                worker: None,
                peer: None,
                peer_gone: None,
                inherited: None,
                state: State::new(),
            }
        }

        /// A fresh lane: records read, nothing observed — the model's
        /// initial state. The shared observed record holds the rule as it is.
        pub(crate) fn new(change: DebuggerChange, world: DebuggerMode, fault: Fault) -> Self {
            let mut rig = Self::bare(change, world, fault);
            rig.record_world();
            rig.lane.settle_records(read_records(&rig.config, alive));
            let step = rig.lane.poll(now_secs());
            rig.apply(step);
            let state = rig.now();
            let mut init = rig.model.init_state();
            init.insert("world", rig.world);
            assert_eq!(state, init, "the fresh lane is the model's initial state");
            rig.state = state;
            rig
        }

        /// A successor whose predecessor's child is still running: its marker
        /// is on disk and read at start. Checked from there on.
        fn inherited(change: DebuggerChange, world: DebuggerMode) -> Self {
            let mut rig = Self::bare(change, world, Fault::None);
            let child = stand_in();
            set_alive(child.pid, true);
            let marker = ChangeMarker::new(change, child, 1).unwrap();
            write_debugger_marker(&rig.config, CHANGE_MARKER, &marker.encode()).unwrap();
            rig.inherited = Some(child);
            rig.seen.launches = 1;
            rig.record_world();
            let records = read_records(&rig.config, alive);
            assert_eq!(records.inherited, Some(marker));
            rig.lane.settle_records(records);
            let step = rig.lane.poll(now_secs());
            rig.apply(step);
            rig.state = rig.now();
            for invariant in &rig.model.invariants {
                assert!(
                    rig.model.check_invariant(invariant.name, &rig.state),
                    "{}: {:?}",
                    invariant.name,
                    rig.state
                );
            }
            rig
        }

        /// An earlier aterm's observed record of the rule as it is now, so the
        /// drift watch judges the change's re-probe at once.
        fn record_world(&self) {
            let marker = ObservedMarker {
                mode: denorm(self.world, self.change),
                since: 1,
                origin: Origin::Unknown,
            };
            write_debugger_marker(&self.config, OBSERVED_MARKER, &marker.encode()).unwrap();
        }

        /// The observed record on disk, mode normalised, and its origin.
        fn recorded(&self) -> Option<(i64, Origin)> {
            read_debugger_marker(&self.config, OBSERVED_MARKER)
                .and_then(|text| ObservedMarker::parse(&text))
                .map(|m| (norm(m.mode, self.change), m.origin))
        }

        fn lock_now(&self) -> i64 {
            if self.peer.is_some() {
                return 2;
            }
            let marker_live = read_debugger_marker(&self.config, CHANGE_MARKER)
                .and_then(|text| ChangeMarker::parse(&text))
                .is_some_and(|marker| alive(&marker.child));
            // One try reads the fact: every holder here lets go by `LOCK_UN`
            // ([`ChangeLock`]), so a child another test forked while the lock
            // was held keeps no copy of it, and a lock that reads busy has a
            // holder — the peer (above), the worker while its child is in
            // flight, or a leaked lock, which fails the step.
            match try_change_lock(&self.config) {
                LockTry::Busy => 1,
                LockTry::Held(_) if marker_live => 1,
                LockTry::Held(_) => 0,
                LockTry::Unavailable(error) => panic!("the scratch lock: {error}"),
            }
        }

        /// Which branch of [`Self::lock_now`] answers, for a failing step.
        fn lock_detail(&self) -> String {
            let marker = read_debugger_marker(&self.config, CHANGE_MARKER)
                .and_then(|text| ChangeMarker::parse(&text));
            let flock = match try_change_lock(&self.config) {
                LockTry::Busy => "busy",
                LockTry::Held(_) => "free",
                LockTry::Unavailable(_) => "unavailable",
            };
            format!(
                "flock {flock}; marker {}",
                marker.map_or_else(
                    || "none".to_string(),
                    |m| format!("names pid {} (alive: {})", m.child.pid, alive(&m.child))
                )
            )
        }

        fn inflight_now(&self) -> i64 {
            let ours = self
                .worker
                .as_ref()
                .map_or(0, |worker| i64::from(alive(&worker.child)));
            let peer = self
                .peer
                .as_ref()
                .map_or(0, |(_, child)| i64::from(alive(child)));
            let inherited = self.inherited.map_or(0, |child| i64::from(alive(&child)));
            ours + peer + inherited
        }

        /// The change marker on disk names a dead child the lane does not
        /// follow.
        fn stale_now(&self) -> i64 {
            i64::from(
                read_debugger_marker(&self.config, CHANGE_MARKER)
                    .and_then(|text| ChangeMarker::parse(&text))
                    .is_some_and(|marker| {
                        !alive(&marker.child) && !self.lane.follows(&marker.child)
                    }),
            )
        }

        fn needs_poll(&self) -> bool {
            let real = self.lane.needs_poll();
            if self.fault == Fault::DropPollInFlight
                && matches!(self.lane.flow.phase(), Phase::InFlight { .. })
            {
                return false;
            }
            real
        }

        fn now(&self) -> State {
            let mut state = project(
                &self.lane,
                &Observed {
                    world: self.world,
                    cached: self.cached,
                    lock: self.lock_now(),
                    inflight: self.inflight_now(),
                    stale: self.stale_now(),
                    seen: &self.seen,
                },
            );
            state.insert("polling", i64::from(self.needs_poll()));
            state
        }

        /// Do what a lane step asks, as `App::apply_debugger_step` does.
        fn apply(&mut self, step: LaneStep) {
            if step.invalidate_probe {
                self.cached = DebuggerProbe::pending();
            }
            settle_markers(
                &self.config,
                step.drift.write,
                step.remove_change_marker.as_ref(),
            );
        }

        fn check_invariants(&self) {
            for invariant in &self.model.invariants {
                assert!(
                    self.model.check_invariant(invariant.name, &self.state),
                    "{}: {:?}",
                    invariant.name,
                    self.state
                );
            }
        }

        /// Fire the model action `action` against the composition's event,
        /// whose return says whether it was taken: the guard must agree, and
        /// a taken step must be one of the model's successors.
        fn step(&mut self, action: &str, event: impl FnOnce(&mut Self) -> bool) {
            let pre = self.now();
            assert_eq!(
                pre, self.state,
                "{action}: the projection drifted from the model"
            );
            let enabled = self.model.action_enabled(action, &pre);
            let taken = event(self);
            assert_eq!(
                taken,
                enabled,
                "{action}: the composition {} what the model {}: {pre:?}",
                if taken { "took" } else { "refused" },
                if enabled { "allows" } else { "forbids" },
            );
            let post = self.now();
            if taken {
                assert!(
                    self.model.successors(action, &pre).contains(&post),
                    "{action}: {pre:?} -> {post:?} is not a step the model admits ({})",
                    self.lock_detail()
                );
                self.state = post;
            } else {
                assert_eq!(post, pre, "{action}: a refusal changed nothing");
            }
            self.check_invariants();
        }

        /// The states `actions` reach from `pre`, each one enabled in turn.
        fn chain(&self, pre: &State, actions: &[&str]) -> Vec<State> {
            let mut states = vec![pre.clone()];
            for action in actions {
                let next: Vec<State> = states
                    .iter()
                    .flat_map(|state| self.model.successors(action, state))
                    .collect();
                assert!(!next.is_empty(), "{action} enabled after {states:?}");
                states = next;
            }
            states
        }

        /// A completed probe of the rule as it is: the lane takes it only
        /// idle or once an exited child's result was read.
        fn probe(&mut self) {
            let read = scripted(denorm(self.world, self.change));
            self.step("Probe", |rig| rig.observe(read));
        }

        /// A probe that failed (timed out).
        fn probe_fails(&mut self) {
            let read = DebuggerProbe {
                mode: DebuggerMode::Unknown,
                member: DebuggerMember::Unknown,
                label: DebuggerProbeLabel::TimedOut,
            };
            self.step("ProbeFails", |rig| rig.observe(read));
        }

        fn observe(&mut self, read: DebuggerProbe) -> bool {
            let accepts = self.lane.flow.accepts_probe();
            let step = self.lane.observe(read, now_secs());
            if accepts {
                self.cached = read;
            }
            let outcome = step.outcome;
            self.apply(step);
            if self.fault == Fault::RetryAfterCancel
                && matches!(outcome, Some(Outcome::NotChanged(_)))
            {
                // THE FAULT: the host starts the change again with no alert.
                let job = self
                    .lane
                    .begin(self.change, OwnerAnswer::yes_for_test())
                    .expect("the retry is taken");
                self.start_worker(job);
                let step = self.lane.poll(now_secs());
                self.apply(step);
                self.account();
            }
            accepts
        }

        /// Start the worker the way `ConsentGestures::live` does, over the
        /// real body, lock and order, and wait until it blocks in its child or
        /// ends.
        fn start_worker(&mut self, ticket: ChangeTicket) {
            assert!(self.worker.is_none(), "one worker at a time in the harness");
            let env = Arc::new(Env {
                change: self.change,
                world: AtomicI64::new(self.world),
                log: Mutex::new(Vec::new()),
                stage: AtomicI64::new(0),
            });
            let child = stand_in();
            let (exit, waits) = mpsc::channel::<DevtoolsChangeRun>();
            let job = DevtoolsJob::new(
                ticket,
                ProbeGate::on(),
                self.lane.slot(),
                (self.fault != Fault::IgnorePeerLock).then(|| self.config.clone()),
                Arc::new(|| {}),
                alive,
            );
            let fault = self.fault;
            let shared = Arc::clone(&env);
            let handle = std::thread::Builder::new()
                .name("rig-consent-devtools".into())
                .spawn(move || {
                    let env = shared;
                    run_devtools_job(job, |change, _gate, on_spawn| {
                        let recheck = || {
                            let world = env.world.load(Ordering::SeqCst);
                            env.log
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(Event::Read(world));
                            scripted(denorm(world, env.change))
                        };
                        let spawn =
                            |argv: &[&str]| {
                                env.log.lock().unwrap_or_else(|p| p.into_inner()).push(
                                    Event::Spawn(argv.iter().map(|a| (*a).to_string()).collect()),
                                );
                                set_alive(child.pid, true);
                                on_spawn(child);
                                env.stage.store(1, Ordering::SeqCst);
                                let exit = waits.recv().unwrap_or(DevtoolsChangeRun::WaitFailed);
                                set_alive(child.pid, false);
                                exit
                            };
                        if fault == Fault::SkipRecheck {
                            // THE FAULT: spawn on the render's reading.
                            spawn(&change_argv(change))
                        } else {
                            aterm_containment::consent::devtools_change_with_for_test(
                                change, recheck, spawn,
                            )
                        }
                    });
                    env.stage.store(2, Ordering::SeqCst);
                })
                .expect("the harness's worker thread");
            let stage = Arc::clone(&env);
            hang_detector("the worker blocking in its child or ending", || {
                stage.stage.load(Ordering::SeqCst) != 0
            });
            self.consumed = 0;
            self.worker = Some(Worker {
                exit,
                handle,
                env,
                child,
            });
        }

        /// Account what the current worker did since the last look.
        fn account(&mut self) {
            let Some(worker) = self.worker.as_ref() else {
                return;
            };
            let events = worker
                .env
                .log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let mut read_precondition = false;
            for event in &events[self.consumed..] {
                match event {
                    Event::Read(world) => read_precondition = *world == 0,
                    Event::Spawn(argv) => {
                        assert_eq!(
                            argv.iter().map(String::as_str).collect::<Vec<_>>(),
                            change_argv(self.change),
                            "the child runs the change's own argv"
                        );
                        self.seen.launches += 1;
                        if !read_precondition {
                            self.seen.unarmed += 1;
                        }
                        if self.seen.fresh == 0 {
                            self.seen.unconsented += 1;
                        }
                        self.seen.fresh = 0;
                        read_precondition = false;
                    }
                }
            }
            self.consumed = events.len();
            self.seen.armed = 0;
            if worker.env.stage.load(Ordering::SeqCst) == 2 {
                // The worker ended: a yes it did not spend is spent now.
                self.seen.fresh = 0;
                if let Some(worker) = self.worker.take() {
                    worker.handle.join().expect("the worker ends");
                }
            }
        }

        /// The owner presses and answers aterm's alert; the host drains the
        /// request into `DebuggerChangeLane::begin` and, on a yes, starts the
        /// worker; the park reads what it did.
        fn press(&mut self, answer: OwnerAnswer) {
            let pre = self.now();
            assert_eq!(
                pre, self.state,
                "press: the projection drifted from the model"
            );
            if self.fault == Fault::PressWhileInFlight
                && matches!(self.lane.flow.phase, Phase::InFlight { .. })
            {
                // THE FAULT: the press guard lets it through.
                self.lane.flow.phase = Phase::Idle;
            }
            let press_enabled = self.model.action_enabled("Press", &pre);
            let answered_yes = matches!(answer, OwnerAnswer::Yes(_));
            let answer_label = format!("{answer:?}");
            let begun = self.lane.begin(self.change, answer);
            let pressed = !matches!(
                begun,
                Err(Refusal::RecordsUnread
                    | Refusal::InFlight
                    | Refusal::Busy
                    | Refusal::State(..))
            );
            assert_eq!(
                pressed,
                press_enabled,
                "Press: the lane {} what the model {}: {pre:?}",
                if pressed { "took" } else { "refused" },
                if press_enabled { "allows" } else { "forbids" },
            );
            let targets = match begun {
                Err(Refusal::NotConfirmed) => self.chain(&pre, &["Press", "Decline"]),
                Err(_) => {
                    assert_eq!(self.now(), pre, "a refused press changed nothing");
                    return;
                }
                Ok(job) => {
                    assert!(answered_yes, "a job only on a yes");
                    self.seen.fresh = 1;
                    self.start_worker(job);
                    let launched = self
                        .worker
                        .as_ref()
                        .is_some_and(|w| w.env.stage.load(Ordering::SeqCst) == 1);
                    // The park reads the spawned child, or the abort.
                    let step = self.lane.poll(now_secs());
                    self.apply(step);
                    self.account();
                    if launched {
                        self.chain(&pre, &["Press", "Consent", "Recheck", "Launch"])
                    } else {
                        self.chain(&pre, &["Press", "Consent", "Recheck"])
                    }
                }
            };
            let post = self.now();
            assert!(
                targets.contains(&post),
                "press ({answer_label}): {pre:?} -> {post:?} is none of {targets:?}"
            );
            self.state = post;
            self.check_invariants();
        }

        /// The child exits: applied, unchanged, or over a customised rule —
        /// ours, or a predecessor's.
        fn exit(&mut self, action: &str) {
            self.step(action, |rig| {
                match action {
                    "ChildApplied" => rig.world = 1,
                    "ChildClobbers" => {
                        rig.world = 1;
                        rig.seen.clobbered = 1;
                    }
                    _ => {}
                }
                if let Some(worker) = rig.worker.as_ref() {
                    worker.env.world.store(rig.world, Ordering::SeqCst);
                    let code = if action == "ChildUnchanged" { 1 } else { 0 };
                    worker
                        .exit
                        .send(DevtoolsChangeRun::Exited {
                            code: Some(code),
                            stdout_head: String::new(),
                            stderr_head: String::new(),
                        })
                        .expect("the stand-in waits for its exit");
                    let env = Arc::clone(&worker.env);
                    hang_detector("the worker ending", || {
                        env.stage.load(Ordering::SeqCst) == 2
                    });
                    rig.account();
                } else if let Some(child) = rig.inherited {
                    set_alive(child.pid, false);
                } else {
                    return false;
                }
                true
            });
        }

        /// The park's re-read: only while the lane asks for it.
        fn notice(&mut self) {
            self.step("Notice", |rig| {
                if !rig.needs_poll() {
                    return false;
                }
                let before = rig.lane.flow.phase();
                let step = rig.lane.poll(now_secs());
                rig.apply(step);
                if rig.fault == Fault::ClaimFromExit
                    && let Phase::Exited(change) = rig.lane.flow.phase
                {
                    // THE FAULT: the exit read as the target.
                    rig.lane.flow.observed = change.target();
                }
                rig.lane.flow.phase() != before
            });
        }

        /// The environment moves the rule.
        fn environment(&mut self, action: &str, world: i64) {
            self.step(action, |rig| {
                rig.world = world;
                if let Some(worker) = rig.worker.as_ref() {
                    worker.env.world.store(world, Ordering::SeqCst);
                }
                true
            });
        }

        /// A peer aterm's worker takes the lock, finds no live change named in
        /// the marker, and starts its child, naming it there — as
        /// `run_devtools_job` does. One try: the lock is free at once unless
        /// a holder the rig knows has it.
        fn peer_launch(&mut self) {
            self.step("PeerLaunch", |rig| {
                let LockTry::Held(lock) = try_change_lock(&rig.config) else {
                    return false;
                };
                if read_debugger_marker(&rig.config, CHANGE_MARKER)
                    .and_then(|text| ChangeMarker::parse(&text))
                    .is_some_and(|marker| alive(&marker.child))
                {
                    return false;
                }
                let child = stand_in();
                set_alive(child.pid, true);
                let marker = ChangeMarker::new(rig.change, child, 1).unwrap();
                write_debugger_marker(&rig.config, CHANGE_MARKER, &marker.encode()).unwrap();
                rig.peer = Some((lock, child));
                true
            });
        }

        fn peer_exit(&mut self) {
            self.step("PeerExit", |rig| {
                let Some((lock, child)) = rig.peer.take() else {
                    return false;
                };
                set_alive(child.pid, false);
                drop(lock);
                rig.peer_gone = Some(child);
                true
            });
        }

        /// The peer records its change's re-probe and lets its marker go, as
        /// its own lane does: through `settle_markers`, which removes the
        /// marker only if it still names that peer's child.
        fn peer_settle(&mut self) {
            self.step("PeerSettle", |rig| {
                let Some(child) = rig.peer_gone else {
                    return false;
                };
                let before = rig.now();
                settle_markers(&rig.config, None, Some(&child));
                rig.now() != before
            });
        }

        /// THE RECORDS RE-READ MID-SESSION, as the park runs it when a read
        /// is due: `ask_records`, the reader body over the marker files, and
        /// the next `poll`. `action` names the model step the scenario expects
        /// (`Adopt`, `Follow`, `Discard`); a read that moves nothing is that
        /// step refused.
        fn reread(&mut self, action: &str) {
            self.step(action, |rig| {
                let before = rig.now();
                let liveness: fn(&DevtoolsChild) -> bool = if rig.fault == Fault::AdoptLive {
                    // THE FAULT: no pid-and-birth check — every child is gone.
                    |_| false
                } else {
                    alive
                };
                let into = rig.lane.ask_records();
                *into.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(read_records(&rig.config, liveness));
                let step = rig.lane.poll(now_secs());
                rig.apply(step);
                if rig.lane.settling.is_some_and(|child| alive(&child)) {
                    rig.seen.misadopted = 1;
                }
                rig.now() != before
            });
        }
    }

    /// The happy path in both directions, a decline, a cancelled dialog, an
    /// abort over a moved rule, the named race, drift, a failed re-probe, a
    /// peer's change, and a predecessor's.
    fn scenarios(fault: Fault) {
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.press(OwnerAnswer::yes_for_test()); // refused: nothing observed yet
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.press(OwnerAnswer::yes_for_test()); // refused: the child is in flight
            rig.probe(); // refused: the child is in flight
            rig.notice(); // nothing to take yet
            rig.exit("ChildApplied");
            rig.probe(); // refused: its result is unread
            rig.press(OwnerAnswer::yes_for_test()); // refused: the result is not read yet
            rig.notice();
            assert!(
                rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the marker outlives the taken result"
            );
            rig.press(OwnerAnswer::yes_for_test()); // refused: not re-probed yet
            rig.probe();
            assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(change)));
            assert_eq!(rig.recorded(), Some((1, Origin::Aterm)), "{change:?}");
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: and goes once the re-probe is recorded"
            );
            assert_eq!(rig.state["claim"], 1, "{change:?}: the re-probe says it");

            // Cancel in aterm's alert, nobody asked, then Cancel in macOS's
            // dialog: nothing is retried; the next launch takes the next yes.
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.press(OwnerAnswer::No);
            rig.press(OwnerAnswer::NotAsked);
            rig.press(OwnerAnswer::Quiet);
            rig.press(OwnerAnswer::yes_for_test());
            rig.exit("ChildUnchanged");
            rig.notice();
            rig.probe();
            assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
            assert_eq!(rig.seen.launches, 1);

            // Moved after the render, before the worker's re-read — to a shape
            // Apple's tool does not recognise, or already to the target: the
            // run launches nothing.
            for (moved, world) in [("Customize", 2), ("SetOutside", 1)] {
                let mut rig = Rig::new(change, change.precondition(), fault);
                rig.probe();
                // The owner's press and yes; the rule moves while aterm's
                // alert is up, so the worker's fresh read shows it.
                rig.environment(moved, world);
                rig.press(OwnerAnswer::yes_for_test());
                assert_eq!(rig.seen.launches, 0, "{change:?} {moved}: nothing launched");
                rig.probe();
                rig.press(OwnerAnswer::yes_for_test()); // refused: not the precondition
            }

            // THE NAMED RESIDUAL RACE: customised while the child waits, and
            // its write lands over it. Honest, and still conformant.
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.environment("Customize", 2);
            rig.exit("ChildClobbers");
            rig.notice();
            rig.probe();
            assert_eq!(rig.state["clobbered"], 1);

            // Drift after a change, a failed probe, and the button back.
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.exit("ChildApplied");
            rig.notice();
            rig.probe_fails();
            assert_eq!(rig.state["claim"], 0, "a failed probe claims nothing");
            rig.probe();
            rig.environment("Drift", 0);
            rig.probe();
            rig.environment("Customize", 2);
            rig.environment("Normalize", 0);
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.exit("ChildUnchanged");
            rig.notice();
            rig.probe();

            // A peer aterm's change holds the lock: our worker reads nothing
            // and spawns nothing; its exit frees the next change.
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.peer_launch();
            rig.press(OwnerAnswer::yes_for_test());
            assert_eq!(
                rig.lane.flow.last(),
                Some(Outcome::PeerInFlight(change)),
                "{change:?}"
            );
            rig.peer_exit();
            rig.press(OwnerAnswer::yes_for_test());
            rig.peer_launch(); // refused: our child holds the lock
            rig.exit("ChildApplied");
            rig.notice();
            rig.probe();

            // A predecessor's child still running at start: presses are
            // refused, and its exit is taken and re-probed like our own.
            let mut rig = Rig::inherited(change, change.precondition());
            rig.press(OwnerAnswer::yes_for_test()); // refused: in flight
            rig.peer_launch(); // refused: its marker names a live child
            rig.notice(); // nothing to take yet
            rig.exit("ChildUnchanged");
            rig.notice();
            assert!(
                rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the dead predecessor's marker stays until its change is recorded"
            );
            rig.probe();
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: and goes once it is"
            );
            assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
            assert_eq!(
                rig.state["launches"], 1,
                "the predecessor's launch stays counted"
            );

            // A RECORDS RE-READ MID-SESSION (an unsettled read again, or the
            // drift watch's): a peer's running child is followed as in
            // flight; a peer's change whose child is gone, its marker still
            // on disk, is adopted as aterm's and re-probed; one the peer has
            // recorded leaves nothing; and a dead marker found while a change
            // of this lane's own is under way is removed.
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.reread("Adopt"); // refused: no marker
            rig.peer_launch();
            rig.reread("Follow");
            rig.press(OwnerAnswer::yes_for_test()); // refused: in flight
            rig.peer_exit();
            rig.notice();
            rig.probe();
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the followed change's marker goes with its re-probe"
            );
            rig.peer_launch();
            rig.peer_exit();
            rig.reread("Adopt");
            assert!(
                rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the adopted change's marker stays until its re-probe is recorded"
            );
            rig.press(OwnerAnswer::yes_for_test()); // refused: its re-probe is owed
            rig.reread("Adopt"); // refused: the lane follows it already
            assert!(
                rig.dir.join(CHANGE_MARKER).exists(),
                "a re-read of a change this lane follows leaves its marker for the re-probe"
            );
            rig.probe();
            assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(change)));
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: and goes once it is"
            );
            rig.peer_launch();
            rig.peer_exit();
            rig.peer_settle();
            rig.reread("Adopt"); // refused: the peer recorded it
            rig.press(OwnerAnswer::yes_for_test());
            rig.exit("ChildApplied");
            rig.notice();
            rig.peer_launch();
            rig.peer_exit();
            rig.reread("Discard");
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the stranger's dead marker went"
            );
            rig.probe();
            assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(change)));
        }
    }

    #[test]
    fn debugger_access_conforms_to_the_model() {
        scenarios(Fault::None);
    }

    /// NEGATIVE CONTROLS: the same harness over a composition carrying each
    /// defect the model exists to catch MUST fail — a bind that passed them
    /// all would be checking nothing.
    #[test]
    fn the_harness_fails_every_faulty_composition() {
        for fault in [
            Fault::SkipRecheck,
            Fault::IgnorePeerLock,
            Fault::PressWhileInFlight,
            Fault::ClaimFromExit,
            Fault::DropPollInFlight,
            Fault::RetryAfterCancel,
            Fault::AdoptLive,
        ] {
            let caught = std::panic::catch_unwind(|| match fault {
                // Each needs the step that exercises it.
                Fault::PressWhileInFlight => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.press(OwnerAnswer::yes_for_test());
                    rig.press(OwnerAnswer::yes_for_test());
                }
                Fault::RetryAfterCancel => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.press(OwnerAnswer::yes_for_test());
                    rig.exit("ChildUnchanged");
                    rig.notice();
                    rig.probe();
                }
                _ => scenarios(fault),
            });
            assert!(
                caught.is_err(),
                "{fault:?}: the harness passed a faulty composition"
            );
        }
    }
}
