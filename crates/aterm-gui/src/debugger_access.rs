// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owner debugger-access change.
//!
//! A press on Settings ▸ Security raises aterm's `confirm_owner` alert, and the owner's yes
//! there is the only start of a change. Setup's one early elevation turns developer mode on by
//! itself with the Mac password the rest of setup's root steps take
//! (`aterm_machines::root_steps`), so nothing else here raises macOS's
//! SamplingTools dialog.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DevtoolsChangeRun, DevtoolsChild, ProbeGate, ProcessBirth, SelfHardening,
};

/// How often the park re-reads the shared result slot while a change is under way.
pub(crate) const RESULT_RECHECK: Duration = Duration::from_secs(5);

/// The marker beside `aterm.toml` naming a change child in flight.
pub(crate) const CHANGE_MARKER: &str = "privacy-debugger-change";

/// The marker beside `aterm.toml` holding the last observed mode, when aterm first saw it, and
/// who set it (design §3.5).
pub(crate) const OBSERVED_MARKER: &str = "privacy-debugger-observed";

/// The Mac-wide change lock beside `aterm.toml`.
pub(crate) const CHANGE_LOCK: &str = "privacy-debugger-change.lock";

/// Both markers hold one short line; anything bigger is not a record this build wrote, and
/// reads as nothing.
pub(crate) const MAX_MARKER_BYTES: usize = 96;

/// Wall-clock seconds since the epoch: the markers' and the page's clock.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The `privacy-debugger-change` record: `<on|off> <pid> <secs>.<usecs> <since>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChangeMarker {
    pub(crate) change: DebuggerChange,
    pub(crate) child: DevtoolsChild,
    /// When the child started, in wall seconds.
    pub(crate) since: u64,
}

impl ChangeMarker {
    /// A record for `child`, or `None` when its birth could not be read at spawn.
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
        let uint = |s: &str| {
            s.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        let mut words = text.split_whitespace();
        let change = DebuggerChange::from_token(words.next()?)?;
        let pid = u32::try_from(uint(words.next()?)?).ok()?;
        let (seconds, microseconds) = words.next()?.split_once('.')?;
        let birth = ProcessBirth {
            seconds: uint(seconds)?,
            microseconds: uint(microseconds)?,
        };
        let since = uint(words.next()?)?;
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
    Aterm,
    Outside,
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
        let since = words
            .next()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
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

/// The held change lock: released by `LOCK_UN` when this drops
/// ([`HeldAdvisoryLock`](crate::native_document_host::HeldAdvisoryLock)), or by the kernel when
/// its process dies.
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
    Held(ChangeLock),
    Busy,
    Unavailable(std::io::Error),
}

/// [`take_change_lock`] in one try, never waiting.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn try_change_lock(config_path: &Path) -> LockTry {
    take_change_lock(config_path, Duration::ZERO)
}

/// How long record observations and marker cleanup wait behind a sibling writer.
pub(crate) const CHANGE_LOCK_PATIENCE: Duration = Duration::from_secs(1);

/// Hang detector for an admitted change waiting for its predecessor's native lock.
const REQUIRED_CHANGE_HANG: Duration = Duration::from_secs(60);

fn change_lock_patience() -> Duration {
    #[cfg(test)]
    if let Some(patience) = PATIENCE_FOR_TEST.with(std::cell::Cell::get) {
        return patience;
    }
    CHANGE_LOCK_PATIENCE
}

#[cfg(test)]
thread_local! {
    static PATIENCE_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// This test thread's record-observation lock budget until the guard drops.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct PatienceForTest(Option<Duration>);

#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn patience_for_test(patience: Duration) -> PatienceForTest {
    PatienceForTest(PATIENCE_FOR_TEST.with(|slot| slot.replace(Some(patience))))
}

#[cfg(test)]
impl Drop for PatienceForTest {
    fn drop(&mut self) {
        PATIENCE_FOR_TEST.with(|slot| slot.set(self.0));
    }
}

/// Take the change lock beside `config_path`, waiting out a busy lock for up to `patience`
/// ([`acquire_advisory_lock_within`](crate::native_document_host::acquire_advisory_lock_within)).
pub(crate) fn take_change_lock(config_path: &Path, patience: Duration) -> LockTry {
    take_change_lock_with_clock(config_path, patience, std::time::Instant::now)
}

fn take_change_lock_with_clock(
    config_path: &Path,
    patience: Duration,
    now: impl FnMut() -> std::time::Instant,
) -> LockTry {
    let Some(path) = marker_path(config_path, CHANGE_LOCK) else {
        return LockTry::Unavailable(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to lock in",
        ));
    };
    #[cfg(unix)]
    {
        use crate::native_document_host::{
            AdvisoryLockRefusal, acquire_advisory_lock_within_clock,
        };
        use std::os::unix::fs::OpenOptionsExt as _;
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            Err(error) => return LockTry::Unavailable(error),
        };
        match acquire_advisory_lock_within_clock(file, patience, now) {
            Ok(held) => LockTry::Held(ChangeLock { _held: held }),
            Err(AdvisoryLockRefusal::Busy) => LockTry::Busy,
            Err(AdvisoryLockRefusal::Failed(error)) => LockTry::Unavailable(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, patience, now);
        LockTry::Unavailable(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "debugger access is a macOS setting",
        ))
    }
}

/// Write the observed record, then, under the change lock, remove an ended change's marker if
/// it still names `release`'s child.
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
    let lock = release.map(|_| take_change_lock(config_path, change_lock_patience()));
    if let Some(marker) = observed
        && let Err(error) = write_debugger_marker(
            config_path,
            OBSERVED_MARKER,
            &marker.encode(),
            crate::durable::Flush::Ordered,
        )
    {
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

/// [`settle_markers`] off the UI thread: two tiny files, but disk I/O, and a lock it may wait a
/// second for.
pub(crate) fn settle_markers_detached(
    config_path: PathBuf,
    observed: Option<ObservedMarker>,
    release: Option<DevtoolsChild>,
) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-debugger-marker",
        crate::qos::Role::Background,
        move || {
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
    /// A change marker naming a gone child whose change was never judged (aterm quit before the
    /// re-probe, or a peer is between taking its result and recording its re-probe).
    pub(crate) stale: Option<ChangeMarker>,
    /// The last observed mode.
    pub(crate) observed: Option<ObservedMarker>,
    /// No change marker, but the change lock was held past its patience.
    pub(crate) unsettled: bool,
}

/// Read both markers, off the UI thread, judging a change marker's child by pid and birth
/// (`alive`), since a pid may be reused.
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
        None if !marker_path(config_path, CHANGE_LOCK)
            .is_some_and(|path| path.symlink_metadata().is_ok()) =>
        {
            None
        }
        None => match take_change_lock(config_path, change_lock_patience()) {
            LockTry::Held(_lock) => read_debugger_marker(config_path, CHANGE_MARKER),
            LockTry::Busy => {
                read.unsettled = true;
                return read;
            }
            LockTry::Unavailable(_) => None,
        },
    };
    if let Some(text) = change {
        match ChangeMarker::parse(&text) {
            Some(marker) if alive(&marker.child) => read.inherited = Some(marker),
            Some(marker) => read.stale = Some(marker),
            None => match take_change_lock(config_path, change_lock_patience()) {
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

/// Start the one records reader.
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
            let read = read_records(&config_path, alive);
            *into.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
            wake();
        },
    )
    .is_ok()
}

/// Where the owner's change stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Confirming(DebuggerChange),
    Rechecking(DebuggerChange),
    InFlight {
        change: DebuggerChange,
        child: DevtoolsChild,
        since: u64,
        inherited: bool,
    },
    Exited(DebuggerChange),
}

/// What a change came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Changed(DebuggerChange),
    NotChanged(DebuggerChange),
    Unrecognized(DebuggerChange),
    Unobserved(DebuggerChange),
    NotDue(DebuggerChange, DebuggerMode),
    NotStarted(DebuggerChange),
    PeerInFlight(DebuggerChange),
}

impl Outcome {
    /// Whether this outcome counts as the owner's decline for `OwnerQuiet`.
    pub(crate) const fn counts_as_decline(self) -> bool {
        matches!(self, Self::NotChanged(_))
    }
}

/// Why a press, or the host's start of a confirmed one, did nothing, in the owner's words for
/// "Nothing was changed: {why}.".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    NoWindow,
    NotMacos,
    PrivacyOff,
    RecordsUnread,
    InFlight,
    NotConfirmed,
    Busy,
    State(DebuggerChange, DebuggerMode),
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
    NotAsked,
    Declined,
    Consented,
}

/// The reducer.
#[derive(Clone, Debug)]
pub(crate) struct DebuggerAccessFlow {
    phase: Phase,
    records_read: bool,
    observed: DebuggerMode,
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

    /// Anything under way, or the predecessor's records unread: the buttons are disabled.
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

    /// The shared records were read, at launch or again mid-session.
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

    /// Whether a probe is taken now: idle, or after the child exited (the model's `Probe` guard).
    pub(crate) const fn accepts_probe(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Exited(_))
    }

    /// A completed probe.
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

    /// Why a press of `change` would be refused now, taking nothing.
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

    /// The owner pressed the button for `change`.
    pub(crate) fn on_press(&mut self, change: DebuggerChange) -> Result<(), Refusal> {
        if let Some(refusal) = self.press_refusal(change) {
            return Err(refusal);
        }
        self.direction = change;
        self.phase = Phase::Confirming(change);
        Ok(())
    }

    /// The owner's answer in aterm's alert.
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
    pub(crate) const fn on_child_spawned(&mut self, child: DevtoolsChild, since: u64) -> bool {
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

    /// The worker's re-read did not show the precondition: nothing was spawned, and what it
    /// read is the flow's observation now.
    pub(crate) const fn on_not_due(&mut self, observed: DebuggerProbe) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.observed = observed.mode;
        self.phase = Phase::Idle;
        let outcome = Outcome::NotDue(change, observed.mode);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// Another aterm holds the change lock: nothing was read or spawned for the change being
    /// re-checked.
    pub(crate) const fn on_peer_busy(&mut self) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.phase = Phase::Idle;
        let outcome = Outcome::PeerInFlight(change);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// No child was started for the change being re-checked.
    pub(crate) const fn on_not_started(&mut self) -> Option<Outcome> {
        let Phase::Rechecking(change) = self.phase else {
            return None;
        };
        self.phase = Phase::Idle;
        let outcome = Outcome::NotStarted(change);
        self.last = Some(outcome);
        Some(outcome)
    }

    /// The park took the exited child's result, or the child is gone with none (lost).
    pub(crate) const fn on_child_exit(&mut self) -> bool {
        let Phase::InFlight { change, .. } = self.phase else {
            return false;
        };
        self.phase = Phase::Exited(change);
        true
    }
}

/// What the host's start re-checks besides the reducer (design §3.4 step 2).
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

/// Why a worker ran nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerRefusal {
    PeerChanging,
    NoLock,
}

#[derive(Debug, Default)]
struct SlotInner {
    job: u64,
    spawned: Option<DevtoolsChild>,
    result: Option<DevtoolsChangeRun>,
    refused: Option<WorkerRefusal>,
    gone: bool,
}

/// The shared result slot (design §3.4 step 3.5).
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

    /// Whether `job`'s worker has ended with something the park has not taken yet (the model's
    /// phase 5 when a child ran).
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

    /// Take what the worker published for `job` since the last read.
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

/// One confirmed change: the direction, its job number and the owner's yes in aterm's alert.
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

/// Everything the worker needs, handed through `ConsentGestures::run_devtools`, a plain
/// function pointer, so the job carries its captures.
pub(crate) struct DevtoolsJob {
    change: DebuggerChange,
    gate: ProbeGate,
    job: u64,
    slot: Arc<ChangeSlot>,
    config_path: Option<PathBuf>,
    wake: Arc<dyn Fn() + Send + Sync>,
    alive: fn(&DevtoolsChild) -> bool,
}

impl DevtoolsJob {
    /// The job for one confirmed change; `ticket` carries the direction, the job number and
    /// the owner's yes.
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
    Started,
    Inert,
    SpawnFailed,
}

/// The inert arm: a headless instance and every unit test.
pub(crate) fn inert_run_devtools(_job: DevtoolsJob) -> DevtoolsSpawn {
    DevtoolsSpawn::Inert
}

/// The live arm: one detached `consent-devtools` worker, counted as an up macOS dialog
/// ([`aterm_types::macos_setup::MacosDialog`]) while it lives.
pub(crate) fn spawn_devtools_worker(job: DevtoolsJob) -> DevtoolsSpawn {
    // Taken before the spawn, so the UI thread's next `macos_dialog_up` already counts it, and
    // dropped when the worker ends however it ends (a panic too), or with the closure if the
    // thread cannot start.
    let dialog = aterm_types::macos_setup::MacosDialog::up();
    let spawned = crate::qos::spawn_lane_thread(
        "consent-devtools",
        crate::qos::Role::Responsive,
        move || {
            let _dialog = dialog;
            run_devtools_job(job, |change, gate, on_spawn| {
                aterm_containment::consent::run_devtools_change(change, gate, on_spawn)
            });
        },
    );
    match spawned {
        Ok(_handle) => DevtoolsSpawn::Started,
        Err(error) => {
            aterm_log::warn!("debugger access: the change worker could not start: {error}");
            DevtoolsSpawn::SpawnFailed
        }
    }
}

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

/// Run a debugger change using the native clock.
pub(crate) fn run_devtools_job<R>(job: DevtoolsJob, run: R)
where
    R: FnOnce(DebuggerChange, ProbeGate, Box<dyn FnOnce(DevtoolsChild) + '_>) -> DevtoolsChangeRun,
{
    run_devtools_job_with_clock(job, run, std::time::Instant::now)
}

/// The worker's body, with the change runner injected (live:
/// `aterm_containment::consent::run_devtools_change`; in tests,
/// `devtools_change_with_for_test`).
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
fn run_devtools_job_with_clock<R>(
    job: DevtoolsJob,
    run: R,
    mut now: impl FnMut() -> std::time::Instant,
) where
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
    let lock = match config_path
        .map(|path| take_change_lock_with_clock(path, REQUIRED_CHANGE_HANG, &mut now))
    {
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
            wake();
        }),
    );
    drop(lock);
    slot.publish_result(id, result);
}

/// The `LogOnly` records of design §3.8, as a fact; the words are
/// `message_reporters::debugger_access_record`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebuggerRecord {
    On,
    OnOutside,
    Off,
    WasOnNowOff,
}

/// What one observation asks the host to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DriftStep {
    pub(crate) record: Option<DebuggerRecord>,
    /// The observed marker to write (only when the mode changed).
    pub(crate) write: Option<ObservedMarker>,
}

/// The observed-mode record and drift (design §3.5).
#[derive(Clone, Debug, Default)]
pub(crate) struct DriftWatch {
    seeded: bool,
    recorded: Option<ObservedMarker>,
    early: Option<(DebuggerMode, Option<DebuggerChange>)>,
    drift_since: Option<u64>,
}

impl DriftWatch {
    pub(crate) const fn recorded(&self) -> Option<ObservedMarker> {
        self.recorded
    }

    pub(crate) const fn drift_since(&self) -> Option<u64> {
        self.drift_since
    }

    /// Whether the watch judges now: `false` until the marker is read, and again while a mode
    /// waits for a fresh read of it.
    pub(crate) const fn seeded(&self) -> bool {
        self.seeded
    }

    /// The marker was read.
    pub(crate) fn seed(
        &mut self,
        recorded: Option<ObservedMarker>,
        now: u64,
        under_way: bool,
    ) -> DriftStep {
        self.seeded = true;
        self.recorded = recorded;
        if recorded.is_some_and(|m| m.mode == DebuggerMode::Enabled || m.origin == Origin::Aterm) {
            self.drift_since = None;
        }
        match self.early.take() {
            Some((mode, attributed @ Some(_))) => self.judge(mode, attributed, now),
            Some((mode, None)) if !under_way => self.judge(mode, None, now),
            _ => DriftStep::default(),
        }
    }

    /// One completed probe's mode.
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
            let kept = self
                .early
                .and_then(|(seen, change)| change.filter(|_| seen == mode));
            self.seeded = false;
            self.early = Some((mode, attributed.filter(|_| ours).or(kept)));
            return DriftStep::default();
        }
        self.judge(mode, attributed, now)
    }

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

/// What one poll or observation asks the host to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneStep {
    /// Retire the cached probe: the rule may have changed under it.
    pub(crate) invalidate_probe: bool,
    /// The change marker naming this child may go.
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

/// The `App`-owned lane: the reducer, the shared result slot, the records reader's answer and
/// the drift watch.
pub(crate) struct DebuggerChangeLane {
    flow: DebuggerAccessFlow,
    slot: Arc<ChangeSlot>,
    job: u64,
    job_done: bool,
    child: Option<DevtoolsChild>,
    settling: Option<DevtoolsChild>,
    released: Option<DevtoolsChild>,
    windowed: bool,
    records: Arc<Mutex<Option<RecordsRead>>>,
    records_asked: bool,
    records_out: bool,
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

const fn never_alive(_child: &DevtoolsChild) -> bool {
    false
}

impl DebuggerChangeLane {
    /// A windowed instance reads liveness by pid and birth; a headless one never has a change
    /// child, and asks nothing.
    pub(crate) fn new(headless: bool) -> Self {
        let mut lane = Self::with_liveness(if headless {
            never_alive
        } else {
            DevtoolsChild::is_running
        });
        lane.windowed = !headless;
        lane
    }

    /// A windowed lane with an injected liveness check (the successor's pid-and-birth read);
    /// tests pass a stand-in.
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

    /// Whether the park would find something unread.
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

    /// Whether the park starts a records reader now.
    pub(crate) fn records_due(&self, now_wall: u64) -> bool {
        !self.records_out
            && (!self.records_asked || self.records_again_at.is_some_and(|at| now_wall >= at))
    }

    /// The records reader was started, or there is nothing to read: the slot it publishes into.
    pub(crate) fn ask_records(&mut self) -> Arc<Mutex<Option<RecordsRead>>> {
        self.records_asked = true;
        self.records_out = true;
        self.records_again_at = None;
        Arc::clone(&self.records)
    }

    /// Settle the records without a reader: no marker location (headless, tests), or its
    /// thread could not start.
    pub(crate) fn settle_records(&mut self, read: RecordsRead) {
        *self.ask_records().lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
    }

    /// Record why the last confirmed change started nothing.
    pub(crate) const fn note_refusal(&mut self, refusal: Option<Refusal>) {
        self.refusal = refusal;
    }

    /// Whether the park must keep re-reading: a change under way, or the records asked for and
    /// not yet read.
    pub(crate) const fn needs_poll(&self) -> bool {
        (self.records_asked && !self.flow.records_read())
            || matches!(
                self.flow.phase(),
                Phase::Rechecking(_) | Phase::InFlight { .. } | Phase::Exited(_)
            )
    }

    /// Whether the park calls [`Self::poll`]: [`Self::needs_poll`] (the model's `polling`), or
    /// a records read out or wanted.
    pub(crate) fn wants_poll(&self) -> bool {
        self.needs_poll() || self.records_out || self.records_again_at.is_some()
    }

    /// The park's deadline for [`Self::poll`].
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.wants_poll().then(|| now + RESULT_RECHECK)
    }

    /// Whether the flow waits for a fresh probe to end the change.
    pub(crate) const fn awaiting_probe(&self) -> bool {
        matches!(self.flow.phase(), Phase::Exited(_))
    }

    pub(crate) fn under_way(&self) -> bool {
        self.flow.phase() != Phase::Idle
    }

    /// Whether debugger access reading off is the person's choice, from what this lane holds
    /// on record ([`off_is_chosen`]).
    pub(crate) fn off_is_chosen(&self) -> bool {
        off_is_chosen(self.drift.drift_since().is_some(), self.drift.recorded())
    }

    /// The owner's press and their answer in aterm's alert on Settings ▸ Security; a yes arms a
    /// new job in the slot.
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

    /// One park re-read (design §3.4 steps 4 and 6): the worker's slot, then the records, then
    /// a predecessor's child's liveness.
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
                self.records_again_at = Some(now_wall.saturating_add(RESULT_RECHECK.as_secs()));
            } else {
                let stale = read.stale.filter(|m| !self.follows(&m.child));
                if self
                    .flow
                    .on_records(read.inherited, stale.map(|m| m.change))
                {
                    self.settling = stale.map(|m| m.child);
                } else {
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
            self.flow.on_child_exit();
            step.invalidate_probe = true;
            self.settling = Some(child);
        }
        self.release_settled(&mut step);
        step
    }

    /// A completed probe (the cached `ConsentState::debugger` answer).
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

    fn follows(&self, child: &DevtoolsChild) -> bool {
        self.followed().contains(&Some(*child))
    }

    fn followed(&self) -> [Option<DevtoolsChild>; 4] {
        [
            self.child,
            self.settling,
            self.released,
            self.flow.in_flight().map(|(_, child, _, _)| child),
        ]
    }

    fn release_settled(&mut self, step: &mut LaneStep) {
        if !matches!(self.flow.phase(), Phase::Exited(_)) && self.drift.seeded() {
            let settled = self.settling.take();
            self.released = settled.or(self.released);
            step.remove_change_marker = step.remove_change_marker.or(settled);
        }
    }

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

/// Whether debugger access reading off is the person's choice: this process saw it go off
/// (`drift`), or the observed record says it was allowed or that aterm or something else turned
/// it off. Pure, so every launch judges it the same way.
pub(crate) fn off_is_chosen(drift: bool, recorded: Option<ObservedMarker>) -> bool {
    drift
        || recorded
            .is_some_and(|was| was.mode == DebuggerMode::Enabled || was.origin != Origin::Unknown)
}

/// A change child in flight, for the row's "waiting" line (since `<time>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InFlightFacts {
    pub(crate) change: DebuggerChange,
    /// When the child started, in wall seconds.
    pub(crate) since: u64,
    /// A predecessor's child, read from its marker.
    pub(crate) inherited: bool,
}

/// The debugger row's facts (design §3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DebuggerFacts {
    /// `[privacy] enabled && check`.
    pub(crate) enabled: bool,
    /// The cached probe: the rule's mode, the account's membership, and why.
    pub(crate) probe: DebuggerProbe,
    /// aterm's own hardening.
    pub(crate) self_hardened: SelfHardening,
    /// A change child in flight, this process's or a predecessor's.
    pub(crate) in_flight: Option<InFlightFacts>,
    /// Anything under way (the alert, the re-read, the child, the re-probe) or the
    /// predecessor's records unread: no button is live.
    pub(crate) busy: bool,
    /// The last recorded mode, when aterm first saw it, and who set it ("turned on from aterm
    /// on `<date>`" / "set outside aterm").
    pub(crate) recorded: Option<ObservedMarker>,
    /// Drift: allowed since this time, off now (in this process).
    pub(crate) drift_since: Option<u64>,
    /// What the last change in this process came to.
    pub(crate) last_outcome: Option<Outcome>,
    /// Why the last confirmed change started nothing.
    pub(crate) refusal: Option<Refusal>,
}

impl Default for DebuggerFacts {
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
    /// Whether the button for `change` is drawn live.
    pub(crate) fn offers(&self, change: DebuggerChange) -> bool {
        self.enabled
            && !self.busy
            && self.probe.mode == change.precondition()
            && (change == DebuggerChange::Off || self.probe.member != DebuggerMember::NotMember)
    }

    /// Whether the row and the alert must add that this aterm is not hardened, so any program
    /// the owner runs could take control of it.
    pub(crate) fn not_hardened(&self) -> bool {
        self.self_hardened != SelfHardening::Hardened
    }
}

#[cfg(test)]
#[path = "debugger_access_tests.rs"]
mod tests;

/// Tier-1 `debugger_access_conforms_to_the_model` (design §5.4): drives the production
/// composition.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
#[path = "debugger_access_conformance_tests.rs"]
pub(crate) mod conformance;

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
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn debugger_access_change_waivers() {}
