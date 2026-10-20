// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's debugger-access change (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
//! §3.4, §3.5, §5.1).
//!
//! The owner presses *Allow debugger access…* (or *Turn off debugger access…*) on
//! Settings ▸ Security; aterm's `confirm_owner` alert states the whole-Mac scope and
//! defaults to Cancel. On a yes the park drains the change into
//! `App::begin_debugger_access_change`, which re-checks its gates and starts one
//! detached `consent-devtools` worker through `ConsentGestures::run_devtools`. The
//! worker takes the Mac-wide [`CHANGE_LOCK`], re-reads `DevToolsSecurity -status`
//! and runs `-enable` / `-disable` unprivileged only if the read still shows the
//! change's precondition (`aterm_containment::consent::run_devtools_change`). macOS
//! asks for an administrator in its own dialog, which aterm never sees or answers;
//! then aterm re-probes and reports only what it reads.
//!
//! THE LAUNCH ASK (owner ruling O29, 2026-10-19): once per Mac, aterm raises the same
//! alert by itself — after the first-launch Full Disk Access and Developer Tools pages,
//! at a quiet moment, and only to turn debugger access on where it reads off and was
//! never seen on or turned off here ([`admit_launch_ask`]). The Mac is written into
//! [`ASKED_RECORD`] before the alert is raised, and the owner's yes takes the same
//! path as a press ([`DebuggerChangeLane::offer`], then the same worker).
//!
//! The deciding parts live here, testable without a window, process or dialog:
//! the pure reducer [`DebuggerAccessFlow`], the worker's result slot
//! [`ChangeSlot`] (the result never rides the wake, because an uncommitted
//! incoming handoff drops every wake but Commit), the [`DriftWatch`], the
//! `App`-owned [`DebuggerChangeLane`] that bundles them, [`DebuggerFacts`], and the
//! launch ask's admission, record and queue ([`LaunchAskQueue`]).
//!
//! It never answers SecurityAgent, retries a change the re-probe reads unchanged,
//! re-enables after drift, trusts an exit code, or starts a change without the
//! owner's yes in aterm's alert — the one their press raises, or the launch ask's.
//! No control verb, config key, environment variable or band capsule reaches it (a
//! key can only silence the launch ask: `[privacy] notice = false`), and the two
//! action ids sit under `MACOS_ACCESS_GESTURE_PREFIX`, which `app act` refuses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DevtoolsChangeRun, DevtoolsChild, ProbeGate, ProcessBirth, SelfHardening,
};

/// How often the park re-reads the shared result slot while a change is under way.
/// The worker's wake is only a nudge; this re-read is what guarantees an exited
/// change is read (the model's `WF(Notice)`).
pub(crate) const RESULT_RECHECK: Duration = Duration::from_secs(5);

/// The marker beside `aterm.toml` naming a change child in flight: its
/// direction, pid, birth and when it started (design §3.4 step 3.3). A
/// successor that finds its child still running keeps the button disabled.
pub(crate) const CHANGE_MARKER: &str = "privacy-debugger-change";

/// The marker beside `aterm.toml` holding the last observed mode, when aterm
/// first saw it, and who set it (design §3.5).
pub(crate) const OBSERVED_MARKER: &str = "privacy-debugger-observed";

/// The Mac-wide change lock beside `aterm.toml`. Every aterm sharing the config (a
/// successor mid-handoff, or a development build beside the installed app) takes
/// it before its worker re-reads `-status`, holds it until the child exits, and
/// writes or removes [`CHANGE_MARKER`] only while holding it. `flock` binds to the
/// open file description, so a second worker in this process is refused too, and
/// the kernel drops the lock when its holder dies.
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

fn write_debugger_marker(
    config_path: &Path,
    name: &str,
    text: &str,
    flush: crate::durable::Flush,
) -> std::io::Result<()> {
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path, name) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record in",
        ));
    };
    crate::config_marker::write_marker(&path, text, MAX_MARKER_BYTES, flush)
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
/// ([`HeldAdvisoryLock`]), or by the kernel when its process dies. Not by the
/// close alone: a child this process forks while the lock is held keeps a copy of
/// the description until it execs, and the lock would stay taken in it. `LOCK_UN`
/// releases the description, the child's copy included.
///
/// [`HeldAdvisoryLock`]: crate::native_document_host::HeldAdvisoryLock
#[derive(Debug)]
pub(crate) struct ChangeLock {
    _held: crate::native_document_host::HeldAdvisoryLock,
}

/// What [`take_change_lock`] found.
#[derive(Debug)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only unix takes the change lock")
)]
pub(crate) enum LockTry {
    /// This caller holds it now.
    Held(ChangeLock),
    /// Another worker — in this process or another aterm — holds it.
    Busy,
    /// The lock file could not be opened (no config directory, a planted link,
    /// an unwritable folder): fail closed.
    Unavailable(std::io::Error),
}

/// [`take_change_lock`] in one try, never waiting.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn try_change_lock(config_path: &Path) -> LockTry {
    take_change_lock(config_path, Duration::ZERO)
}

/// How long [`take_change_lock`] waits out a busy lock: long enough for a short
/// hold ([`settle_markers`], the records reader) here or in a peer, or a peer's
/// forked child that still holds a copy until it execs. A real change holder,
/// whose child waits on macOS's administrator dialog, keeps it for seconds to
/// hours and reads `Busy`.
pub(crate) const CHANGE_LOCK_PATIENCE: Duration = Duration::from_secs(1);

/// Take the change lock beside `config_path`, waiting out a busy lock for up to
/// `patience` ([`acquire_advisory_lock_within`]). Worker threads only: it sleeps.
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

/// Write the observed record, then, under the change lock, remove an ended
/// change's marker if it still names `release`'s child. In that order, a peer
/// reading in between finds either the marker (its child dead, so the change is
/// re-read as aterm's) or the record. A lock held past its patience may be a newer
/// change writing its own marker: that one is left alone; the record is written
/// regardless.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    // Ordered: the next probe observes the mode again and the drift watch tells the
    // person of any change; the marker only has to be whole or absent.
    if let Some(marker) = observed
        && let Err(error) = write_debugger_marker(
            config_path,
            OBSERVED_MARKER,
            &marker.encode(),
            crate::durable::Flush::Ordered,
        )
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
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-debugger-marker",
        crate::qos::Role::Background,
        move || {
            // Background: it takes no lock the UI thread contends (only this
            // module's own marker mutex and the change lock).
            settle_markers(&config_path, observed, release.as_ref());
        },
    );
    if let Err(error) = spawned {
        aterm_log::warn!("debugger access: markers not settled: {error}");
    }
}

/// What the records reader found beside `aterm.toml`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecordsRead {
    /// A predecessor's change child, still running.
    pub(crate) inherited: Option<ChangeMarker>,
    /// A change marker naming a child that is gone, whose change was never judged
    /// (aterm quit before the re-probe, or a peer sits between taking its result
    /// and recording its re-probe). Left on disk: the lane re-reads the change as
    /// aterm's own, so the re-probe is attributed to aterm, and removes the marker
    /// once that re-probe is recorded ([`DebuggerChangeLane::poll`]).
    pub(crate) stale: Option<ChangeMarker>,
    /// The last observed mode.
    pub(crate) observed: Option<ObservedMarker>,
    /// No change marker, but the change lock was held past its patience: a worker
    /// sits between its `-status` re-read and its spawn (the marker is written once
    /// the child starts), or holds a child whose birth it could not record. This
    /// read settles nothing; the lane reads again after [`RESULT_RECHECK`].
    pub(crate) unsettled: bool,
}

/// Read both markers, off the UI thread, judging a change marker's child by pid
/// and birth (`alive`), since a pid may be reused. A stale marker is reported and
/// left for the lane ([`RecordsRead::stale`]); only an unparsable one is removed
/// here, under the change lock and only if it still reads the same. A missing
/// marker means nothing is in flight only under the lock (workers write it while
/// holding the lock), so a lock held with no marker is [`RecordsRead::unsettled`].
///
/// The change marker is read before the observed record: [`settle_markers`]
/// writes the record before it removes the marker, so a read that finds no marker
/// finds the record that change left.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "Adopt",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "Follow",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
pub(crate) fn read_records(config_path: &Path, alive: fn(&DevtoolsChild) -> bool) -> RecordsRead {
    let mut read = RecordsRead::default();
    let change = match read_debugger_marker(config_path, CHANGE_MARKER) {
        Some(text) => Some(text),
        // No lock file: no worker ever took it here (a launch does not create
        // one), so none holds it now.
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
    crate::qos::spawn_lane_thread(
        "aterm-debugger-records",
        crate::qos::Role::Responsive,
        move || {
            // Responsive, not Background: it publishes under a mutex the UI
            // thread's park takes, the documented floor for such a holder. It
            // reads two tiny files and is gone.
            let read = read_records(&config_path, alive);
            *into.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
            wake();
        },
    )
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

/// The reducer. Pure: it performs nothing, reads no clock and asks no OS; the
/// host feeds it what happened and does what it allows. Its refusals are the
/// model's guards. Nothing here reads aterm's own hardening: an unhardened aterm
/// offers the gesture too, and the alert says what that exposes.
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
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) const fn direction(&self) -> DebuggerChange {
        self.direction
    }

    pub(crate) const fn last(&self) -> Option<Outcome> {
        self.last
    }

    pub(crate) const fn records_read(&self) -> bool {
        self.records_read
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

    /// The shared records were read, at launch or again mid-session. `inherited`
    /// is a change child proven running by pid and birth (a predecessor's or a
    /// peer's): the change is in flight here too. `stale` is a change whose child
    /// is gone and whose re-probe nobody recorded: a fresh probe ends it as this
    /// aterm's change. `true` when `stale` was taken; a flow with something under
    /// way takes neither.
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

    /// Why a press of `change` would be refused now, taking nothing: what
    /// [`Self::on_press`] checks, for the launch ask, which writes its record
    /// only where the press it stands for would be taken.
    pub(crate) fn press_refusal(&self, change: DebuggerChange) -> Option<Refusal> {
        if !self.records_read {
            return Some(Refusal::RecordsUnread);
        }
        match self.phase {
            Phase::Idle => {}
            Phase::Rechecking(_) | Phase::InFlight { .. } => return Some(Refusal::InFlight),
            Phase::Confirming(_) | Phase::Exited(_) => return Some(Refusal::Busy),
        }
        (self.observed != change.precondition()).then_some(Refusal::State(change, self.observed))
    }

    /// The owner pressed the button for `change`. Refused unless the
    /// predecessor's records are read, nothing is under way, and the last probe
    /// shows the change's precondition. Never refused for aterm's own hardening.
    pub(crate) fn on_press(&mut self, change: DebuggerChange) -> Result<(), Refusal> {
        if let Some(refusal) = self.press_refusal(change) {
            return Err(refusal);
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
        self.phase = Phase::InFlight {
            change,
            child,
            since,
            inherited: false,
        };
        true
    }

    /// The worker's re-read did not show the precondition: nothing was spawned,
    /// and what it read is the flow's observation now.
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

    /// The park took the exited child's result, or the child is gone with none
    /// (lost). Either way only a fresh probe says what happened, so the model's
    /// three exits are all taken the same way.
    pub(crate) fn on_child_exit(&mut self) -> bool {
        let Phase::InFlight { change, .. } = self.phase else {
            return false;
        };
        self.phase = Phase::Exited(change);
        true
    }
}

/// What the host's start re-checks besides the reducer (design §3.4 step 2).
/// `self_hardened` is carried so the refusal table can be pinned; it is not a gate.
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

/// One confirmed change: the direction, its job number and the owner's yes. Only
/// [`DebuggerChangeLane::begin`] mints one (for a press, or inside
/// [`DebuggerChangeLane::offer`] for the launch ask), on an [`OwnerAnswer::Yes`]
/// whose [`OwnerYes`] only `native_settings::owner_confirms_with` mints, after
/// aterm's alert, so a [`DevtoolsJob`] and its `DevToolsSecurity` worker exist only
/// after the owner's yes in that alert. Not `Clone`: one yes, one job.
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
    /// The lane's liveness check (pid and birth): a change marker naming a child
    /// it proves running is a change in flight on this Mac, and the worker runs
    /// nothing over it.
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

/// The live arm: one detached `consent-devtools` worker whose handle is dropped;
/// it may wait hours on macOS's dialog, so nothing may join it. Never called
/// from a test.
pub(crate) fn spawn_devtools_worker(job: DevtoolsJob) -> DevtoolsSpawn {
    let spawned = crate::qos::spawn_lane_thread(
        "consent-devtools",
        crate::qos::Role::Responsive,
        move || {
            // Responsive: it publishes into the result slot the UI thread's
            // park reads under the same mutex, the documented floor for such a
            // holder. Nearly all of its life is a wait on one child, so the
            // rank costs nothing.
            run_devtools_job(job, |change, gate, on_spawn| {
                aterm_containment::consent::run_devtools_change(change, gate, on_spawn)
            });
        },
    );
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

/// The worker's body, with the change runner injected (live:
/// `aterm_containment::consent::run_devtools_change`; the Tier-1 bind uses
/// `devtools_change_with_for_test` with a scripted read and a stand-in child).
/// It takes the change lock first (a lock held elsewhere runs nothing), publishes
/// the child the moment it starts and writes the change marker, publishes the
/// result when the child exits, releases the lock, and through [`WorkerGone`] sets
/// `gone` and posts the nudge. The marker stays until the park has re-probed and
/// recorded the result ([`LaneStep::remove_change_marker`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "Recheck",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "Launch",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "ChildApplied",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DebuggerAccessChange",
        action = "ChildUnchanged",
        project = "aterm_gui::debugger_access::conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    // Held through the re-read, the spawn and the wait, and released before the
    // result is published: the park that takes the result removes the change
    // marker after its re-probe and must find the lock free, or the stale marker
    // outlives the change.
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
    // A holder that died while its child waited left the lock free but the child
    // running; its marker still names that child, so a second child is refused.
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
                    if let Err(error) = write_debugger_marker(
                        path,
                        CHANGE_MARKER,
                        &marker.encode(),
                        crate::durable::Flush::Durable,
                    ) {
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
/// modes a change moves between; `unrecognized` and `unknown` are not drift. It
/// never re-enables anything.
///
/// The record is shared by every aterm on this config, and a peer records its own
/// changes. So a mode this process did not change is judged against the marker as
/// it is on disk now, never this process's copy: the watch un-seeds, keeps the
/// mode, and the lane reads the records again ([`DebuggerChangeLane::poll`]), so a
/// peer's change is neither re-filed as outside or drift nor overwritten.
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

    /// The marker was read. A mode seen before then is judged against it now,
    /// unless the same read put a change under way and the mode carries no
    /// attribution: that early probe may predate the change's end, so it is
    /// dropped and the re-probe that ends the change is judged, as aterm's.
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
    /// The child of the change that exited and waits to be judged: this
    /// process's own, an inherited one, or one a stale marker named. Its marker
    /// stays on disk until the re-probe is judged and the observed record written,
    /// so a peer re-reading in between re-reads the change as aterm's rather than
    /// filing it as set outside aterm.
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

    /// Record why the last confirmed change — a press, or the launch ask's yes —
    /// started nothing (the page shows it until the next one starts).
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

    /// Whether the shared records have been read (a press's first gate).
    pub(crate) const fn records_read(&self) -> bool {
        self.flow.records_read()
    }

    /// Whether a change is under way: aterm's alert, the worker's re-read, a
    /// child in flight (this process's or an inherited one, whose macOS dialog
    /// may be up), or the re-probe that ends it.
    pub(crate) fn under_way(&self) -> bool {
        self.flow.phase() != Phase::Idle
    }

    /// The owner's press and their answer in aterm's alert (`menu::confirm_owner`,
    /// which only the owner's press and the once-per-Mac launch ask reach, the
    /// latter through [`Self::offer`]), and on a yes only, a new job armed in the
    /// slot. The press is refused unless the records are read, nothing is under
    /// way and the last probe shows the precondition. `Err` names why nothing may
    /// start.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Press",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Consent",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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

    /// One park re-read (design §3.4 steps 4 and 6): the worker's slot, then the
    /// records, then a predecessor's child's liveness. It takes an exited child's
    /// result whether or not its wake arrived (the model's weakly fair `Notice`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Notice",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Adopt",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Follow",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Discard",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn poll(&mut self, now_wall: u64) -> LaneStep {
        let mut step = LaneStep::default();
        // The records read is taken before the worker's slot and handled after it.
        // A read names a marker its worker wrote after `publish_spawned`, so the
        // slot take then holds that child, and the job's own child is followed by
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

    /// A completed probe (the cached `ConsentState::debugger` answer). Ends an exited
    /// change and feeds the drift watch; while a change is under way the drift
    /// watch waits for the re-probe that can attribute it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Probe",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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

    /// Whether `child` is one this lane already follows: the current job's, the
    /// flow's in flight (a predecessor's included), the settling one, or the one
    /// last released.
    fn follows(&self, child: &DevtoolsChild) -> bool {
        [
            self.child,
            self.settling,
            self.released,
            self.flow.in_flight().map(|(_, child, _, _)| child),
        ]
        .contains(&Some(*child))
    }

    /// Let the settling child's marker go once its change is judged: the re-probe
    /// ended it and the drift watch judged that probe, its observed record in this
    /// step or an earlier one. A watch waiting for a fresh records read judges the
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
// The launch ask (owner ruling O29, 2026-10-19)
// ---------------------------------------------------------------------------

/// The record beside `aterm.toml` of the Macs on which aterm has asked, by itself,
/// whether to allow debugger access: one `<host-uuid> <secs>` line per Mac
/// (`aterm_containment::consent::host_uuid`), oldest first. The config folder may
/// sync between Macs, hence the host key: a Mac whose synced record names only
/// others is asked once too.
pub(crate) const ASKED_RECORD: &str = "privacy-debugger-asked";

/// The Macs the asked record keeps. Past that the oldest line goes, and that Mac
/// is asked again, once, the next time aterm runs there.
pub(crate) const MAX_ASKED_LINES: usize = 8;

/// A host key's longest spelling (the kernel's UUID is 36 bytes).
const MAX_HOST_BYTES: usize = 64;

/// The asked record's bound: [`MAX_ASKED_LINES`] lines of the longest host key
/// and a `u64`. Anything bigger is not a record this build wrote.
const MAX_ASKED_BYTES: usize = MAX_ASKED_LINES * (MAX_HOST_BYTES + 1 + 20 + 1);

/// A host key the record can hold: one plain word (ASCII letters, digits and `-`,
/// as the kernel's UUID is spelled), so every line splits back exactly.
fn host_key_ok(host: &str) -> bool {
    (1..=MAX_HOST_BYTES).contains(&host.len())
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The asked record's lines, strictly as [`note_asked`] writes them, or `None`:
/// newline-terminated, each line a host key, one space and its seconds, and at
/// most [`MAX_ASKED_LINES`] of them.
fn parse_asked(text: &str) -> Option<Vec<(&str, u64)>> {
    let body = text.strip_suffix('\n')?;
    let mut lines = Vec::new();
    for line in body.split('\n') {
        let (host, secs) = line.split_once(' ')?;
        if !host_key_ok(host) || secs.is_empty() || !secs.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        lines.push((host, secs.parse().ok()?));
    }
    (lines.len() <= MAX_ASKED_LINES).then_some(lines)
}

/// Where the asked record lives beside `config_path`.
fn asked_path(config_path: &Path) -> std::io::Result<PathBuf> {
    marker_path(config_path, ASKED_RECORD).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record in",
        )
    })
}

/// The asked record's text: `Ok(None)` for a genuine absence. Anything else at
/// the path goes through the bounded, regular, no-final-link marker reader, and
/// what it refuses — a link, a folder, an oversized or non-UTF-8 file — is an
/// error, as is a folder that cannot be read.
fn read_asked_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => crate::config_marker::read_marker(path, MAX_ASKED_BYTES)
            .map(Some)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the asked record is not a bounded regular UTF-8 record",
                )
            }),
    }
}

/// Whether the asked record names `host`. It fails toward NOT asking: a record
/// that is there but cannot be read whole — a link, a folder, too big, not UTF-8,
/// malformed — a folder that cannot be read, and a host key no record could hold
/// all read as asked. Only an absent record, or a whole one naming other Macs
/// only, is not asked.
pub(crate) fn read_asked(config_path: &Path, host: &str) -> bool {
    if !host_key_ok(host) {
        return true;
    }
    match asked_path(config_path).and_then(|path| read_asked_text(&path)) {
        Ok(None) => false,
        Ok(Some(text)) => {
            parse_asked(&text).is_none_or(|lines| lines.iter().any(|&(named, _)| named == host))
        }
        Err(_) => true,
    }
}

/// Write `host` into the asked record, AHEAD of the alert it stands for
/// ([`Flush::Durable`]: once the alert is up, a crash, quit or update must never
/// ask again, and this record is all that remembers it). A record that names
/// `host` already is left as it is; past [`MAX_ASKED_LINES`] the oldest lines go.
/// A record that cannot be read whole is never overwritten (`InvalidData`; it
/// reads as asked anyway), and nothing is written through a link
/// (`config_marker::write_marker`).
///
/// [`Flush::Durable`]: crate::durable::Flush::Durable
pub(crate) fn note_asked(config_path: &Path, host: &str, now: u64) -> std::io::Result<()> {
    use std::fmt::Write as _;
    if !host_key_ok(host) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a host key the asked record can hold",
        ));
    }
    let path = asked_path(config_path)?;
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    let text = read_asked_text(&path)?;
    let mut lines = match text.as_deref() {
        None => Vec::new(),
        Some(text) => parse_asked(text).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the asked record is malformed and is left as found",
            )
        })?,
    };
    if lines.iter().any(|&(named, _)| named == host) {
        return Ok(());
    }
    lines.push((host, now));
    let keep = lines.len().saturating_sub(MAX_ASKED_LINES);
    let mut record = String::new();
    for (named, secs) in &lines[keep..] {
        let _ = writeln!(record, "{named} {secs}");
    }
    crate::config_marker::write_marker(
        &path,
        &record,
        MAX_ASKED_BYTES,
        crate::durable::Flush::Durable,
    )
}

/// What the change marker and lock beside `aterm.toml` say to the launch ask,
/// judged as the worker ([`run_devtools_job`]) and the records reader
/// ([`read_records`]) judge them, so the three never disagree about one disk
/// ([`DebuggerChangeLane::change_on_disk`]).
#[derive(Debug)]
enum OnDisk {
    /// Nothing a press's worker would be refused over: no change marker, or one
    /// that names no change (a link, a folder, a file too big, not UTF-8 or
    /// malformed — the records reader ignores or removes it and the worker
    /// ignores it) or a gone child this lane follows already; and no lock file,
    /// or a free one.
    Clear(ChangeLock),
    /// A change this lane does not follow, or one still running: a marker
    /// naming a running child (a peer's, or a predecessor's whose worker died
    /// with it), a marker naming a gone child nobody has recorded (the model's
    /// `stale`), or the lock held (a peer's worker between its re-read and its
    /// spawn).
    Change,
    /// The lock file is there but cannot be opened or locked (a link, a folder,
    /// another owner's file, a filesystem that refuses `flock`): the worker
    /// would refuse a yes (`WorkerRefusal::NoLock`), so nothing can start.
    NoLock,
}

/// This Mac in the asked record, as the launch ask reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostAsked {
    /// The host key is not read yet: it is read once, off the event loop.
    Cold,
    /// The kernel names no host, so no record could be keyed to this Mac.
    Unnamed,
    /// The record names this Mac, or cannot be read whole.
    Asked,
    /// The record is absent, or names other Macs only.
    NotAsked,
}

/// What the launch ask is judged on (design §3.4 step 0, owner ruling O29). Pure
/// data, gathered by the App at its look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaunchAskFacts {
    /// A windowed instance.
    pub(crate) windowed: bool,
    pub(crate) macos: bool,
    /// `[privacy] enabled && check`.
    pub(crate) privacy: bool,
    /// `[privacy] notice`, the switch that silences aterm's launch questions. A
    /// key may only silence the ask, never start it (design §3.9).
    pub(crate) notice: bool,
    /// A config folder to keep the record in: without one nothing would remember
    /// the answer, and every launch would ask again.
    pub(crate) config: bool,
    pub(crate) host: HostAsked,
    /// The shared debugger records (a predecessor's change, the observed mode)
    /// are read.
    pub(crate) records_read: bool,
    /// A change under way ([`DebuggerChangeLane::under_way`]), this process's or
    /// an inherited one.
    pub(crate) under_way: bool,
    /// The owner answered a debugger-access alert in this process, yes or no
    /// (`OwnerQuiet::debugger_answered`).
    pub(crate) answered: bool,
    /// The cached probe, `None` until the look reads it: a look dispatches the
    /// probe worker only while no answer is cached, so an ask a gate above
    /// already decides reads nothing, and one waiting for its moment re-reads
    /// nothing.
    pub(crate) probe: Option<DebuggerProbe>,
    /// The probe was read within [`ASK_PROBE_FRESH`] (or by an instance's
    /// syscall-free arm, which answers at every read). The alert is raised only
    /// on such a reading, so a rule switched on meanwhile — by the owner's own
    /// `sudo DevToolsSecurity -enable` in an aterm tab, which retires no cached
    /// answer — settles [`AskSettle::AlreadyOn`] instead of asking to allow
    /// what is allowed.
    pub(crate) probe_fresh: bool,
    /// The observed record, as the lane holds it.
    pub(crate) recorded: Option<ObservedMarker>,
    /// This process saw debugger access go from allowed to off.
    pub(crate) drift: bool,
    /// aterm's own hardening, read with the probe.
    pub(crate) self_hardened: SelfHardening,
    /// This process's Full Disk Access launch decision is made (not
    /// `FdaLaunch::Undecided`). Its hand-over still queued, or its panel up, is
    /// [`Self::page`]; a hand-over gone with nothing to end it holds nothing.
    pub(crate) fda_decided: bool,
    /// This process's Developer Tools launch reading has completed, whatever it
    /// read (a refusal included).
    pub(crate) devtools_decided: bool,
    /// A Full Disk Access or Developer Tools hand-over waits, or aterm's helper
    /// panel is up.
    pub(crate) page: bool,
    /// The folder warm-up is walking (a macOS folder dialog may be up).
    pub(crate) warmup: bool,
    /// The quiet moment (`App::devtools_moment_is_quiet`): an aterm window
    /// focused, the owner present but hands off, this process settled.
    pub(crate) quiet: bool,
    /// The owner declined a Security gesture recently (`OwnerQuiet`).
    pub(crate) owner_quiet: bool,
}

/// What the launch ask does now ([`admit_launch_ask`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchAsk {
    /// Write the record, then raise the alert ([`DebuggerChangeLane::offer`]).
    Ask,
    /// Look again later ([`AskWait::look_again`]).
    Wait(AskWait),
    /// Write the record and raise nothing: the question is answered on this Mac
    /// ([`DebuggerChangeLane::forgo`]).
    Settle(AskSettle),
    /// Nothing more this process, and nothing written: a later launch judges
    /// again.
    Drop(AskDrop),
}

/// Why the launch ask asks nothing this process and writes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AskDrop {
    /// A headless instance: nobody to ask.
    Headless,
    /// Debugger access is a macOS setting.
    NotMacos,
    /// `[privacy] enabled` or `check` is off.
    PrivacyOff,
    /// `[privacy] notice` is off.
    NoticeOff,
    /// No config folder to keep the record in.
    NoConfig,
    /// The kernel names no host.
    NoHost,
    /// The record names this Mac, or cannot be read whole.
    Asked,
    /// The probe reads a rule Apple's tool does not recognise, which `-enable`
    /// would overwrite.
    Unrecognized,
    /// The probe gave no answer (refused, failed, timed out).
    Unread,
    /// The account is in neither admin nor _developer, or that is unknown: the
    /// switch might not help it.
    NotMember,
    /// This aterm is not hardened, or that is unknown: the ask is aterm's own
    /// initiative, which design §3.6 keeps to a hardened aterm. Its button still
    /// offers the change (O8).
    NotHardened,
}

impl AskDrop {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::NotMacos => "not_macos",
            Self::PrivacyOff => "privacy_off",
            Self::NoticeOff => "notice_off",
            Self::NoConfig => "no_config",
            Self::NoHost => "no_host",
            Self::Asked => "asked",
            Self::Unrecognized => "unrecognized",
            Self::Unread => "unread",
            Self::NotMember => "not_member",
            Self::NotHardened => "not_hardened",
        }
    }
}

/// Why the launch ask looks again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AskWait {
    /// The host key is not read yet.
    HostCold,
    /// The shared debugger records are not read yet.
    RecordsUnread,
    /// The probe has not answered yet.
    ProbePending,
    /// The Full Disk Access launch decision is not made yet.
    BehindFullDiskAccess,
    /// The Developer Tools launch reading has not completed.
    BehindDeveloperTools,
    /// A hand-over waits, or aterm's helper panel is up.
    BehindPage,
    /// The folder warm-up is walking.
    BehindWarmup,
    /// Not the quiet moment.
    NotQuiet,
    /// The owner declined a Security gesture recently.
    OwnerQuiet,
    /// The moment has come, but the probe is older than [`ASK_PROBE_FRESH`]:
    /// it is read again before the alert.
    ProbeStale,
}

/// How old a probe the launch ask raises its alert on may be. Older, it is
/// retired and read again first ([`AskWait::ProbeStale`]). Longer than
/// [`crate::devtools_helper::PANEL_TICK`], the look's cadence while that read
/// is out, so the reading it waited for is fresh at the next look.
pub(crate) const ASK_PROBE_FRESH: Duration = Duration::from_secs(5);

impl AskWait {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::HostCold => "host_cold",
            Self::RecordsUnread => "records_unread",
            Self::ProbePending => "probe_pending",
            Self::BehindFullDiskAccess => "behind_full_disk_access",
            Self::BehindDeveloperTools => "behind_developer_tools",
            Self::BehindPage => "behind_page",
            Self::BehindWarmup => "behind_warmup",
            Self::NotQuiet => "not_quiet",
            Self::OwnerQuiet => "owner_quiet",
            Self::ProbeStale => "probe_stale",
        }
    }

    /// How long until the next look: [`crate::devtools_helper::PANEL_TICK`]
    /// while a read the look started (the host key, the probe) completes — its
    /// completion wakes the window; this is the backstop — and
    /// [`crate::devtools_helper::RECHECK_EVERY`] otherwise, as the hand-overs'
    /// looks for their quiet moment do. Never zero, so a park never spins on it.
    pub(crate) const fn look_again(self) -> Duration {
        match self {
            Self::HostCold | Self::ProbePending | Self::ProbeStale => {
                crate::devtools_helper::PANEL_TICK
            }
            _ => crate::devtools_helper::RECHECK_EVERY,
        }
    }
}

/// Why the launch ask settles: the question is answered on this Mac, and the
/// record says so with no alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AskSettle {
    /// A change is under way: the owner is answering one already.
    Underway,
    /// The owner answered a debugger-access alert in this process.
    Answered,
    /// Debugger access is allowed already.
    AlreadyOn,
    /// It reads off, but this process saw it go off, or the record says it was
    /// allowed or that aterm or something else turned it off: aterm never turns
    /// it back on (design §6).
    TurnedOff,
}

impl AskSettle {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Underway => "under_way",
            Self::Answered => "answered",
            Self::AlreadyOn => "already_on",
            Self::TurnedOff => "turned_off",
        }
    }
}

/// THE LAUNCH ASK'S ADMISSION (owner ruling O29). Pure, and the order is the
/// contract: the instance's gates; this Mac in the record; the reads it waits
/// on; whether the question is answered here already; whether turning debugger
/// access on can help this account from this aterm; then the moment — behind
/// the Full Disk Access and Developer Tools launch pages, with no page, panel
/// or folder dialog up, the owner present and paused, and no recent decline;
/// and last a probe fresh enough to raise the alert on.
pub(crate) fn admit_launch_ask(facts: &LaunchAskFacts) -> LaunchAsk {
    let gates = [
        (facts.windowed, AskDrop::Headless),
        (facts.macos, AskDrop::NotMacos),
        (facts.privacy, AskDrop::PrivacyOff),
        (facts.notice, AskDrop::NoticeOff),
        (facts.config, AskDrop::NoConfig),
    ];
    if let Some(&(_, why)) = gates.iter().find(|(open, _)| !*open) {
        return LaunchAsk::Drop(why);
    }
    match facts.host {
        HostAsked::Cold => return LaunchAsk::Wait(AskWait::HostCold),
        HostAsked::Unnamed => return LaunchAsk::Drop(AskDrop::NoHost),
        HostAsked::Asked => return LaunchAsk::Drop(AskDrop::Asked),
        HostAsked::NotAsked => {}
    }
    if !facts.records_read {
        return LaunchAsk::Wait(AskWait::RecordsUnread);
    }
    let Some(probe) = facts
        .probe
        .filter(|probe| probe.label != DebuggerProbeLabel::Pending)
    else {
        return LaunchAsk::Wait(AskWait::ProbePending);
    };
    if facts.under_way {
        return LaunchAsk::Settle(AskSettle::Underway);
    }
    if facts.answered {
        return LaunchAsk::Settle(AskSettle::Answered);
    }
    match probe.mode {
        DebuggerMode::Enabled => return LaunchAsk::Settle(AskSettle::AlreadyOn),
        DebuggerMode::Disabled
            if facts.drift
                || facts.recorded.is_some_and(|was| {
                    was.mode == DebuggerMode::Enabled || was.origin != Origin::Unknown
                }) =>
        {
            return LaunchAsk::Settle(AskSettle::TurnedOff);
        }
        DebuggerMode::Disabled => {}
        DebuggerMode::Unrecognized => return LaunchAsk::Drop(AskDrop::Unrecognized),
        DebuggerMode::Unknown => return LaunchAsk::Drop(AskDrop::Unread),
    }
    if !matches!(
        probe.member,
        DebuggerMember::Admin | DebuggerMember::Developer
    ) {
        return LaunchAsk::Drop(AskDrop::NotMember);
    }
    if facts.self_hardened != SelfHardening::Hardened {
        return LaunchAsk::Drop(AskDrop::NotHardened);
    }
    let waits = [
        (!facts.fda_decided, AskWait::BehindFullDiskAccess),
        (!facts.devtools_decided, AskWait::BehindDeveloperTools),
        (facts.page, AskWait::BehindPage),
        (facts.warmup, AskWait::BehindWarmup),
        (!facts.quiet, AskWait::NotQuiet),
        (facts.owner_quiet, AskWait::OwnerQuiet),
        (!facts.probe_fresh, AskWait::ProbeStale),
    ];
    waits
        .iter()
        .find(|(holds, _)| *holds)
        .map_or(LaunchAsk::Ask, |&(_, why)| LaunchAsk::Wait(why))
}

/// Why [`DebuggerChangeLane::offer`] gave no ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferEnd {
    /// No config folder to keep the record in: nothing was written or asked.
    NoRecord,
    /// The press the ask stands for would be refused now (here, or a change on
    /// disk): nothing was written or asked.
    Refused(Refusal),
    /// The change lock beside `aterm.toml` is there but cannot be opened or
    /// locked, so the worker would refuse a yes (`WorkerRefusal::NoLock`):
    /// nothing was written or asked, and looking again changes nothing.
    NoLock,
    /// The record names this Mac (or cannot be read whole): nothing was asked.
    Asked,
    /// The record could not be written: nothing was asked, since an alert no
    /// record remembers would come back at every launch.
    NotRecorded,
    /// The owner said no. The record stands: they are not asked again.
    Declined,
    /// No alert could be shown, or a recent decline quieted it: not a yes. The
    /// record stands.
    NotAsked,
}

impl OfferEnd {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NoRecord => "no_record",
            Self::Refused(_) => "refused",
            Self::NoLock => "no_lock",
            Self::Asked => "asked",
            Self::NotRecorded => "not_recorded",
            Self::Declined => "declined",
            Self::NotAsked => "not_asked",
        }
    }
}

impl DebuggerChangeLane {
    /// THE LAUNCH ASK'S ALERT AND ANSWER (owner ruling O29): write this Mac into
    /// [`ASKED_RECORD`], raise aterm's alert through `confirm`, and take the answer
    /// exactly as a press of *Allow debugger access…* ([`Self::begin`]: a yes arms
    /// a job and mints the ticket the worker needs). Nothing is written or asked
    /// unless that press would be taken now — the records read, nothing under way
    /// here, no change on disk the worker would be refused over
    /// ([`Self::change_on_disk`]), the last probe reading debugger access off —
    /// and the record does not name this Mac yet. A change lock that cannot be
    /// taken at all ends it ([`OfferEnd::NoLock`]): no yes could start anything.
    /// The record goes to disk before `confirm` runs, so an alert left up through
    /// a crash, quit or update never comes back; a record that cannot be written
    /// asks nothing. Only On: aterm never asks by itself to turn it off.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Offer",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn offer(
        &mut self,
        config_path: Option<&Path>,
        host: &str,
        confirm: impl FnOnce() -> crate::native_settings::OwnerAnswer,
    ) -> Result<ChangeTicket, OfferEnd> {
        use crate::native_settings::OwnerAnswer;
        let Some(config_path) = config_path else {
            return Err(OfferEnd::NoRecord);
        };
        if let Some(refusal) = self.flow.press_refusal(DebuggerChange::On) {
            return Err(OfferEnd::Refused(refusal));
        }
        let claim = match self.change_on_disk(config_path, now_secs()) {
            OnDisk::Clear(claim) => claim,
            OnDisk::Change => return Err(OfferEnd::Refused(Refusal::InFlight)),
            OnDisk::NoLock => return Err(OfferEnd::NoLock),
        };
        if read_asked(config_path, host) {
            return Err(OfferEnd::Asked);
        }
        if let Err(error) = note_asked(config_path, host, now_secs()) {
            aterm_log::warn!("debugger launch ask: this Mac could not be recorded: {error}");
            return Err(OfferEnd::NotRecorded);
        }
        // The record is now visible to every contender. Never keep the change
        // lock while an alert runs its modal loop or a confirmed worker starts.
        drop(claim);
        let answer = confirm();
        let declined = answer == OwnerAnswer::No;
        self.begin(DebuggerChange::On, answer)
            .map_err(|refusal| match refusal {
                Refusal::NotConfirmed if declined => OfferEnd::Declined,
                Refusal::NotConfirmed => OfferEnd::NotAsked,
                other => OfferEnd::Refused(other),
            })
    }

    /// Whether a change on disk would refuse the press [`Self::offer`] stands
    /// for, judged as the worker judges it: a change marker counts only when it
    /// names a child that is running, or a gone one this lane does not follow
    /// (the model's `stale`), and the lock only when it is held (one try, never
    /// a wait: the UI thread asks). A marker that names no change is no change,
    /// as it is to [`read_records`] and [`run_devtools_job`], so a stray file
    /// at its path cannot hold the ask. A marker this idle lane does not follow
    /// asks for a records read at once (`now_wall`): the park then follows that
    /// change, or adopts a gone one and re-probes it, and the next look finds
    /// it under way and settles — rather than waiting on a marker an idle lane
    /// would never read again.
    fn change_on_disk(&mut self, config_path: &Path, now_wall: u64) -> OnDisk {
        // A first launch may have no config directory yet. The record writer
        // already creates it; the lock must be able to precede that write.
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let claim = match take_change_lock(config_path, Duration::ZERO) {
            LockTry::Held(claim) => claim,
            LockTry::Busy => return OnDisk::Change,
            LockTry::Unavailable(error) => {
                aterm_log::warn!("debugger launch ask: the change lock cannot be taken: {error}");
                return OnDisk::NoLock;
            }
        };
        let marker = read_debugger_marker(config_path, CHANGE_MARKER)
            .and_then(|text| ChangeMarker::parse(&text));
        if let Some(marker) = marker {
            let followed = self.follows(&marker.child);
            if !followed && !self.records_out && self.records_again_at.is_none() {
                self.records_again_at = Some(now_wall);
            }
            if !followed || (self.alive)(&marker.child) {
                return OnDisk::Change;
            }
        }
        // Retain one kernel lock across the record's read and durable write:
        // another process cannot pass the same absent-host reading meanwhile.
        OnDisk::Clear(claim)
    }

    /// THE LAUNCH ASK SETTLED (owner ruling O29): write this Mac into
    /// [`ASKED_RECORD`] and nothing else. The question is answered here — a
    /// change is under way, the owner answered the alert in this process,
    /// debugger access is on, or it was on or turned off before — so no later
    /// launch asks it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "Forgo",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn forgo(config_path: Option<&Path>, host: &str) -> std::io::Result<()> {
        let Some(config_path) = config_path else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config directory to record in",
            ));
        };
        note_asked(config_path, host, now_secs())
    }
}

/// The App's launch ask: queued on a windowed macOS instance until it asks,
/// settles or drops, with the park's next look. It holds the host-key reader
/// (injected; the live one, `host_uuid_if_warm`, never makes a syscall) and the
/// record's answer for this Mac, read once.
#[derive(Debug)]
pub(crate) struct LaunchAskQueue {
    queued: bool,
    /// When the next look is due; `None`: at the next park.
    next_look: Option<Instant>,
    host: fn() -> Option<&'static str>,
    /// The host key's read off the event loop: `None` until it is started, then
    /// whether it has ended.
    host_read: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The record's answer, as read for a host.
    asked: Option<(&'static str, bool)>,
    /// What the last look waited on.
    waiting: Option<AskWait>,
    /// When the first offer this process was refused before anything was
    /// written ([`Self::refused`]).
    refused_since: Option<Instant>,
}

/// How long offers refused before anything was written (a change on disk, the
/// lane busy) may keep the launch ask queued, from the first refusal. A change
/// that is really under way is followed or adopted by the lane well within it
/// and settles the ask; past it the ask ends for this process with nothing
/// written, so a disk state nothing here settles can never hold the folder
/// warm-up, which waits while the ask is queued, for longer.
pub(crate) const REFUSED_FOR: Duration = Duration::from_secs(5 * 60);

impl LaunchAskQueue {
    /// Queued when `queued` (a windowed macOS instance), with the live host key.
    pub(crate) fn new(queued: bool) -> Self {
        Self {
            queued,
            next_look: None,
            host: aterm_containment::consent::host_uuid_if_warm,
            host_read: None,
            asked: None,
            waiting: None,
            refused_since: None,
        }
    }

    /// A queued ask with a stand-in host key whose read has ended.
    #[cfg(test)]
    pub(crate) fn queued_for_test(host: fn() -> Option<&'static str>) -> Self {
        Self {
            host,
            host_read: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
            ..Self::new(true)
        }
    }

    /// A queued ask with a stand-in host key whose read is out, ended once
    /// `done` is set.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn reading_host_for_test(
        host: fn() -> Option<&'static str>,
        done: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            host,
            host_read: Some(done),
            ..Self::new(true)
        }
    }

    /// Whether the ask is still to come this process.
    pub(crate) const fn queued(&self) -> bool {
        self.queued
    }

    /// Whether a look is due at `now`.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.queued && self.next_look.is_none_or(|at| now >= at)
    }

    /// Look again at `at`.
    pub(crate) fn look_later(&mut self, at: Instant) {
        self.next_look = Some(at);
    }

    /// The look waits on `why`: look again at `at`. `true` when that is not
    /// what the last look waited on (worth one line in the log).
    pub(crate) fn wait(&mut self, why: AskWait, at: Instant) -> bool {
        self.look_later(at);
        self.waiting.replace(why) != Some(why)
    }

    /// The offer at `now` was refused before anything was written or asked
    /// ([`OfferEnd::Refused`]): look again after
    /// [`crate::devtools_helper::RECHECK_EVERY`], unless refusals have kept the
    /// ask queued for [`REFUSED_FOR`] since the first one — then it is over for
    /// this process (`false`), with nothing written, and a later launch judges
    /// again. The span is never restarted, so refusals met only at the quiet
    /// moments between the owner's typing still end it.
    pub(crate) fn refused(&mut self, now: Instant) -> bool {
        let since = *self.refused_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= REFUSED_FOR {
            self.end();
            return false;
        }
        self.look_later(now + crate::devtools_helper::RECHECK_EVERY);
        true
    }

    /// The park's deadline: the next look, while the ask is queued.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.next_look.filter(|_| self.queued)
    }

    /// The host key, once warm.
    pub(crate) fn warm_host(&self) -> Option<&'static str> {
        (self.host)()
    }

    /// A cold host key's read: `Some(done)` the first time — the caller reads it
    /// off the event loop and sets `done` when that read ends — `None` after.
    pub(crate) fn start_host_read(&mut self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        if self.host_read.is_some() {
            return None;
        }
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.host_read = Some(Arc::clone(&done));
        Some(done)
    }

    /// Whether the host key's read has ended: a key still cold then is none.
    pub(crate) fn host_read_ended(&self) -> bool {
        self.host_read
            .as_ref()
            .is_some_and(|done| done.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Whether the record names `host` ([`read_asked`]), read once per process.
    pub(crate) fn asked(&mut self, config_path: &Path, host: &'static str) -> bool {
        match self.asked {
            Some((read_for, asked)) if read_for == host => asked,
            _ => {
                let asked = read_asked(config_path, host);
                self.asked = Some((host, asked));
                asked
            }
        }
    }

    /// The ask is over for this process.
    pub(crate) fn end(&mut self) {
        self.queued = false;
        self.next_look = None;
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
    /// The cached probe: the rule's mode, the account's membership, and why. The
    /// only source of what the row says the rule is.
    pub(crate) probe: DebuggerProbe,
    /// aterm's own hardening. Not a gate for the owner's press (O8): when it is
    /// not `Hardened` the row and the alert say what that exposes. aterm's own
    /// launch ask does gate on it ([`admit_launch_ask`]).
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
    /// Why the last confirmed change (a press, or the launch ask's yes)
    /// started nothing.
    pub(crate) refusal: Option<Refusal>,
}

impl Default for DebuggerFacts {
    /// The zero state: aterm did not look, nothing is under way that it knows of,
    /// and nothing is offered.
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
    /// under way, the probe shows the change's precondition, and for *Allow* the
    /// account is one the switch would help. aterm's own hardening is not consulted.
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
    #[cfg(unix)]
    #[test]
    fn a_busy_change_lock_is_waited_out_only_for_its_patience() {
        let dir = scratch("patience");
        let config = dir.join("aterm.toml");
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("a fresh lock is free");
        };
        // Held throughout: the patience runs out and the answer is Busy.
        assert!(matches!(
            take_change_lock(&config, std::time::Duration::from_millis(1)),
            LockTry::Busy
        ));
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
    #[cfg(unix)]
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

    /// The worker's lock is free the moment it ends, even while a child forked
    /// under it has yet to exec: [`ChangeLock`] lets go by `LOCK_UN`, so the park's
    /// marker removal takes the lock in one try. Negative control: the same lock
    /// let go by the close alone stays taken in the parked child's copy.
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

        // Negative control: released by the close alone, the lock lives on in
        // the parked child's copy of the description until that child execs.
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

    /// The debugger's two markers name their flushes (aterm-durable's counting seam):
    /// the change marker a worker records for the child it spawned stays durable; the
    /// observed mode is only ordered, since the next probe observes it again and the
    /// drift watch tells the person of any change.
    #[cfg(unix)]
    #[test]
    fn the_change_marker_stays_durable_and_the_observed_mode_is_only_ordered() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = scratch("flush-tiers");
        let config = dir.join("aterm.toml");
        let slot = Arc::new(ChangeSlot::default());
        slot.arm(1);
        let mut marked = job(DebuggerChange::On, 1, &slot);
        marked.config_path = Some(config.clone());
        let recording = Recording::start();
        run_devtools_job(marked, |_, _, on_spawn| {
            on_spawn(child(77));
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        });
        assert!(dir.join(CHANGE_MARKER).exists(), "the worker's marker");
        assert_eq!(recording.take(), [Flush::Durable], "the change marker");
        let observed = ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 5,
            origin: Origin::Unknown,
        };
        settle_markers(&config, Some(observed), Some(&child(77)));
        assert_eq!(recording.take(), [Flush::Ordered], "the observed mode");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_records_reader_keeps_a_live_child_names_a_stale_one_and_drops_an_unreadable_one() {
        let dir = scratch("records");
        let config = dir.join("aterm.toml");
        let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        write_debugger_marker(
            &config,
            CHANGE_MARKER,
            &marker.encode(),
            crate::durable::Flush::Durable,
        )
        .unwrap();
        let observed = ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 5,
            origin: Origin::Unknown,
        };
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &observed.encode(),
            crate::durable::Flush::Ordered,
        )
        .unwrap();
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
        write_debugger_marker(
            &config,
            CHANGE_MARKER,
            "on not-a-pid",
            crate::durable::Flush::Durable,
        )
        .unwrap();
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
        write_debugger_marker(
            &config,
            CHANGE_MARKER,
            &marker.encode(),
            crate::durable::Flush::Durable,
        )
        .unwrap();
        settle_markers(&config, None, Some(&child(9)));
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "a marker naming another child stays"
        );
        settle_markers(&config, None, Some(&child(4242)));
        assert!(!dir.join(CHANGE_MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A records read while a worker holds the change lock before its child (and
    /// marker) exist settles nothing: the lane keeps its buttons off and reads
    /// again, and the second read finds the orphaned child in flight.
    #[cfg(unix)]
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
        write_debugger_marker(
            &config,
            CHANGE_MARKER,
            &marker.encode(),
            crate::durable::Flush::Durable,
        )
        .unwrap();
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

    /// The worker body, with a stand-in runner: it holds the change lock for the
    /// whole run, publishes and marks the child the moment it starts, then the
    /// result, leaves the marker for the park, and nudges once per event. A
    /// panicking runner still reads as gone; a lock or live marker held elsewhere
    /// runs nothing.
    #[cfg(unix)]
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

    /// Two windowed aterms share one config. The idle one judges the other's
    /// change against the shared record: never set outside aterm, never drift,
    /// never overwriting the peer's `aterm` origin. A change neither made is
    /// still recorded as outside.
    #[test]
    fn a_peer_aterms_change_is_judged_against_the_shared_record() {
        let dir = scratch("peer-drift");
        let config = dir.join("aterm.toml");
        let write = |marker: ObservedMarker| {
            write_debugger_marker(
                &config,
                OBSERVED_MARKER,
                &marker.encode(),
                crate::durable::Flush::Ordered,
            )
            .unwrap();
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

    /// A's change child exits; A takes the result, re-probes, and only then
    /// records it. Peer B, re-reading the shared records inside that window, finds
    /// A's change marker (its child dead) and re-reads the change as aterm's, not
    /// as set outside aterm. The marker goes once the change is recorded.
    #[cfg(unix)]
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
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &before.encode(),
            crate::durable::Flush::Ordered,
        )
        .unwrap();
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

        // Inside the window, B sees the rule on and judges it against a fresh read.
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

    /// A released change is not adopted again: a records read that still finds
    /// this aterm's own dead marker (its removal runs on another thread) neither
    /// re-opens the change nor asks to remove the marker twice.
    #[cfg(unix)]
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
            crate::durable::Flush::Ordered,
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

    /// A records read that finds this aterm's own change's marker, its child
    /// gone, before the park read the child from the slot: one park re-read takes
    /// both, and the marker stays until the re-probe is recorded rather than being
    /// removed as a stranger's.
    #[cfg(unix)]
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
            crate::durable::Flush::Ordered,
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

    /// A re-probe that lands while the drift watch waits for a fresh records read
    /// is judged at that read, and the change's marker waits with it, so a peer
    /// re-reading in between files the change as aterm's.
    #[cfg(unix)]
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
            crate::durable::Flush::Ordered,
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

    /// A change whose child finished while no aterm read its result is still
    /// aterm's: the re-probe records it as the owner's change from aterm, never as
    /// outside or drift, also when a probe landed before the records did.
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

    /// The `WF(Notice)` site, lane level: while a child is in flight the park keeps
    /// re-reading, so a result whose wake was dropped is found by the next re-read,
    /// and so is a predecessor's child that died with an empty slot.
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
        // The child started and the park read it: in flight, and still re-reading
        // every RESULT_RECHECK, since the wake of its exit may never come.
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

    // -----------------------------------------------------------------------
    // The launch ask (owner ruling O29)
    // -----------------------------------------------------------------------

    /// Facts under which the launch ask asks: every gate open, this Mac not in
    /// the record, the records and the probe read (off, an admin), a hardened
    /// aterm, both launch pages over, nothing up, and a quiet moment.
    fn ready() -> LaunchAskFacts {
        LaunchAskFacts {
            windowed: true,
            macos: true,
            privacy: true,
            notice: true,
            config: true,
            host: HostAsked::NotAsked,
            records_read: true,
            under_way: false,
            answered: false,
            probe: Some(probe(DebuggerMode::Disabled)),
            probe_fresh: true,
            recorded: None,
            drift: false,
            self_hardened: SelfHardening::Hardened,
            fda_decided: true,
            devtools_decided: true,
            page: false,
            warmup: false,
            quiet: true,
            owner_quiet: false,
        }
    }

    fn off_as(member: DebuggerMember) -> Option<DebuggerProbe> {
        Some(DebuggerProbe {
            member,
            ..probe(DebuggerMode::Disabled)
        })
    }

    fn recorded(mode: DebuggerMode, origin: Origin) -> Option<ObservedMarker> {
        Some(ObservedMarker {
            mode,
            since: 1_790_000_000,
            origin,
        })
    }

    /// THE LAUNCH ASK'S TABLE: every named outcome, each from the ready facts with
    /// one fact changed, then the order between neighbours. An aterm that is not
    /// hardened, or cannot tell, asks nothing by itself (its button still offers
    /// the change); neither does an account whose membership is unknown; debugger
    /// access on, or seen go off, settles the question for this Mac; and
    /// `[privacy] notice = false` silences it.
    #[test]
    fn the_launch_ask_names_every_outcome_in_its_order() {
        use LaunchAsk::{Ask, Drop, Settle, Wait};
        /// A row: its name, the one change to the ready facts, the outcome.
        type Row = (&'static str, fn(&mut LaunchAskFacts), LaunchAsk);
        assert_eq!(admit_launch_ask(&ready()), Ask);
        let rows: &[Row] = &[
            ("headless", |f| f.windowed = false, Drop(AskDrop::Headless)),
            ("not macOS", |f| f.macos = false, Drop(AskDrop::NotMacos)),
            (
                "privacy off",
                |f| f.privacy = false,
                Drop(AskDrop::PrivacyOff),
            ),
            ("notice off", |f| f.notice = false, Drop(AskDrop::NoticeOff)),
            (
                "no config folder",
                |f| f.config = false,
                Drop(AskDrop::NoConfig),
            ),
            (
                "host cold",
                |f| f.host = HostAsked::Cold,
                Wait(AskWait::HostCold),
            ),
            (
                "no host",
                |f| f.host = HostAsked::Unnamed,
                Drop(AskDrop::NoHost),
            ),
            ("asked", |f| f.host = HostAsked::Asked, Drop(AskDrop::Asked)),
            (
                "records unread",
                |f| f.records_read = false,
                Wait(AskWait::RecordsUnread),
            ),
            (
                "probe unread",
                |f| f.probe = None,
                Wait(AskWait::ProbePending),
            ),
            (
                "probe pending",
                |f| f.probe = Some(DebuggerProbe::pending()),
                Wait(AskWait::ProbePending),
            ),
            (
                "under way",
                |f| f.under_way = true,
                Settle(AskSettle::Underway),
            ),
            (
                "answered",
                |f| f.answered = true,
                Settle(AskSettle::Answered),
            ),
            (
                "enabled",
                |f| f.probe = Some(probe(DebuggerMode::Enabled)),
                Settle(AskSettle::AlreadyOn),
            ),
            ("drift", |f| f.drift = true, Settle(AskSettle::TurnedOff)),
            (
                "recorded on",
                |f| f.recorded = recorded(DebuggerMode::Enabled, Origin::Outside),
                Settle(AskSettle::TurnedOff),
            ),
            (
                "turned off by aterm",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Aterm),
                Settle(AskSettle::TurnedOff),
            ),
            (
                "seen turned off outside aterm",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Outside),
                Settle(AskSettle::TurnedOff),
            ),
            (
                "unrecognized",
                |f| f.probe = Some(probe(DebuggerMode::Unrecognized)),
                Drop(AskDrop::Unrecognized),
            ),
            (
                "status unread",
                |f| f.probe = Some(probe(DebuggerMode::Unknown)),
                Drop(AskDrop::Unread),
            ),
            (
                "probe timed out",
                |f| f.probe = Some(DebuggerProbe::refused(DebuggerProbeLabel::TimedOut)),
                Drop(AskDrop::Unread),
            ),
            (
                "not a member",
                |f| f.probe = off_as(DebuggerMember::NotMember),
                Drop(AskDrop::NotMember),
            ),
            (
                "membership unknown",
                |f| f.probe = off_as(DebuggerMember::Unknown),
                Drop(AskDrop::NotMember),
            ),
            (
                "not hardened",
                |f| f.self_hardened = SelfHardening::NotHardened,
                Drop(AskDrop::NotHardened),
            ),
            (
                "hardening unknown",
                |f| f.self_hardened = SelfHardening::Unknown,
                Drop(AskDrop::NotHardened),
            ),
            (
                "full disk access undecided",
                |f| f.fda_decided = false,
                Wait(AskWait::BehindFullDiskAccess),
            ),
            (
                "developer tools unread",
                |f| f.devtools_decided = false,
                Wait(AskWait::BehindDeveloperTools),
            ),
            ("a page", |f| f.page = true, Wait(AskWait::BehindPage)),
            (
                "the warm-up",
                |f| f.warmup = true,
                Wait(AskWait::BehindWarmup),
            ),
            ("not quiet", |f| f.quiet = false, Wait(AskWait::NotQuiet)),
            (
                "owner quiet",
                |f| f.owner_quiet = true,
                Wait(AskWait::OwnerQuiet),
            ),
            (
                "the probe stale at the moment",
                |f| f.probe_fresh = false,
                Wait(AskWait::ProbeStale),
            ),
            // Off at first sight is no reason to hold back: asked.
            (
                "first sight off",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Unknown),
                Ask,
            ),
            (
                "a developer",
                |f| f.probe = off_as(DebuggerMember::Developer),
                Ask,
            ),
        ];
        for (name, change, expected) in rows {
            let mut facts = ready();
            change(&mut facts);
            assert_eq!(admit_launch_ask(&facts), *expected, "{name}");
        }
        // Every named outcome is reached by some row.
        let every = [
            Ask,
            Drop(AskDrop::Headless),
            Drop(AskDrop::NotMacos),
            Drop(AskDrop::PrivacyOff),
            Drop(AskDrop::NoticeOff),
            Drop(AskDrop::NoConfig),
            Drop(AskDrop::NoHost),
            Drop(AskDrop::Asked),
            Drop(AskDrop::Unrecognized),
            Drop(AskDrop::Unread),
            Drop(AskDrop::NotMember),
            Drop(AskDrop::NotHardened),
            Wait(AskWait::HostCold),
            Wait(AskWait::RecordsUnread),
            Wait(AskWait::ProbePending),
            Wait(AskWait::BehindFullDiskAccess),
            Wait(AskWait::BehindDeveloperTools),
            Wait(AskWait::BehindPage),
            Wait(AskWait::BehindWarmup),
            Wait(AskWait::NotQuiet),
            Wait(AskWait::OwnerQuiet),
            Wait(AskWait::ProbeStale),
            Settle(AskSettle::Underway),
            Settle(AskSettle::Answered),
            Settle(AskSettle::AlreadyOn),
            Settle(AskSettle::TurnedOff),
        ];
        for outcome in every {
            assert!(
                rows.iter().any(|(_, _, expected)| *expected == outcome),
                "{outcome:?} has a row"
            );
        }
        let mut words = std::collections::BTreeSet::new();
        for outcome in every {
            let word = match outcome {
                Ask => "ask",
                Drop(why) => why.as_str(),
                Wait(why) => why.as_str(),
                Settle(why) => why.as_str(),
            };
            assert!(words.insert(word), "{word} names one outcome");
        }

        // The order between neighbours: the first that holds decides.
        let order: &[Row] = &[
            (
                "a gate before the host",
                |f| {
                    f.notice = false;
                    f.host = HostAsked::Cold;
                },
                Drop(AskDrop::NoticeOff),
            ),
            (
                "the record before the reads",
                |f| {
                    f.host = HostAsked::Asked;
                    f.records_read = false;
                    f.probe = None;
                },
                Drop(AskDrop::Asked),
            ),
            (
                "the records before the probe",
                |f| {
                    f.records_read = false;
                    f.probe = None;
                    f.under_way = true;
                },
                Wait(AskWait::RecordsUnread),
            ),
            (
                "the probe before a settle",
                |f| {
                    f.probe = Some(DebuggerProbe::pending());
                    f.under_way = true;
                },
                Wait(AskWait::ProbePending),
            ),
            (
                "under way before the rule",
                |f| {
                    f.under_way = true;
                    f.probe = Some(probe(DebuggerMode::Unrecognized));
                },
                Settle(AskSettle::Underway),
            ),
            (
                "on before aterm's hardening",
                |f| {
                    f.probe = Some(probe(DebuggerMode::Enabled));
                    f.self_hardened = SelfHardening::NotHardened;
                },
                Settle(AskSettle::AlreadyOn),
            ),
            (
                "turned off before the account",
                |f| {
                    f.drift = true;
                    f.probe = Some(DebuggerProbe {
                        member: DebuggerMember::NotMember,
                        ..probe(DebuggerMode::Disabled)
                    });
                },
                Settle(AskSettle::TurnedOff),
            ),
            (
                "the account before the hardening",
                |f| {
                    f.probe = off_as(DebuggerMember::NotMember);
                    f.self_hardened = SelfHardening::Unknown;
                },
                Drop(AskDrop::NotMember),
            ),
            (
                "a drop before the pages",
                |f| {
                    f.self_hardened = SelfHardening::NotHardened;
                    f.fda_decided = false;
                },
                Drop(AskDrop::NotHardened),
            ),
            (
                "full disk access before developer tools",
                |f| {
                    f.fda_decided = false;
                    f.devtools_decided = false;
                    f.page = true;
                },
                Wait(AskWait::BehindFullDiskAccess),
            ),
            (
                "the pages before the moment",
                |f| {
                    f.page = true;
                    f.quiet = false;
                },
                Wait(AskWait::BehindPage),
            ),
            (
                "the moment before the owner's quiet",
                |f| {
                    f.quiet = false;
                    f.owner_quiet = true;
                },
                Wait(AskWait::NotQuiet),
            ),
            // A stale reading is re-read only at the moment, never while the ask
            // waits for it; and a stale `Enabled` still settles at once.
            (
                "the moment before a stale probe",
                |f| {
                    f.owner_quiet = true;
                    f.probe_fresh = false;
                },
                Wait(AskWait::OwnerQuiet),
            ),
            (
                "a stale reading of on settles",
                |f| {
                    f.probe = Some(probe(DebuggerMode::Enabled));
                    f.probe_fresh = false;
                },
                Settle(AskSettle::AlreadyOn),
            ),
        ];
        for (name, change, expected) in order {
            let mut facts = ready();
            change(&mut facts);
            assert_eq!(admit_launch_ask(&facts), *expected, "{name}");
        }
    }

    /// A wait looks again soon only while a read it started completes, and
    /// never at once: a past deadline would spin the park.
    #[test]
    fn a_waiting_launch_ask_looks_again_soon_only_for_a_read() {
        use crate::devtools_helper::{PANEL_TICK, RECHECK_EVERY};
        for why in [
            AskWait::HostCold,
            AskWait::ProbePending,
            AskWait::ProbeStale,
        ] {
            assert_eq!(why.look_again(), PANEL_TICK, "{why:?}");
        }
        assert!(
            ASK_PROBE_FRESH > PANEL_TICK,
            "the reading a stale probe waits for is fresh at the next look"
        );
        for why in [
            AskWait::RecordsUnread,
            AskWait::BehindFullDiskAccess,
            AskWait::BehindDeveloperTools,
            AskWait::BehindPage,
            AskWait::BehindWarmup,
            AskWait::NotQuiet,
            AskWait::OwnerQuiet,
        ] {
            assert_eq!(why.look_again(), RECHECK_EVERY, "{why:?}");
        }
        assert!(!PANEL_TICK.is_zero() && !RECHECK_EVERY.is_zero());
    }

    /// THE ASKED RECORD: absent is not asked; the write is DURABLE (it goes down
    /// before the alert it stands for); one line per Mac, the same Mac never
    /// twice; bounded to the newest [`MAX_ASKED_LINES`] Macs, the oldest dropped
    /// (and so asked again); and a host key no line could hold is refused.
    #[test]
    fn the_asked_record_is_durable_one_line_per_mac_and_bounded() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = scratch("asked");
        let config = dir.join("aterm.toml");
        let path = dir.join(ASKED_RECORD);
        assert!(!read_asked(&config, "MAC-A"), "a genuine absence");
        let recording = Recording::start();
        note_asked(&config, "MAC-A", 100).unwrap();
        assert_eq!(recording.take(), [Flush::Durable], "written ahead, durably");
        drop(recording);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "MAC-A 100\n");
        assert!(read_asked(&config, "MAC-A"));
        assert!(
            !read_asked(&config, "MAC-B"),
            "another Mac on a synced config"
        );
        note_asked(&config, "MAC-A", 200).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "MAC-A 100\n",
            "one line per Mac"
        );
        for (n, host) in ["B", "C", "D", "E", "F", "G", "H"].iter().enumerate() {
            note_asked(&config, &format!("MAC-{host}"), 300 + n as u64).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            MAX_ASKED_LINES
        );
        assert!(read_asked(&config, "MAC-A"));
        note_asked(&config, "MAC-I", 400).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), MAX_ASKED_LINES);
        assert!(text.starts_with("MAC-B 300\n") && text.ends_with("MAC-I 400\n"));
        assert!(!read_asked(&config, "MAC-A"), "the oldest Mac went");
        // The longest record this build writes fits its own bound.
        let long = |i: u8| format!("{}{}", "F".repeat(MAX_HOST_BYTES - 1), char::from(b'0' + i));
        for i in 0..8 {
            note_asked(&config, &long(i), u64::MAX).unwrap();
        }
        assert!(std::fs::metadata(&path).unwrap().len() as usize <= MAX_ASKED_BYTES);
        assert!(read_asked(&config, &long(7)));
        let before = std::fs::read(&path).unwrap();
        let too_long = "F".repeat(MAX_HOST_BYTES + 1);
        for bad in [
            "",
            "MAC A",
            "MAC-A\n",
            "MAC-A 1",
            "mac/a",
            too_long.as_str(),
        ] {
            assert!(read_asked(&config, bad), "{bad:?} reads as asked");
            assert_eq!(
                note_asked(&config, bad, 1).map_err(|e| e.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "{bad:?}"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A record that is there but cannot be read whole reads as ASKED — fail
    /// toward not asking — and is never followed, overwritten or replaced: a
    /// planted link (to a record naming another Mac), a folder, an oversized
    /// file, and every malformed shape.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_asked_record_reads_as_asked_and_is_left_as_found() {
        let dir = scratch("asked-unreadable");
        let config = dir.join("aterm.toml");
        let path = dir.join(ASKED_RECORD);

        let target = dir.join("elsewhere");
        std::fs::write(&target, "MAC-B 1\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(read_asked(&config, "MAC-A"), "a link is never read through");
        assert!(note_asked(&config, "MAC-A", 2).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "MAC-B 1\n");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&path).unwrap();

        std::fs::create_dir(&path).unwrap();
        assert!(read_asked(&config, "MAC-A"), "a folder");
        assert!(note_asked(&config, "MAC-A", 2).is_err());
        assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
        std::fs::remove_dir(&path).unwrap();

        let nine = (0..=MAX_ASKED_LINES)
            .map(|i| format!("MAC-{i} 1\n"))
            .collect::<String>();
        let oversized = format!("MAC-B 1\n{}\n", "x".repeat(MAX_ASKED_BYTES));
        for bytes in [
            oversized.as_bytes(),
            nine.as_bytes(),
            &b""[..],
            &b"\n"[..],
            &b"MAC-B\n"[..],
            &b"MAC-B 1"[..],
            &b"MAC-B x\n"[..],
            &b"MAC-B -1\n"[..],
            &b"MAC-B +1\n"[..],
            &b"MAC-B 1 2\n"[..],
            &b"MAC-B  1\n"[..],
            &b"MAC/B 1\n"[..],
            &b"MAC-B 1\n\n"[..],
            &b"MAC-B 99999999999999999999999\n"[..],
            &b"\xff 1\n"[..],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(
                read_asked(&config, "MAC-A"),
                "{:?} reads as asked",
                String::from_utf8_lossy(bytes)
            );
            assert!(note_asked(&config, "MAC-A", 2).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "left as found");
        }
        // A whole record naming other Macs only is not this Mac's answer.
        std::fs::write(&path, "MAC-B 1\nMAC-C 2\n").unwrap();
        assert!(!read_asked(&config, "MAC-A"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lane where a press of *Allow debugger access…* would be taken: records
    /// read, the probe reading it off.
    #[cfg(unix)]
    fn askable() -> DebuggerChangeLane {
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead::default());
        lane.poll(1);
        lane.observe(probe(DebuggerMode::Disabled), 2);
        lane
    }

    #[cfg(unix)]
    fn never_confirmed() -> OwnerAnswer {
        panic!("no alert may be raised here")
    }

    /// THE LAUNCH OFFER takes the owner's answer exactly as a press: a yes arms the
    /// same job a press arms and gives the ticket; no, nobody asked, and a quiet
    /// give none. The record is written BEFORE the alert in every case, so a
    /// second offer on the same config folder — this lane, or a new one as after
    /// a relaunch — asks nothing; another Mac on that folder is asked.
    #[cfg(unix)]
    #[test]
    fn the_launch_offer_writes_ahead_and_takes_the_answer_as_a_press() {
        let dir = scratch("offer");
        let config = dir.join("aterm.toml");
        let mut lane = askable();
        let ticket = lane
            .offer(Some(&config), "MAC-A", || {
                assert!(read_asked(&config, "MAC-A"), "written before the alert");
                OwnerAnswer::yes_for_test()
            })
            .expect("a yes gives the ticket");
        let mut pressed = askable();
        let press = pressed
            .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        assert_eq!(ticket, press, "the same job a press arms");
        assert_eq!(lane.flow().phase(), Phase::Rechecking(DebuggerChange::On));
        lane.slot().publish_spawned(ticket.job(), child(9));
        lane.poll(3);
        assert!(
            lane.flow().in_flight().is_some(),
            "the worker's job is armed"
        );
        assert_eq!(
            askable().offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Asked),
            "a relaunch on the same config asks nothing"
        );

        for (host, answer, end) in [
            ("MAC-B", OwnerAnswer::No, OfferEnd::Declined),
            ("MAC-C", OwnerAnswer::NotAsked, OfferEnd::NotAsked),
            ("MAC-D", OwnerAnswer::Quiet, OfferEnd::NotAsked),
        ] {
            let mut lane = askable();
            assert_eq!(
                lane.offer(Some(&config), host, move || answer),
                Err(end),
                "{host}"
            );
            assert_eq!(
                lane.flow().phase(),
                Phase::Idle,
                "{host}: nothing under way"
            );
            assert!(read_asked(&config, host), "{host}: recorded all the same");
            assert_eq!(
                lane.offer(Some(&config), host, never_confirmed),
                Err(OfferEnd::Asked),
                "{host}: asked once"
            );
            assert_eq!(
                askable().offer(Some(&config), host, never_confirmed),
                Err(OfferEnd::Asked),
                "{host}: and not after a relaunch"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An offer the press would refuse writes NOTHING and raises nothing: no config
    /// folder, records unread, the probe not showing it off, a change under way
    /// here, a peer's change lock held, a change marker on disk. A record that
    /// cannot be written raises nothing either. `forgo` writes the record alone.
    #[cfg(unix)]
    #[test]
    fn a_refused_launch_offer_writes_nothing_and_forgo_writes_only_the_record() {
        let dir = scratch("offer-refused");
        let config = dir.join("aterm.toml");
        let unwritten = |dir: &Path| !dir.join(ASKED_RECORD).exists();
        assert_eq!(
            askable().offer(None, "MAC-A", never_confirmed),
            Err(OfferEnd::NoRecord)
        );
        let unread = &mut DebuggerChangeLane::with_liveness(|_| false);
        assert_eq!(
            unread.offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Refused(Refusal::RecordsUnread))
        );
        let mut on = askable();
        on.observe(probe(DebuggerMode::Enabled), 3);
        assert_eq!(
            on.offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Refused(Refusal::State(
                DebuggerChange::On,
                DebuggerMode::Enabled
            )))
        );
        let mut busy = askable();
        busy.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        assert_eq!(
            busy.offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Refused(Refusal::InFlight))
        );
        {
            let LockTry::Held(_peer) = take_change_lock(&config, Duration::ZERO) else {
                panic!("the scratch lock is free");
            };
            assert_eq!(
                askable().offer(Some(&config), "MAC-A", never_confirmed),
                Err(OfferEnd::Refused(Refusal::InFlight)),
                "a peer's worker holds the lock"
            );
        }
        let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        std::fs::write(dir.join(CHANGE_MARKER), marker.encode()).unwrap();
        assert_eq!(
            askable().offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Refused(Refusal::InFlight)),
            "a change marker on disk"
        );
        std::fs::remove_file(dir.join(CHANGE_MARKER)).unwrap();
        assert!(unwritten(&dir), "no refusal wrote the record");

        // A folder the record cannot be created in: nothing is asked.
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let beyond = locked.join("sub").join("aterm.toml");
        assert!(!read_asked(&beyond, "MAC-A"), "absent");
        assert_eq!(
            askable().offer(Some(&beyond), "MAC-A", never_confirmed),
            Err(OfferEnd::NoLock)
        );
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        assert!(DebuggerChangeLane::forgo(None, "MAC-A").is_err());
        DebuggerChangeLane::forgo(Some(&config), "MAC-A").unwrap();
        assert!(read_asked(&config, "MAC-A"));
        assert!(!dir.join(CHANGE_MARKER).exists(), "the record alone");
        assert_eq!(
            askable().offer(Some(&config), "MAC-A", never_confirmed),
            Err(OfferEnd::Asked),
            "settled: never asked after"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The App's queue: queued only when asked to be, due at once, then at the
    /// look it was put off to; the record read once per process; over at `end`.
    #[test]
    fn the_launch_ask_queue_looks_when_due_and_reads_the_record_once() {
        let idle = LaunchAskQueue::new(false);
        assert!(!idle.queued() && !idle.due(Instant::now()));
        assert_eq!(idle.next_deadline(), None);
        let mut queue = LaunchAskQueue::queued_for_test(|| Some("MAC-A"));
        let now = Instant::now();
        assert!(queue.due(now) && queue.host_read_ended());
        assert_eq!(
            queue.next_deadline(),
            None,
            "the first look is the next park's"
        );
        assert_eq!(queue.start_host_read().map(|_| ()), None, "read already");
        assert!(
            queue.wait(AskWait::NotQuiet, now + RESULT_RECHECK),
            "a new wait"
        );
        assert!(
            !queue.wait(AskWait::NotQuiet, now + RESULT_RECHECK),
            "the same wait is not logged twice"
        );
        assert!(!queue.due(now) && queue.due(now + RESULT_RECHECK));
        assert_eq!(queue.next_deadline(), Some(now + RESULT_RECHECK));
        assert!(queue.wait(AskWait::BehindPage, now + RESULT_RECHECK));
        let dir = scratch("queue");
        let config = dir.join("aterm.toml");
        assert!(!queue.asked(&config, "MAC-A"));
        note_asked(&config, "MAC-A", 1).unwrap();
        assert!(!queue.asked(&config, "MAC-A"), "read once per process");
        assert!(queue.asked(&config, "MAC-B") == read_asked(&config, "MAC-B"));
        queue.end();
        assert!(!queue.queued() && !queue.due(now + RESULT_RECHECK));
        assert_eq!(queue.next_deadline(), None);
        let mut cold = LaunchAskQueue::new(true);
        let done = cold.start_host_read().expect("the first time");
        assert!(cold.start_host_read().is_none() && !cold.host_read_ended());
        done.store(true, std::sync::atomic::Ordering::Release);
        assert!(cold.host_read_ended());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Refused offers keep the ask queued, looking again later, for at most
    /// [`REFUSED_FOR`] from the first refusal, whatever waits came between: then
    /// it is over for this process, so a disk state the lane never settles cannot
    /// hold the folder warm-up (which waits while the ask is queued) for longer.
    #[test]
    fn refused_offers_hold_the_launch_ask_for_a_bounded_span() {
        use crate::devtools_helper::RECHECK_EVERY;
        let mut queue = LaunchAskQueue::queued_for_test(|| Some("MAC-A"));
        let first = Instant::now();
        assert!(queue.refused(first), "the first refusal looks again");
        assert!(queue.queued() && !queue.due(first));
        assert_eq!(queue.next_deadline(), Some(first + RECHECK_EVERY));
        let mut at = first;
        while at + RECHECK_EVERY < first + REFUSED_FOR {
            at += RECHECK_EVERY;
            assert!(queue.due(at));
            // A wait between refusals does not restart the span.
            queue.wait(AskWait::NotQuiet, at + RECHECK_EVERY);
            assert!(queue.refused(at), "still within the span");
        }
        assert!(!queue.refused(first + REFUSED_FOR), "the span is over");
        assert!(!queue.queued() && queue.next_deadline().is_none());
    }

    /// THE OFFER JUDGES THE DISK AS THE WORKER DOES. A file at the change marker's
    /// path that names no change — a link, a folder, an oversized or malformed
    /// file — is no change: the records reader and the worker ignore it, so the
    /// offer asks. A marker naming a gone child this lane does not follow is
    /// refused, and asks the park for a records read at once, so the lane adopts
    /// it and the next look settles; one this lane follows already is no change.
    /// A lock file the offer cannot open or lock ends it with nothing written: no
    /// yes could start the worker, which refuses without the lock.
    #[cfg(unix)]
    #[test]
    fn the_launch_offer_judges_the_disk_as_the_worker_does() {
        let root = aterm_tempfile::TempDir::new().expect("owned offer-disk fixture");
        let dir = root.path();
        let config = dir.join("aterm.toml");
        let marker_at = dir.join(CHANGE_MARKER);
        let lock_at = dir.join(CHANGE_LOCK);
        let asked = |host| read_asked(&config, host);
        let yes = || OwnerAnswer::yes_for_test();
        let claim = match askable().change_on_disk(&config, now_secs()) {
            OnDisk::Clear(claim) => claim,
            other => panic!("a fresh record can be claimed: {other:?}"),
        };
        assert_eq!(
            askable().offer(Some(&config), "MAC-CLAIM", never_confirmed),
            Err(OfferEnd::Refused(Refusal::InFlight)),
            "another lane cannot ask before the claimed record is written"
        );
        assert!(!asked("MAC-CLAIM"), "the refused contender wrote nothing");
        drop(claim);
        assert!(askable().offer(Some(&config), "MAC-CLAIM", yes).is_ok());
        assert_eq!(
            askable().offer(Some(&config), "MAC-CLAIM", never_confirmed),
            Err(OfferEnd::Asked),
            "the durable claim is visible after the lock is released"
        );
        /// What a stray puts at the marker's path.
        type Plant = fn(&Path);
        let strays: [(&str, &str, Plant); 4] = [
            ("a link", "MAC-A", |at| {
                std::os::unix::fs::symlink("/dev/null", at).unwrap();
            }),
            ("a folder", "MAC-B", |at| std::fs::create_dir(at).unwrap()),
            ("an oversized file", "MAC-C", |at| {
                std::fs::write(at, vec![b'x'; MAX_MARKER_BYTES + 1]).unwrap();
            }),
            ("a malformed file", "MAC-D", |at| {
                std::fs::write(at, "not a change marker\n").unwrap();
            }),
        ];
        for (name, host, plant) in strays {
            plant(&marker_at);
            let mut lane = askable();
            assert!(
                lane.offer(Some(&config), host, yes).is_ok(),
                "{name} at the marker's path is no change"
            );
            assert!(asked(host), "{name}: asked");
            if marker_at.is_dir() && !marker_at.is_symlink() {
                std::fs::remove_dir(&marker_at).unwrap();
            } else {
                std::fs::remove_file(&marker_at).unwrap();
            }
        }

        // A gone child nobody recorded: refused, and read again at once.
        let mut lane = askable();
        // The read its first probe asked for, which seeds the drift watch.
        lane.settle_records(read_records(&config, |_| false));
        lane.poll(3);
        assert!(
            !lane.records_due(now_secs()),
            "an idle lane reads nothing more"
        );
        let gone = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        std::fs::write(&marker_at, gone.encode()).unwrap();
        assert_eq!(
            lane.offer(Some(&config), "MAC-E", never_confirmed),
            Err(OfferEnd::Refused(Refusal::InFlight))
        );
        assert!(!asked("MAC-E"), "nothing written");
        assert!(lane.records_due(now_secs()), "the park reads the records");
        lane.settle_records(read_records(&config, |_| false));
        lane.poll(now_secs());
        assert!(lane.under_way(), "adopted: the next look settles");
        // The same marker, followed by the lane that let it go: no change.
        let mut released = askable();
        released.released = Some(child(4242));
        assert!(
            released.offer(Some(&config), "MAC-E", yes).is_ok(),
            "a gone child this lane follows is no change"
        );
        std::fs::remove_file(&marker_at).unwrap();

        // A lock file that cannot be opened: nothing written, nothing asked.
        std::fs::remove_file(&lock_at).unwrap();
        std::os::unix::fs::symlink("/dev/null", &lock_at).unwrap();
        assert_eq!(
            askable().offer(Some(&config), "MAC-F", never_confirmed),
            Err(OfferEnd::NoLock)
        );
        assert!(!asked("MAC-F"), "nothing written");
        std::fs::remove_file(&lock_at).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Tier-1: the real lane, worker and order against the derived model
// ---------------------------------------------------------------------------

/// Tier-1 `debugger_access_conforms_to_the_model` (design §5.4): drives the production
/// composition — [`DebuggerChangeLane`], the worker body [`run_devtools_job`] on its own thread
/// with the real change lock and marker in a scratch config folder, and the real order
/// `aterm_containment::consent::devtools_change_with_for_test` with a scripted `-status` re-read
/// and a stand-in child — and projects it, direction-normalised, onto
/// `aterm_models_gui::debugger_access_change_model()`. Each step's model guard must equal what
/// the composition did, and a taken step must land on a successor the model admits.
///
/// `phase`, `obs` and `polling` come from the lane (phase 5 is a result it has not taken);
/// `claim` from `native_settings::debugger_row_states`; `lock` from the real `flock` and a marker
/// naming a live child; `asked` from the asked record on disk, through the real reader, for the
/// rig's one Mac; the counters from what the stand-in environment saw, never the lane's own;
/// `world` and `clobbered` are the environment's.
///
/// Three steps are compound: the owner's press-and-answer runs `Press`, `Consent`/`Decline` and,
/// on a yes, `Recheck` and `Launch` up to the park reading the spawned child; the launch ask runs
/// `Offer` and then the same answer (On only: aterm never asks by itself to turn it off); and a
/// probe the lane refuses leaves the row's cached answer alone. A relaunch over the same config
/// folder starts a new lane where the model can be, and is checked from there on.
#[cfg(test)]
// Its harness runs where the change lock does (unix); see the tests below.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) mod conformance {
    use super::*;
    use crate::native_settings::OwnerAnswer;
    use aterm_models_gui::debugger_access_change_model;
    use aterm_spec::derive::Model;
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
        /// A launch ask went past its gates while the asked record already
        /// named this Mac.
        reasked: i64,
        /// A launch ask went past its gates while a change was under way.
        overlapped: i64,
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
        /// The asked record on disk names the rig's Mac (`read_asked`).
        asked: i64,
        seen: &'a Seen,
    }

    /// The projection (the anchors' `project`).
    ///
    /// The model's other actions are the environment's or the mutants' (the
    /// press's on `Buggy = 1`, the launch ask's on `Buggy = 2`), waived here by
    /// name; each mutant's negative control runs the
    /// harness over a composition carrying that defect and requires it to fail
    /// (`the_harness_fails_every_faulty_composition`).
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Drift",
        reason = "Environment: an administrator, a macOS upgrade or a -disable elsewhere moves \
                  the rule back to the change's precondition. The Tier-1 harness moves `world`; \
                  the shipping drift watch only records what a probe reads and never re-enables."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SetOutside",
        reason = "Environment: the rule reaches the change's target outside any child of \
                  aterm's. The Tier-1 harness moves `world`; the worker's re-read then reads \
                  NotDue and spawns nothing."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Customize",
        reason = "Environment: a rule in a shape Apple's tool does not recognise. The Tier-1 \
                  harness moves `world`; the worker's re-read then reads NotDue and spawns nothing."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "Normalize",
        reason = "Environment: a recognised rule written back. The Tier-1 harness moves `world`."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PeerLaunch",
        reason = "Environment: another aterm sharing the config takes the change lock and \
                  starts its child. The Tier-1 harness takes the real flock and names a live \
                  child in the real change marker; the lane's worker then runs nothing."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PeerExit",
        reason = "Environment: the peer's child exits and its worker releases the lock. The \
                  Tier-1 harness drops the flock."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PressWhileInFlight",
        reason = "Buggy = 1 mutant: the lane refuses a press while a child is in flight \
                  (on_press: InFlight); the negative control forces one through and the \
                  harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SkipRecheck",
        reason = "Buggy = 1 mutant: the only order, devtools_change_with, re-reads -status \
                  before it spawns; the negative control runs an order that does not, and the \
                  harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "IgnorePeerLock",
        reason = "Buggy = 1 mutant: run_devtools_job takes the change lock before the re-read \
                  and holds it through the child; the negative control runs a worker that takes \
                  no lock, and the harness fails (the projected lock is not held)."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "ClaimFromExitCode",
        reason = "Buggy = 1 mutant: the reducer's observation and the row's claim come only \
                  from a probe; the negative control makes the lane take the exit as the \
                  target, and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "RetryAfterCancel",
        reason = "Buggy = 1 mutant: an unchanged result ends the change (NotChanged) and the \
                  host starts a worker only after the owner's yes; the negative control retries \
                  without one, and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "SpendBanked",
        reason = "Buggy = 1 mutant: the lane arms a job only inside begin, on the owner's yes \
                  (the scenarios' No/NotAsked/Quiet presses launch nothing); a worker started with no \
                  answer is the RetryAfterCancel control's shape."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "AdoptLive",
        reason = "Buggy = 1 mutant: read_records files a change marker as stale only when its \
                  child is gone by pid and birth; the negative control runs a records reader \
                  that skips that check over a peer's running child, and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "ResultWakeLost",
        reason = "Buggy = 1 mutant: the lane's needs_poll holds while a change is under way, \
                  whatever the wake did; the negative control drops the re-read while a child \
                  is in flight and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "OfferAgain",
        reason = "Buggy = 2 mutant: offer reads the asked record before it writes it, and \
                  writes it before the alert; the negative control offers over a record it \
                  never reads, and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "OfferWhileInFlight",
        reason = "Buggy = 2 mutant: offer is refused while a change is under way (the press's \
                  own refusal, then a change marker or held lock on disk); the negative \
                  control raises the alert over our running child, and the harness fails."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "OfferRunsUnasked",
        reason = "Buggy = 2 mutant: offer takes its answer only from the alert it raises, \
                  through begin; the negative control answers yes without raising one, and the \
                  harness fails (a launch no alert was answered for)."
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
        state.insert("asked", observed.asked);
        state.insert("reasked", observed.seen.reasked);
        state.insert("overlapped", observed.seen.overlapped);
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
        /// A launch ask that never reads the asked record.
        OfferAgain,
        /// A launch ask that never asks whether a change is under way.
        OfferWhileInFlight,
        /// A launch ask that takes a yes no alert gave.
        OfferRunsUnasked,
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
        /// The row's cached probe answer (`ConsentState::debugger`).
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
            // A rig whose folder a relaunch took over leaves it to that one.
            if !self.dir.as_os_str().is_empty() {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
    }

    /// The Mac every rig runs on, as the asked record keys it.
    const RIG_HOST: &str = "00000000-0000-4000-8000-00000000d1b6";

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
            write_debugger_marker(
                &rig.config,
                CHANGE_MARKER,
                &marker.encode(),
                crate::durable::Flush::Durable,
            )
            .unwrap();
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

        /// The next launch on this Mac over the same config folder, with
        /// nothing running: a new lane reads the shared records afresh and has
        /// read no probe yet, while the asked record, the observed record and
        /// the change marker stay as the last process left them, and what
        /// that process saw happen stays seen. The model has no process
        /// boundary, so the new lane must start in a state the model reaches.
        /// Checked from there on.
        fn relaunch(mut self) -> Self {
            assert!(
                self.worker.is_none() && self.peer.is_none() && self.inherited.is_none(),
                "the rig relaunches with nothing running"
            );
            let mut lane = DebuggerChangeLane::with_liveness(alive);
            lane.flow.direction = self.change;
            let mut rig = Self {
                model: debugger_access_change_model(),
                lane,
                change: self.change,
                fault: self.fault,
                // The folder moves to the new rig, which removes it.
                dir: std::mem::take(&mut self.dir),
                config: self.config.clone(),
                world: self.world,
                cached: DebuggerProbe::pending(),
                seen: Seen {
                    fresh: 0,
                    armed: 0,
                    ..self.seen
                },
                consumed: 0,
                worker: None,
                peer: None,
                peer_gone: self.peer_gone,
                inherited: None,
                state: State::new(),
            };
            drop(self);
            rig.lane.settle_records(read_records(&rig.config, alive));
            let step = rig.lane.poll(now_secs());
            rig.apply(step);
            rig.state = rig.now();
            rig.check_invariants();
            assert!(
                aterm_spec::interp::reachable_states(&rig.model).contains(&rig.state),
                "the relaunched lane starts where the model can be: {:?}",
                rig.state
            );
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
            write_debugger_marker(
                &self.config,
                OBSERVED_MARKER,
                &marker.encode(),
                crate::durable::Flush::Ordered,
            )
            .unwrap();
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
            // One try reads the fact: every holder here lets go by `LOCK_UN`, so a
            // busy lock has a holder (the peer, the worker while its child is in
            // flight, or a leaked lock, which fails the step).
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

        /// The asked record on disk names this Mac, as the real reader judges it.
        fn asked_now(&self) -> i64 {
            i64::from(read_asked(&self.config, RIG_HOST))
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
                    asked: self.asked_now(),
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
                // The fault: the host starts the change again with no alert.
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
                            // The fault: spawn on the render's reading.
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
                // The fault: the press guard lets it through.
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
                    // The park reads the spawned child, or why none started.
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

        /// The launch ask at its moment, as `App::offer_debugger_access_at_launch`
        /// runs it: the real `DebuggerChangeLane::offer` writes this Mac into
        /// the asked record, raises the alert (here, `answer`) and takes the
        /// answer as a press's; on a yes the host starts the worker, and the
        /// park reads what it did. `None` is a ticket given; otherwise why
        /// there was none. The offer goes past its gates exactly when the
        /// model's `Offer` is enabled.
        fn offer(&mut self, answer: OwnerAnswer) -> Option<OfferEnd> {
            assert_eq!(
                self.change,
                DebuggerChange::On,
                "aterm never asks by itself to turn debugger access off"
            );
            let pre = self.now();
            assert_eq!(
                pre, self.state,
                "offer: the projection drifted from the model"
            );
            let offer_enabled = self.model.action_enabled("Offer", &pre);
            let answered_yes = matches!(answer, OwnerAnswer::Yes(_));
            let answer_label = format!("{answer:?}");
            let fault = self.fault;
            // Whether the offer went past its gates, its record written, to
            // the moment its alert is raised.
            let mut reached = false;
            let offered = match fault {
                Fault::OfferWhileInFlight
                    if matches!(self.lane.flow.phase(), Phase::InFlight { .. }) =>
                {
                    // The fault: an ask that never looks for a change under way.
                    note_asked(&self.config, RIG_HOST, now_secs()).expect("the scratch record");
                    reached = true;
                    self.lane
                        .begin(DebuggerChange::On, answer)
                        .map_err(OfferEnd::Refused)
                }
                _ => {
                    if fault == Fault::OfferAgain {
                        // The fault: an ask that never reads the record.
                        let _ = std::fs::remove_file(self.dir.join(ASKED_RECORD));
                    }
                    let confirm = || {
                        reached = true;
                        if fault == Fault::OfferRunsUnasked {
                            // The fault: a yes no alert gave.
                            OwnerAnswer::yes_for_test()
                        } else {
                            answer
                        }
                    };
                    self.lane.offer(Some(&self.config), RIG_HOST, confirm)
                }
            };
            if reached {
                if pre["asked"] == 1 {
                    self.seen.reasked = 1;
                }
                if pre["phase"] != 0 || pre["lock"] != 0 || pre["inflight"] != 0 {
                    self.seen.overlapped = 1;
                }
            }
            assert_eq!(
                reached,
                offer_enabled,
                "Offer: the composition {} what the model {}: {pre:?}",
                if reached { "took" } else { "refused" },
                if offer_enabled { "allows" } else { "forbids" },
            );
            let (end, targets) = match offered {
                Ok(ticket) => {
                    if fault != Fault::OfferRunsUnasked {
                        assert!(answered_yes, "a ticket only on a yes");
                        self.seen.fresh = 1;
                    }
                    self.start_worker(ticket);
                    let launched = self
                        .worker
                        .as_ref()
                        .is_some_and(|w| w.env.stage.load(Ordering::SeqCst) == 1);
                    // The park reads the spawned child, or why none started.
                    let step = self.lane.poll(now_secs());
                    self.apply(step);
                    self.account();
                    let targets = if launched {
                        self.chain(&pre, &["Offer", "Consent", "Recheck", "Launch"])
                    } else {
                        self.chain(&pre, &["Offer", "Consent", "Recheck"])
                    };
                    (None, targets)
                }
                Err(end @ (OfferEnd::Declined | OfferEnd::NotAsked)) => {
                    (Some(end), self.chain(&pre, &["Offer", "Decline"]))
                }
                Err(end) => {
                    assert_eq!(self.now(), pre, "a refused offer ({end:?}) changed nothing");
                    return Some(end);
                }
            };
            let post = self.now();
            assert!(
                targets.contains(&post),
                "offer ({answer_label}): {pre:?} -> {post:?} is none of {targets:?}"
            );
            self.state = post;
            self.check_invariants();
            end
        }

        /// The launch ask settled, as the App settles it (a change under way,
        /// an alert the owner answered, the rule on, or on or turned off
        /// before): the real `DebuggerChangeLane::forgo` writes this Mac into
        /// the asked record, in whatever phase the lane is, and nothing else.
        fn forgo(&mut self) {
            self.step("Forgo", |rig| {
                let before = rig.now();
                DebuggerChangeLane::forgo(Some(&rig.config), RIG_HOST)
                    .expect("the scratch record is writable");
                rig.now() != before
            });
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
                    // The fault: the exit read as the target.
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
                write_debugger_marker(
                    &rig.config,
                    CHANGE_MARKER,
                    &marker.encode(),
                    crate::durable::Flush::Durable,
                )
                .unwrap();
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

        /// The records re-read mid-session, as the park runs it when a read is
        /// due: `ask_records`, the reader body over the marker files, and the next
        /// `poll`. `action` names the model step the scenario expects (`Adopt`,
        /// `Follow`, `Discard`); a read that moves nothing is that step refused.
        fn reread(&mut self, action: &str) {
            self.step(action, |rig| {
                let before = rig.now();
                let liveness: fn(&DevtoolsChild) -> bool = if rig.fault == Fault::AdoptLive {
                    // The fault: no pid-and-birth check, so every child is gone.
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

    /// The happy path in both directions, a decline, a cancelled dialog, a run the
    /// moved rule stops, the named race, drift, a failed re-probe, a peer's change,
    /// and a predecessor's.
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

            // The named residual race: customised while the child waits, and its
            // write lands over it. Still conformant.
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

            // A records re-read mid-session: a peer's running child is followed as
            // in flight; a peer's change whose child is gone, its marker on disk,
            // is adopted as aterm's and re-probed; one the peer recorded leaves
            // nothing; and a dead marker found while this lane's own change is
            // under way is removed.
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
        launch_ask_scenarios(fault);
    }

    /// The launch ask (owner ruling O29), On only: refused before the probe; a
    /// yes that runs the change as a press's would, after which the record
    /// refuses every later ask; a No, or no alert, that the record carries into
    /// the next launch while the button stays the owner's; refused under a
    /// peer's lock and over its dead marker; refused during our own child,
    /// which settles it; settled first and never asked; and refused over a
    /// predecessor's running child.
    fn launch_ask_scenarios(fault: Fault) {
        let on = DebuggerChange::On;
        let yes = OwnerAnswer::yes_for_test;
        let unread = Some(OfferEnd::Refused(Refusal::State(on, DebuggerMode::Unknown)));
        let in_flight = Some(OfferEnd::Refused(Refusal::InFlight));

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        assert_eq!(
            rig.offer(yes()),
            unread,
            "nothing is asked before the probe"
        );
        assert_eq!(rig.state["asked"], 0, "and nothing is recorded");
        rig.probe();
        assert_eq!(rig.offer(yes()), None);
        assert_eq!(rig.offer(yes()), in_flight);
        rig.exit("ChildApplied");
        rig.notice();
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(on)));
        assert_eq!(
            rig.recorded(),
            Some((1, Origin::Aterm)),
            "filed as a press's"
        );
        rig.environment("Drift", 0);
        rig.probe();
        assert_eq!(
            rig.offer(yes()),
            Some(OfferEnd::Asked),
            "asked once per Mac"
        );
        rig.forgo(); // refused: the record names this Mac already

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        assert_eq!(rig.offer(OwnerAnswer::No), Some(OfferEnd::Declined));
        assert_eq!(rig.offer(yes()), Some(OfferEnd::Asked));
        let mut rig = rig.relaunch();
        assert_eq!(rig.offer(yes()), unread);
        rig.probe();
        assert_eq!(
            rig.offer(yes()),
            Some(OfferEnd::Asked),
            "the next launch asks nothing"
        );
        rig.press(yes());
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(on)));
        assert_eq!(rig.seen.launches, 1, "the button is the owner's still");

        for answer in [OwnerAnswer::NotAsked, OwnerAnswer::Quiet] {
            let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
            rig.probe();
            assert_eq!(rig.offer(answer), Some(OfferEnd::NotAsked));
            assert_eq!(rig.offer(yes()), Some(OfferEnd::Asked), "the record stands");
            assert_eq!(rig.seen.launches, 0);
        }

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        rig.peer_launch();
        assert_eq!(
            rig.offer(yes()),
            in_flight,
            "a peer's change holds the lock"
        );
        rig.peer_exit();
        assert_eq!(
            rig.offer(yes()),
            in_flight,
            "its marker waits for that peer"
        );
        rig.peer_settle();
        assert_eq!(rig.offer(OwnerAnswer::No), Some(OfferEnd::Declined));

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        rig.press(yes());
        assert_eq!(rig.offer(yes()), in_flight, "our own child is in flight");
        rig.forgo();
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.offer(yes()), Some(OfferEnd::Asked), "settled under way");

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.forgo();
        rig.probe();
        assert_eq!(
            rig.offer(yes()),
            Some(OfferEnd::Asked),
            "settled, never asked"
        );

        let mut rig = Rig::inherited(on, DebuggerMode::Disabled);
        assert_eq!(rig.offer(yes()), in_flight, "a predecessor's child runs");
        rig.forgo();
    }

    // The change lock is a `flock` (`take_change_lock`): off unix it is
    // unavailable by design, so the harness and its negative controls run
    // where the lock does.
    #[cfg(unix)]
    #[test]
    fn debugger_access_conforms_to_the_model() {
        scenarios(Fault::None);
    }

    /// Negative controls: the same harness over a composition carrying each defect
    /// the model exists to catch must fail.
    #[cfg(unix)]
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
            Fault::OfferAgain,
            Fault::OfferWhileInFlight,
            Fault::OfferRunsUnasked,
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
                // A No, then the next launch's ask over the record it wrote.
                Fault::OfferAgain => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.offer(OwnerAnswer::No);
                    let mut rig = rig.relaunch();
                    rig.probe();
                    rig.offer(OwnerAnswer::No);
                }
                Fault::OfferWhileInFlight => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.press(OwnerAnswer::yes_for_test());
                    rig.offer(OwnerAnswer::No);
                }
                // The owner would say no; the composition never asks them.
                Fault::OfferRunsUnasked => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.offer(OwnerAnswer::No);
                }
                _ => scenarios(fault),
            });
            // A launch-ask fault is caught by the step that carries it, not by
            // whatever else breaks after.
            let caught_by = match fault {
                Fault::OfferAgain | Fault::OfferWhileInFlight => {
                    Some("Offer: the composition took what the model forbids")
                }
                Fault::OfferRunsUnasked => Some("offer (No): "),
                _ => None,
            };
            match (caught, caught_by) {
                (Ok(()), _) => panic!("{fault:?}: the harness passed a faulty composition"),
                (Err(payload), Some(expected)) => {
                    let message = payload
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| payload.downcast_ref::<&str>().copied())
                        .unwrap_or_default();
                    assert!(
                        message.contains(expected),
                        "{fault:?}: caught by something else: {message}"
                    );
                }
                (Err(_), None) => {}
            }
        }
    }
}

/// `DebuggerAccessChange` actions no shipping function here performs, each waived where
/// its Tier-1 (`conformance`) binds it.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Drift",
    reason = "Environment: an administrator, a macOS upgrade or a -disable elsewhere moves \
              the rule back to the change's precondition. The Tier-1 harness moves `world`; \
              the shipping drift watch only records what a probe reads and never re-enables."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SetOutside",
    reason = "Environment: the rule reaches the change's target outside any child of \
              aterm's. The Tier-1 harness moves `world`; the worker's re-read then reads \
              NotDue and spawns nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Customize",
    reason = "Environment: a rule in a shape Apple's tool does not recognise. The Tier-1 \
              harness moves `world`; the worker's re-read then reads NotDue and spawns nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "Normalize",
    reason = "Environment: a recognised rule written back. The Tier-1 harness moves `world`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PeerLaunch",
    reason = "Environment: another aterm sharing the config takes the change lock and \
              starts its child. The Tier-1 harness takes the real flock and names a live \
              child in the real change marker; the lane's worker then runs nothing."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PeerExit",
    reason = "Environment: the peer's child exits and its worker releases the lock. The \
              Tier-1 harness drops the flock."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PressWhileInFlight",
    reason = "Buggy = 1 mutant: the lane refuses a press while a child is in flight \
              (on_press: InFlight); the negative control forces one through and the \
              harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SkipRecheck",
    reason = "Buggy = 1 mutant: the only order, devtools_change_with, re-reads -status \
              before it spawns; the negative control runs an order that does not, and the \
              harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "IgnorePeerLock",
    reason = "Buggy = 1 mutant: run_devtools_job takes the change lock before the re-read \
              and holds it through the child; the negative control runs a worker that takes \
              no lock, and the harness fails (the projected lock is not held)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "ClaimFromExitCode",
    reason = "Buggy = 1 mutant: the reducer's observation and the row's claim come only \
              from a probe; the negative control makes the lane take the exit as the \
              target, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "RetryAfterCancel",
    reason = "Buggy = 1 mutant: an unchanged result ends the change (NotChanged) and the \
              host starts a worker only after the owner's yes; the negative control retries \
              without one, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "SpendBanked",
    reason = "Buggy = 1 mutant: the lane arms a job only inside begin, on the owner's yes \
              (the scenarios' No/NotAsked/Quiet presses launch nothing); a worker started with no \
              answer is the RetryAfterCancel control's shape."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "AdoptLive",
    reason = "Buggy = 1 mutant: read_records files a change marker as stale only when its \
              child is gone by pid and birth; the negative control runs a records reader \
              that skips that check over a peer's running child, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "ResultWakeLost",
    reason = "Buggy = 1 mutant: the lane's needs_poll holds while a change is under way, \
              whatever the wake did; the negative control drops the re-read while a child \
              is in flight and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "OfferAgain",
    reason = "Buggy = 2 mutant: offer reads the asked record before it writes it, and \
              writes it before the alert; the negative control offers over a record it \
              never reads, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "OfferWhileInFlight",
    reason = "Buggy = 2 mutant: offer is refused while a change is under way (the press's \
              own refusal, then a change marker or held lock on disk); the negative \
              control raises the alert over our running child, and the harness fails."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "OfferRunsUnasked",
    reason = "Buggy = 2 mutant: offer takes its answer only from the alert it raises, \
              through begin; the negative control answers yes without raising one, and the \
              harness fails (a launch no alert was answered for)."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn debugger_access_change_waivers() {}
