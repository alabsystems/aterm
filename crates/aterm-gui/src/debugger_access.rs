// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owner debugger-access change.
//!
//! A press on Settings ▸ Security raises aterm's `confirm_owner` alert. System State's
//! debugger step starts the same change once per Mac on the window's own admission of a
//! setup plan with its Permissions group (`docs/DESIGN-system-state-2026-10-21.md` §6),
//! with no alert of aterm's: macOS's administrator dialog is the one gesture left. A
//! control client's gesture on System State is never a person's press
//! (`App::take_system_state_press`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    DevtoolsChangeRun, DevtoolsChild, ProbeGate, ProcessBirth, SelfHardening,
};
use aterm_types::MutexExt as _;
use aterm_types::macos_setup::AccessOutcome;

/// How often the park re-reads the shared result slot while a change is under way.
pub(crate) const RESULT_RECHECK: Duration = Duration::from_secs(5);

/// The marker beside `aterm.toml` naming a change child in flight.
pub(crate) const CHANGE_MARKER: &str = "privacy-debugger-change";

/// The marker beside `aterm.toml` holding the last observed mode, when aterm first saw it, and who set it (design §3.5).
pub(crate) const OBSERVED_MARKER: &str = "privacy-debugger-observed";

/// The Mac-wide change lock beside `aterm.toml`.
pub(crate) const CHANGE_LOCK: &str = "privacy-debugger-change.lock";

/// Both markers hold one short line; anything bigger is not a record this build wrote and reads as nothing.
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

/// The held change lock, released by `LOCK_UN` when this drops ([`HeldAdvisoryLock`](crate::native_document_host::HeldAdvisoryLock)), or by the kernel when its process dies.
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

/// How long [`take_change_lock`] waits out a busy lock.
pub(crate) const CHANGE_LOCK_PATIENCE: Duration = Duration::from_secs(1);

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

/// This test thread's change-lock patience until the guard drops.
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

/// Take the change lock beside `config_path`, waiting out a busy lock for up to `patience` ([`acquire_advisory_lock_within`](crate::native_document_host::acquire_advisory_lock_within)).
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

/// Write the observed record, then, under the change lock, remove an ended change's marker if it still names `release`'s child.
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

/// [`settle_markers`] off the UI thread (two tiny files, still disk I/O, and a lock it may wait a second for).
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
    /// A change marker naming a child that is gone, whose change was never judged (aterm quit before the re-probe, or a peer sits between taking its result and recording its re-probe).
    pub(crate) stale: Option<ChangeMarker>,
    /// The last observed mode.
    pub(crate) observed: Option<ObservedMarker>,
    /// No change marker, but the change lock was held past its patience.
    pub(crate) unsettled: bool,
}

/// Read both markers, off the UI thread, judging a change marker's child by pid and birth (`alive`), since a pid may be reused.
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

/// Why a press, or the host's start of a confirmed one, did nothing — in the owner's words, for "Nothing was changed: {why}.".
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

    /// The worker's re-read did not show the precondition: nothing was spawned, and what it read is the flow's observation now.
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

    /// Another aterm holds the change lock: nothing was read or spawned for the change being re-checked.
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

    /// Whether `job`'s worker has ended with something the park has not taken yet (the model's phase 5 when a child ran).
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

/// One confirmed change: the direction, its job number and the yes (the owner's in aterm's alert, or the window's own admission).
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

/// Everything the worker needs, handed through `ConsentGestures::run_devtools` (a plain function pointer, so the job carries its captures).
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
    /// The job for one confirmed change; `ticket` carries the direction, the job number and the owner's yes.
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

/// The live arm: one detached `consent-devtools` worker whose handle is dropped; while it lives it counts as an up macOS dialog ([`aterm_types::macos_setup::MacosDialog`]).
pub(crate) fn spawn_devtools_worker(job: DevtoolsJob) -> DevtoolsSpawn {
    // Up from before this returns — so the UI thread's next look at
    // `macos_dialog_up` already counts it, before the worker's first statement
    // — until the worker is gone, however it ends (a panic drops it too). A
    // thread that cannot start drops it with the closure.
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

/// The worker's body, with the change runner injected (live: `aterm_containment::consent::run_devtools_change`; the Tier-1 bind uses `devtools_change_with_for_test` with a scripted read and a stand-in child).
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
    let lock = match config_path.map(|path| take_change_lock(path, change_lock_patience())) {
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

/// The `LogOnly` records of design §3.8, as a fact; the words are `message_reporters::debugger_access_record`'s.
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

    /// Whether the watch judges now; `false` until the marker is read, and again while a mode waits for a fresh read of it.
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

/// The `App`-owned lane: the reducer, the shared result slot, the records reader's answer and the drift watch.
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
    /// A windowed instance reads liveness by pid and birth; a headless one never has a change child, and asks nothing.
    pub(crate) fn new(headless: bool) -> Self {
        let mut lane = Self::with_liveness(if headless {
            never_alive
        } else {
            DevtoolsChild::is_running
        });
        lane.windowed = !headless;
        lane
    }

    /// A windowed lane with an injected liveness check (the successor's pid-and-birth read); tests pass a stand-in.
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

    /// Settle the records without a reader: no marker location (headless, tests), or its thread could not start.
    pub(crate) fn settle_records(&mut self, read: RecordsRead) {
        *self.ask_records().lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
    }

    /// Record why the last confirmed change started nothing.
    pub(crate) const fn note_refusal(&mut self, refusal: Option<Refusal>) {
        self.refusal = refusal;
    }

    /// Whether the park must keep re-reading: a change under way, or the records asked for and not yet read.
    pub(crate) const fn needs_poll(&self) -> bool {
        (self.records_asked && !self.flow.records_read())
            || matches!(
                self.flow.phase(),
                Phase::Rechecking(_) | Phase::InFlight { .. } | Phase::Exited(_)
            )
    }

    /// Whether the park calls [`Self::poll`]: [`Self::needs_poll`] (the model's `polling`), or a records read out or wanted.
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

    /// Whether the shared records have been read (a press's first gate).
    pub(crate) const fn records_read(&self) -> bool {
        self.flow.records_read()
    }

    /// Whether a change is under way.
    pub(crate) fn under_way(&self) -> bool {
        self.flow.phase() != Phase::Idle
    }

    /// Why a press for `change` would be refused now ([`Self::begin`]'s first gate), or `None`.
    pub(crate) fn press_refusal(&self, change: DebuggerChange) -> Option<Refusal> {
        self.flow.press_refusal(change)
    }

    /// Whether debugger access reading off is the person's choice, from what this lane holds on record ([`off_is_chosen`]).
    pub(crate) fn off_is_chosen(&self) -> bool {
        off_is_chosen(self.drift.drift_since().is_some(), self.drift.recorded())
    }

    /// The owner's press and their answer (aterm's alert on Settings ▸ Security, or the window's own admission spent by [`Self::start_from_plan`] or a `Turn On…` press), and on a yes only, a new job armed in the slot.
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

    /// One park re-read (design §3.4 steps 4 and 6): the worker's slot, then the records, then a predecessor's child's liveness.
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

/// The record beside `aterm.toml` of the Macs on which aterm has started the debugger step by itself, or found the question answered: one `<host-uuid> <secs>` line per Mac.
pub(crate) const ASKED_RECORD: &str = "privacy-debugger-asked";

/// The Macs the asked record keeps.
pub(crate) const MAX_ASKED_LINES: usize = 8;

const MAX_HOST_BYTES: usize = 64;

const MAX_ASKED_BYTES: usize = MAX_ASKED_LINES * (MAX_HOST_BYTES + 1 + 20 + 1);

fn host_key_ok(host: &str) -> bool {
    (1..=MAX_HOST_BYTES).contains(&host.len())
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

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

fn record_path(config_path: &Path, record: &str) -> std::io::Result<PathBuf> {
    marker_path(config_path, record).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record in",
        )
    })
}

fn read_asked_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => crate::config_marker::read_marker(path, MAX_ASKED_BYTES)
            .map(Some)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the record is not a bounded regular UTF-8 record",
                )
            }),
    }
}

/// Whether the asked record names `host`.
pub(crate) fn read_asked(config_path: &Path, host: &str) -> bool {
    read_host_record(config_path, ASKED_RECORD, host)
}

/// Whether the once-per-Mac record `record` beside `config_path` names `host`, read as [`read_asked`] reads [`ASKED_RECORD`].
pub(crate) fn read_host_record(config_path: &Path, record: &str, host: &str) -> bool {
    if !host_key_ok(host) {
        return true;
    }
    match record_path(config_path, record).and_then(|path| read_asked_text(&path)) {
        Ok(None) => false,
        Ok(Some(text)) => {
            parse_asked(&text).is_none_or(|lines| lines.iter().any(|&(named, _)| named == host))
        }
        Err(_) => true,
    }
}

/// Write `host` into the asked record, AHEAD of the change it stands for.
pub(crate) fn note_asked(config_path: &Path, host: &str, now: u64) -> std::io::Result<()> {
    note_host_record(config_path, ASKED_RECORD, host, now)
}

/// Write `host` into the once-per-Mac record `record` beside `config_path`, as [`note_asked`] writes [`ASKED_RECORD`].
pub(crate) fn note_host_record(
    config_path: &Path,
    record: &str,
    host: &str,
    now: u64,
) -> std::io::Result<()> {
    use std::fmt::Write as _;
    if !host_key_ok(host) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a host key the record can hold",
        ));
    }
    let path = record_path(config_path, record)?;
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    let text = read_asked_text(&path)?;
    let mut lines = match text.as_deref() {
        None => Vec::new(),
        Some(text) => parse_asked(text).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the record is malformed and is left as found",
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

/// Whether the once-per-Mac record `record` beside `config_path` names `host`, read the other way from [`read_host_record`].
pub(crate) fn host_record_lists(
    config_path: &Path,
    record: &str,
    host: &str,
) -> std::io::Result<bool> {
    if !host_key_ok(host) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a host key the record can hold",
        ));
    }
    let Some(text) = read_asked_text(&record_path(config_path, record)?)? else {
        return Ok(false);
    };
    parse_asked(&text)
        .map(|lines| lines.iter().any(|&(named, _)| named == host))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "the record is malformed")
        })
}

/// Take `host` out of the record `record` beside `config_path`, written as [`note_host_record`] writes it.
pub(crate) fn forget_host_record(
    config_path: &Path,
    record: &str,
    host: &str,
) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let path = record_path(config_path, record)?;
    let _serial = DEBUGGER_MARKS.lock().unwrap_or_else(|p| p.into_inner());
    let Some(text) = read_asked_text(&path)? else {
        return Ok(());
    };
    let lines = parse_asked(&text).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the record is malformed and is left as found",
        )
    })?;
    if !lines.iter().any(|&(named, _)| named == host) {
        return Ok(());
    }
    let mut kept = String::new();
    for (named, secs) in lines.iter().filter(|&&(named, _)| named != host) {
        let _ = writeln!(kept, "{named} {secs}");
    }
    if kept.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    crate::config_marker::write_marker(
        &path,
        &kept,
        MAX_ASKED_BYTES,
        crate::durable::Flush::Durable,
    )
}

/// What the disk half of the debugger step's start found, and did ([`StartDisk::prepare`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartPrep {
    Recorded,
    Change { read_records: bool },
    NoLock,
    Asked,
    NotRecorded,
}

/// What the disk half of the debugger step's start needs from the lane, copied so it can run off the event loop.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartDisk {
    followed: [Option<DevtoolsChild>; 4],
    alive: fn(&DevtoolsChild) -> bool,
}

impl StartDisk {
    /// THE DISK HALF OF THE DEBUGGER STEP'S START: take the change lock, judge the marker, read the record and write this Mac into it durably, ahead of the change.
    pub(crate) fn prepare(self, config_path: &Path, host: &str, now: u64) -> StartPrep {
        // A first launch may have no config directory yet. The record writer
        // already creates it; the lock must be able to precede that write.
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let claim = match take_change_lock(config_path, Duration::ZERO) {
            LockTry::Held(claim) => claim,
            LockTry::Busy => return StartPrep::Change { read_records: true },
            LockTry::Unavailable(error) => {
                aterm_log::warn!("debugger step: the change lock cannot be taken: {error}");
                return StartPrep::NoLock;
            }
        };
        let marker = read_debugger_marker(config_path, CHANGE_MARKER)
            .and_then(|text| ChangeMarker::parse(&text));
        if let Some(marker) = marker {
            let followed = self.followed.contains(&Some(marker.child));
            if !followed || (self.alive)(&marker.child) {
                return StartPrep::Change {
                    read_records: !followed,
                };
            }
        }
        if read_asked(config_path, host) {
            return StartPrep::Asked;
        }
        if let Err(error) = note_asked(config_path, host, now) {
            aterm_log::warn!("debugger step: this Mac could not be recorded: {error}");
            return StartPrep::NotRecorded;
        }
        drop(claim);
        StartPrep::Recorded
    }
}

/// This Mac in the asked record, as the debugger step reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostAsked {
    Cold,
    Unnamed,
    Asked,
    NotAsked,
}

/// What System State's debugger step is judged on (`docs/DESIGN-system-state-2026-10-21.md` §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DebuggerStepFacts {
    /// A windowed instance.
    pub(crate) windowed: bool,
    pub(crate) macos: bool,
    /// `[privacy] enabled && check`.
    pub(crate) privacy: bool,
    /// `[privacy] notice`: off pauses the step, and no key starts it.
    pub(crate) notice: bool,
    /// A config folder to keep the record in: without one nothing would remember the step, and every launch would start it again.
    pub(crate) config: bool,
    /// THE STANDING YES in hand (`App::admitted_access`): the window's own
    /// admission of a setup plan with its Permissions group — its automatic
    /// answer to the startup offer, or a person's press on the page — and that
    /// plan still the one in force. Without it the step parks until one comes
    /// ([`StepVerdict::Park`]).
    pub(crate) admitted: bool,
    pub(crate) host: HostAsked,
    /// The shared debugger records (a predecessor's change, the observed mode) are read.
    pub(crate) records_read: bool,
    /// A change under way ([`DebuggerChangeLane::under_way`]), this process's or an inherited one.
    pub(crate) under_way: bool,
    /// The owner answered Settings ▸ Security's debugger-access alert in this process, yes or no (`OwnerQuiet::debugger_answered`).
    pub(crate) answered: bool,
    /// The cached probe, `None` until the look reads it.
    pub(crate) probe: Option<DebuggerProbe>,
    /// The probe was read within [`ASK_PROBE_FRESH`] (or by an instance's syscall-free arm, which answers at every read).
    pub(crate) probe_fresh: bool,
    /// The observed record, as the lane holds it.
    pub(crate) recorded: Option<ObservedMarker>,
    /// This process saw debugger access go from allowed to off.
    pub(crate) drift: bool,
    /// aterm's own hardening, read with the probe.
    pub(crate) self_hardened: SelfHardening,
    /// This process's Full Disk Access launch decision is made (not `FdaLaunch::Undecided`).
    pub(crate) fda_decided: bool,
    /// This process's Developer Tools launch reading has completed, whatever it read (a refusal included).
    pub(crate) devtools_decided: bool,
    /// A Full Disk Access or Developer Tools hand-over waits, or aterm's helper panel is up.
    pub(crate) page: bool,
    /// The folder warm-up is walking (a macOS folder dialog may be up).
    pub(crate) warmup: bool,
    /// System State is the visible, active view of a focused window with no overlay (`App::system_state_frontmost`).
    pub(crate) frontmost: bool,
    /// A macOS dialog or page this process put up is up ([`aterm_types::macos_setup::macos_dialog_up`]).
    pub(crate) dialog_up: bool,
    /// The owner declined a Security gesture recently (`OwnerQuiet`).
    pub(crate) owner_quiet: bool,
}

/// What the debugger step does now ([`admit_debugger_step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepVerdict {
    /// Name macOS's dialog on the page, then write the record and start the change.
    Start,
    /// No standing yes: nothing written, and no look until the window's own admission comes.
    Park,
    /// A `[privacy]` switch is off: a pause, never an answer; nothing written or recorded.
    Pause(StepPause),
    Wait(StepWait),
    /// Write the record and start nothing: the question is answered on this Mac.
    Settle(StepSettle),
    /// Nothing more this process, and no record of this Mac written.
    Drop(StepDrop),
}

/// Why the debugger step starts nothing this process and writes no record of
/// this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepDrop {
    Headless,
    NotMacos,
    NoConfig,
    NoHost,
    Asked,
    Unrecognized,
    Unread,
    NotMember,
    NotHardened,
}

impl StepDrop {
    /// What the drop says of the row for the revision whose admission is in
    /// hand, where it settles the row: this Mac's once-per-Mac step is over
    /// (`Asked`: an earlier revision's, perhaps cancelled in macOS's dialog).
    /// `None` says nothing: a build that is not hardened and an account in
    /// neither group read *not needed* on the row itself, a rule the probe
    /// cannot read or Apple's tool does not recognise is read again, and the
    /// rest have no window or record to settle it in. A `[privacy]` switch off
    /// is no drop at all but a pause ([`StepVerdict::Pause`]), which records
    /// nothing: the row reads as the person's choice from the published
    /// opt-out while the switch is off, and is still to do once it is on.
    pub(crate) const fn outcome(self) -> Option<AccessOutcome> {
        match self {
            Self::Asked => Some(AccessOutcome::Declined),
            Self::Headless
            | Self::NotMacos
            | Self::NoConfig
            | Self::NoHost
            | Self::Unrecognized
            | Self::Unread
            | Self::NotMember
            | Self::NotHardened => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::NotMacos => "not_macos",
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

/// Which `[privacy]` switch pauses the debugger step ([`StepVerdict::Pause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepPause {
    /// `[privacy] enabled` or `check` is off.
    PrivacyOff,
    /// `[privacy] notice` is off.
    NoticeOff,
}

impl StepPause {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PrivacyOff => "privacy_off",
            Self::NoticeOff => "notice_off",
        }
    }
}

/// Why the debugger step looks again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepWait {
    HostCold,
    RecordsUnread,
    ProbePending,
    BehindFullDiskAccess,
    BehindDeveloperTools,
    BehindPage,
    BehindWarmup,
    DialogUp,
    NotFrontmost,
    OwnerQuiet,
    ProbeStale,
    ProbeUnread,
    Announced,
    Recording,
}

/// How old a probe the debugger step starts the change on may be.
pub(crate) const ASK_PROBE_FRESH: Duration = Duration::from_secs(5);

impl StepWait {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::HostCold => "host_cold",
            Self::RecordsUnread => "records_unread",
            Self::ProbePending => "probe_pending",
            Self::BehindFullDiskAccess => "behind_full_disk_access",
            Self::BehindDeveloperTools => "behind_developer_tools",
            Self::BehindPage => "behind_page",
            Self::BehindWarmup => "behind_warmup",
            Self::DialogUp => "dialog_up",
            Self::NotFrontmost => "not_frontmost",
            Self::OwnerQuiet => "owner_quiet",
            Self::ProbeStale => "probe_stale",
            Self::ProbeUnread => "probe_unread",
            Self::Announced => "announced",
            Self::Recording => "recording",
        }
    }

    /// How long until the next look.
    pub(crate) const fn look_again(self) -> Duration {
        match self {
            Self::HostCold
            | Self::ProbePending
            | Self::ProbeStale
            | Self::Announced
            | Self::Recording
            | Self::NotFrontmost
            | Self::DialogUp => crate::devtools_helper::PANEL_TICK,
            _ => crate::devtools_helper::RECHECK_EVERY,
        }
    }
}

/// Why the debugger step settles: the question is answered on this Mac, and the record says so with nothing started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepSettle {
    Underway,
    Answered,
    AlreadyOn,
    TurnedOff,
}

impl StepSettle {
    /// What the settle says of the row for the revision whose admission is in
    /// hand: a change under way was handed to macOS's dialog, the rule on is
    /// granted, and an answer on the Security page or an explicit opt-out is
    /// declined.
    pub(crate) const fn outcome(self) -> AccessOutcome {
        match self {
            Self::Underway => AccessOutcome::Handed,
            Self::AlreadyOn => AccessOutcome::Granted,
            Self::Answered | Self::TurnedOff => AccessOutcome::Declined,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Underway => "under_way",
            Self::Answered => "answered",
            Self::AlreadyOn => "already_on",
            Self::TurnedOff => "turned_off",
        }
    }
}

/// Whether debugger access reading off is the person's choice: this process saw it go off (`drift`), or the observed record says it was allowed or that aterm or something else turned it off. Pure, so every launch judges it the same way.
pub(crate) fn off_is_chosen(drift: bool, recorded: Option<ObservedMarker>) -> bool {
    drift
        || recorded
            .is_some_and(|was| was.mode == DebuggerMode::Enabled || was.origin != Origin::Unknown)
}

/// THE DEBUGGER STEP'S ADMISSION (`docs/DESIGN-system-state-2026-10-21.md` §6). Pure, and the order is the contract: the instance's gates, the standing yes, the `[privacy]` switches, the record, the reads, the question answered here, whether the change can help, the moment, and a fresh probe.
pub(crate) fn admit_debugger_step(facts: &DebuggerStepFacts) -> StepVerdict {
    let gates = [
        (facts.windowed, StepDrop::Headless),
        (facts.macos, StepDrop::NotMacos),
        (facts.config, StepDrop::NoConfig),
    ];
    if let Some(&(_, why)) = gates.iter().find(|(open, _)| !*open) {
        return StepVerdict::Drop(why);
    }
    if !facts.admitted {
        return StepVerdict::Park;
    }
    // The `[privacy]` switches after the admission, before anything is read:
    // off, the step pauses with nothing written or recorded, and goes on for
    // the revision in force once they are on again.
    let switches = [
        (facts.privacy, StepPause::PrivacyOff),
        (facts.notice, StepPause::NoticeOff),
    ];
    if let Some(&(_, why)) = switches.iter().find(|(open, _)| !*open) {
        return StepVerdict::Pause(why);
    }
    match facts.host {
        HostAsked::Cold => return StepVerdict::Wait(StepWait::HostCold),
        HostAsked::Unnamed => return StepVerdict::Drop(StepDrop::NoHost),
        HostAsked::Asked => return StepVerdict::Drop(StepDrop::Asked),
        HostAsked::NotAsked => {}
    }
    if !facts.records_read {
        return StepVerdict::Wait(StepWait::RecordsUnread);
    }
    let Some(probe) = facts
        .probe
        .filter(|probe| probe.label != DebuggerProbeLabel::Pending)
    else {
        return StepVerdict::Wait(StepWait::ProbePending);
    };
    if facts.under_way {
        return StepVerdict::Settle(StepSettle::Underway);
    }
    if facts.answered {
        return StepVerdict::Settle(StepSettle::Answered);
    }
    match probe.mode {
        DebuggerMode::Enabled => return StepVerdict::Settle(StepSettle::AlreadyOn),
        DebuggerMode::Disabled if off_is_chosen(facts.drift, facts.recorded) => {
            return StepVerdict::Settle(StepSettle::TurnedOff);
        }
        DebuggerMode::Disabled => {}
        DebuggerMode::Unrecognized => return StepVerdict::Drop(StepDrop::Unrecognized),
        DebuggerMode::Unknown => return StepVerdict::Drop(StepDrop::Unread),
    }
    if !matches!(
        probe.member,
        DebuggerMember::Admin | DebuggerMember::Developer
    ) {
        return StepVerdict::Drop(StepDrop::NotMember);
    }
    if facts.self_hardened != SelfHardening::Hardened {
        return StepVerdict::Drop(StepDrop::NotHardened);
    }
    let waits = [
        (!facts.fda_decided, StepWait::BehindFullDiskAccess),
        (!facts.devtools_decided, StepWait::BehindDeveloperTools),
        (facts.page, StepWait::BehindPage),
        (facts.warmup, StepWait::BehindWarmup),
        (facts.dialog_up, StepWait::DialogUp),
        (!facts.frontmost, StepWait::NotFrontmost),
        (facts.owner_quiet, StepWait::OwnerQuiet),
        (!facts.probe_fresh, StepWait::ProbeStale),
    ];
    waits
        .iter()
        .find(|(holds, _)| *holds)
        .map_or(StepVerdict::Start, |&(_, why)| StepVerdict::Wait(why))
}

/// Why [`DebuggerChangeLane::start_from_plan`] gave no ticket. The admission
/// it was handed is spent either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartEnd {
    Refused(Refusal),
    NoLock,
    Asked,
    NotRecorded,
}

impl StartEnd {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Refused(_) => "refused",
            Self::NoLock => "no_lock",
            Self::Asked => "asked",
            Self::NotRecorded => "not_recorded",
        }
    }
}

impl DebuggerChangeLane {
    /// THE STANDING YES SPENT: for a Mac the start's disk half has just written into [`ASKED_RECORD`], take `admitted`, the window's own admission of a setup plan with its Permissions group, as the yes a press takes ([`Self::begin`] with `OwnerYes::from_admitted`). No alert: macOS's administrator dialog is the one gesture left. Only On.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DebuggerAccessChange",
            action = "PlanConsent",
            project = "aterm_gui::debugger_access::conformance::project"
        )
    )]
    pub(crate) fn start_from_plan(
        &mut self,
        prepared: StartPrep,
        now_wall: u64,
        admitted: crate::workstation_setup::AdmittedAccess,
    ) -> Result<ChangeTicket, StartEnd> {
        use crate::native_settings::{OwnerAnswer, OwnerYes};
        self.start_prepared(prepared, now_wall)?;
        // The press may have become one the lane refuses while the disk half
        // ran (a person's own press, say): the record stands, nothing starts.
        if let Some(refusal) = self.start_refusal() {
            return Err(StartEnd::Refused(refusal));
        }
        self.begin(
            DebuggerChange::On,
            OwnerAnswer::Yes(OwnerYes::from_admitted(admitted)),
        )
        .map_err(StartEnd::Refused)
    }

    /// [`Self::start_from_plan`] with its disk half run here, as the App runs the three steps apart.
    #[cfg(test)]
    pub(crate) fn start_now(
        &mut self,
        config_path: &Path,
        host: &str,
        admitted: crate::workstation_setup::AdmittedAccess,
    ) -> Result<ChangeTicket, StartEnd> {
        if let Some(refusal) = self.start_refusal() {
            return Err(StartEnd::Refused(refusal));
        }
        let prepared = self.start_disk().prepare(config_path, host, now_secs());
        self.start_from_plan(prepared, now_secs(), admitted)
    }

    /// Why the press [`Self::start_from_plan`] stands for would be refused now, taking nothing.
    pub(crate) fn start_refusal(&self) -> Option<Refusal> {
        self.flow.press_refusal(DebuggerChange::On)
    }

    /// What the disk half of [`Self::start_from_plan`] needs from this lane, copied, so it can run off the event loop ([`StartDisk::prepare`]).
    pub(crate) fn start_disk(&self) -> StartDisk {
        StartDisk {
            followed: self.followed(),
            alive: self.alive,
        }
    }

    pub(crate) fn start_prepared(
        &mut self,
        prepared: StartPrep,
        now_wall: u64,
    ) -> Result<(), StartEnd> {
        match prepared {
            StartPrep::Recorded => Ok(()),
            StartPrep::Change { read_records } => {
                if read_records && !self.records_out && self.records_again_at.is_none() {
                    self.records_again_at = Some(now_wall);
                }
                Err(StartEnd::Refused(Refusal::InFlight))
            }
            StartPrep::NoLock => Err(StartEnd::NoLock),
            StartPrep::Asked => Err(StartEnd::Asked),
            StartPrep::NotRecorded => Err(StartEnd::NotRecorded),
        }
    }

    /// THE DEBUGGER STEP SETTLED: write this Mac into [`ASKED_RECORD`] and nothing else.
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

/// The App's debugger step: queued on a windowed macOS instance until it starts, settles or drops, with the park's next look; parked while no standing yes is in hand.
#[derive(Debug)]
pub(crate) struct DebuggerStepQueue {
    queued: bool,
    /// A windowed macOS instance's step: only it is ever queued again.
    windowed: bool,
    /// Parked for want of a standing yes: not queued, and queued again by
    /// [`Self::unpark`] when the window's own admission comes.
    parked: bool,
    /// Paused while a `[privacy]` switch is off ([`StepVerdict::Pause`]): not
    /// queued, and queued again by [`Self::resume`] once every switch is on.
    paused: bool,
    /// The setup revision the step last came for: the window's own admission of
    /// it, or the one a person's `Turn On…` spent ([`Self::note_started`]).
    revision: Option<u64>,
    /// The page's headline names macOS's dialog ([`Self::announce`]).
    announced: Option<Announced>,
    /// The change System State started (this step, or a person's `Turn On…`
    /// on the page) and the setup revision it spent: its outcome is the access
    /// record's for that revision.
    started: Option<u64>,
    /// The revisions the window admitted while that change was under way
    /// ([`Self::join_started`]): the change in flight answers them too.
    joined: Vec<u64>,
    /// Where the access record goes (`aterm_types::dirs::state_dir()` for a
    /// launch, [`Self::recording_in`]); `None` writes none.
    access_dir: Option<PathBuf>,
    /// Appends the debugger row's outcomes off the UI thread, in order.
    access_writer: AccessWriter,
    next_look: Option<Instant>,
    host: fn() -> Option<&'static str>,
    host_read: Option<Arc<std::sync::atomic::AtomicBool>>,
    asked: Option<(&'static str, Arc<std::sync::OnceLock<bool>>)>,
    prepared: Option<(Arc<std::sync::OnceLock<StartPrep>>, DebuggerFacts)>,
    record_read: RecordRead,
    waiting: Option<StepWait>,
    refused_since: Option<Instant>,
    unread_since: Option<Instant>,
    unread_reads: u32,
}

/// THE ACCESS RECORD'S WRITER: the debugger row's outcomes for System State's Permissions stage (`aterm_types::macos_setup::access_record`), appended in order on a thread of its own, never in the UI thread's turn. A line whose write fails is lost with a warning, and the stage judges that row again.
#[derive(Debug, Default)]
struct AccessWriter {
    queue: Arc<Mutex<AccessQueue>>,
}

/// The lines [`AccessWriter`] has yet to write, and whether a drain runs.
#[derive(Debug, Default)]
struct AccessQueue {
    lines: std::collections::VecDeque<(PathBuf, u64, AccessOutcome)>,
    draining: bool,
}

impl AccessWriter {
    /// Queue one line; start the drain if none runs.
    fn append(&self, dir: PathBuf, revision: u64, outcome: AccessOutcome) {
        let start = {
            let mut queue = self.queue.lock_or_recover();
            queue.lines.push_back((dir, revision, outcome));
            !std::mem::replace(&mut queue.draining, true)
        };
        if !start {
            return;
        }
        let queue = Arc::clone(&self.queue);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-access-record",
            crate::qos::Role::Background,
            move || {
                loop {
                    // Taken and released per line: the UI thread never waits for a write.
                    let next = {
                        let mut queue = queue.lock_or_recover();
                        let next = queue.lines.pop_front();
                        if next.is_none() {
                            queue.draining = false;
                        }
                        next
                    };
                    let Some((dir, revision, outcome)) = next else {
                        return;
                    };
                    if let Err(error) = aterm_types::macos_setup::access_record::append(
                        &dir,
                        revision,
                        aterm_types::macos_setup::Access::Debugger,
                        outcome,
                    ) {
                        aterm_log::warn!(
                            "debugger access: the access record could not be written ({}): {error}",
                            outcome.word()
                        );
                    }
                }
            },
        );
        if let Err(error) = spawned {
            let mut queue = self.queue.lock_or_recover();
            let lost = queue.lines.len();
            queue.lines.clear();
            queue.draining = false;
            aterm_log::warn!("debugger access: {lost} access record line(s) not written: {error}");
        }
    }

    /// Wait until the queue is empty and no drain runs.
    #[cfg(test)]
    fn drained_for_test(&self) {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the access record's lines are written",
            || {
                let queue = self.queue.lock_or_recover();
                (queue.lines.is_empty() && !queue.draining).then_some(())
            },
        );
    }
}

/// The page's announcement of macOS's dialog: the window whose System State view names it, that window's presented-frame count then, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Announced {
    window: crate::WindowId,
    presented: u64,
    at: Instant,
}

/// Where System State stands for the debugger step at a look (the App's `debugger_step_moment`; a test's stand-in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StepMoment {
    /// The focused window whose visible, active view is System State with no overlay, and how many frames it has presented. `None`: System State is not frontmost.
    pub(crate) page: Option<(crate::WindowId, u64)>,
    /// A macOS dialog or page this process put up is up.
    pub(crate) dialog_up: bool,
}

/// How long the page names macOS's dialog before the change puts it up, at the least: long enough to read, and to outlast the page's status poll.
pub(crate) const ANNOUNCE_FOR: Duration = Duration::from_secs(2);

/// How long starts refused before anything was written (a change on disk, the lane busy) may keep the debugger step queued, from the first refusal.
pub(crate) const REFUSED_FOR: Duration = Duration::from_secs(5 * 60);

/// How many reads of the rule that read nothing a recorded debugger step meets before [`REFUSED_FOR`] from the first can end it ([`DebuggerStepQueue::unread`]).
pub(crate) const UNREAD_READS: u32 = 3;

/// Where [`DebuggerStepQueue`] does its disk work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordRead {
    Thread,
    #[cfg(test)]
    Inline,
}

impl DebuggerStepQueue {
    /// Queued when `queued` (a windowed macOS instance), with the live host key
    /// and no access record ([`Self::recording_in`] names its folder).
    pub(crate) fn new(queued: bool) -> Self {
        Self {
            queued,
            windowed: queued,
            parked: false,
            paused: false,
            revision: None,
            announced: None,
            started: None,
            joined: Vec::new(),
            access_dir: None,
            access_writer: AccessWriter::default(),
            next_look: None,
            host: aterm_containment::consent::host_uuid_if_warm,
            host_read: None,
            asked: None,
            prepared: None,
            record_read: RecordRead::Thread,
            waiting: None,
            refused_since: None,
            unread_since: None,
            unread_reads: 0,
        }
    }

    /// A queued step with a stand-in host key whose read has ended, its record read on the asking thread.
    #[cfg(test)]
    pub(crate) fn queued_for_test(host: fn() -> Option<&'static str>) -> Self {
        Self {
            host,
            host_read: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
            record_read: RecordRead::Inline,
            ..Self::new(true)
        }
    }

    /// A queued step with a stand-in host key whose read is out, ended once `done` is set, its record read on the asking thread.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn reading_host_for_test(
        host: fn() -> Option<&'static str>,
        done: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            host,
            host_read: Some(done),
            record_read: RecordRead::Inline,
            ..Self::new(true)
        }
    }

    /// A queued step with a stand-in host key whose read has ended, its record read off the asking thread as a launch's is.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn reading_record_for_test(host: fn() -> Option<&'static str>) -> Self {
        Self {
            record_read: RecordRead::Thread,
            ..Self::queued_for_test(host)
        }
    }

    /// The access record goes to `dir` (a launch's: `aterm_types::dirs::state_dir()`).
    pub(crate) fn recording_in(mut self, dir: Option<PathBuf>) -> Self {
        self.access_dir = dir;
        self
    }

    /// Whether the step is still to come this process (and not parked).
    pub(crate) const fn queued(&self) -> bool {
        self.queued
    }

    /// No standing yes: stop looking, with nothing written, until
    /// [`Self::unpark`]. The folder warm-up, which waits while the step is
    /// queued, does not wait on a parked one.
    pub(crate) fn park(&mut self) {
        self.parked = true;
        self.announced = None;
        self.end();
    }

    /// A `[privacy]` switch is off ([`StepVerdict::Pause`]): stop looking, with
    /// nothing written or recorded, until [`Self::resume`]. A start's disk half
    /// answered and kept ([`Self::keep_prepared`]) stays kept, for the look
    /// after the resume to spend. A switch off is a pause, never an answer.
    pub(crate) fn pause(&mut self) {
        self.paused = true;
        self.end();
    }

    /// Every `[privacy]` switch is on again: a paused step looks again at the
    /// next park, for the revision then in force, as if it had never paused.
    /// Nothing for a step that is not paused.
    pub(crate) fn resume(&mut self) {
        if std::mem::take(&mut self.paused) {
            self.queued = true;
            self.next_look = None;
            self.waiting = None;
        }
    }

    /// Whether the step is paused for a `[privacy]` switch that is off.
    pub(crate) const fn paused(&self) -> bool {
        self.paused
    }

    /// The window's own admission of setup revision `revision` came: a parked
    /// step looks again at the next park. So does one over for an earlier
    /// revision, as a later launch's would (design §6: a new approval asks
    /// again), its record read again first: the step's own start and a
    /// person's `Turn On…` write this Mac into it, so on a Mac that answered,
    /// the look settles the new revision's row ([`StepDrop::Asked`]) with no
    /// second dialog. Nothing for a step still queued, an admission of the
    /// revision it came for (a change to another group keeps it, and its
    /// outcome stands), or an instance that never queues it.
    pub(crate) fn unpark(&mut self, revision: u64) {
        let again = self
            .revision
            .replace(revision)
            .is_some_and(|before| before != revision)
            && self.windowed
            && !self.queued;
        if again {
            self.asked = None;
            self.waiting = None;
            self.refused_since = None;
            self.unread_since = None;
            self.unread_reads = 0;
        }
        if std::mem::take(&mut self.parked) || again {
            self.queued = true;
            self.next_look = None;
        }
    }

    /// Whether the step is parked for want of a standing yes.
    #[cfg(test)]
    pub(crate) const fn parked(&self) -> bool {
        self.parked
    }

    /// A person's `Turn On…` answered the once-per-Mac question here
    /// (`App::turn_on_debugger_from_system_state`): the step is over for this
    /// revision, parked, paused or queued, and neither an admission of it nor
    /// the `[privacy]` switches coming back on queues it again; one
    /// of a later revision finds this Mac in the record ([`Self::unpark`]), so
    /// a Cancel in macOS's dialog is never followed by the step's own.
    pub(crate) fn answered_by_press(&mut self) {
        self.parked = false;
        self.paused = false;
        // A start's disk half answered for the step is let go with it: the
        // press records this Mac itself.
        self.prepared = None;
        self.end();
    }

    /// The page names macOS's dialog in `window`, which has presented
    /// `presented` frames, from `now`: `true` when that is news (a first
    /// announcement, or one moved to another window), which restarts the
    /// count.
    pub(crate) fn announce(
        &mut self,
        window: crate::WindowId,
        presented: u64,
        now: Instant,
    ) -> bool {
        if self
            .announced
            .is_some_and(|announced| announced.window == window)
        {
            return false;
        }
        self.announced = Some(Announced {
            window,
            presented,
            at: now,
        });
        true
    }

    /// Whether the announcement has been on screen: `window` is the one that
    /// names the dialog, it has presented a frame since, and [`ANNOUNCE_FOR`]
    /// has passed by `now`.
    pub(crate) fn shown(&self, window: crate::WindowId, presented: u64, now: Instant) -> bool {
        self.announced.is_some_and(|announced| {
            announced.window == window
                && presented != announced.presented
                && now.saturating_duration_since(announced.at) >= ANNOUNCE_FOR
        })
    }

    /// The page no longer names the dialog (the step waits, or is over).
    pub(crate) fn withdraw(&mut self) {
        self.announced = None;
    }

    /// Whether the page names macOS's dialog now.
    pub(crate) const fn announced(&self) -> bool {
        self.announced.is_some()
    }

    /// System State started a change spending setup revision `revision`: its
    /// outcome is that revision's access record ([`Self::finish`]), and the
    /// step has come for that revision ([`Self::unpark`]).
    pub(crate) fn note_started(&mut self, revision: u64) {
        self.started = Some(revision);
        self.revision = Some(revision);
    }

    /// The setup revision of the change System State started, while it is
    /// under way.
    pub(crate) const fn started(&self) -> Option<u64> {
        self.started
    }

    /// The window's own admission of setup revision `revision` came while the
    /// change System State started is under way, macOS's dialog perhaps still
    /// up: that change answers it, so the step is not queued again for it (a
    /// look would find this Mac in the record and decline the revision before
    /// anyone answered the dialog). The revision is handed now, as the one that
    /// started the change was, and the change's outcome is its outcome too
    /// ([`Self::finish`]). The admission of a revision the step came for
    /// already changes nothing.
    pub(crate) fn join_started(&mut self, revision: u64) {
        if self.revision.replace(revision) == Some(revision)
            || self.started == Some(revision)
            || self.joined.contains(&revision)
        {
            return;
        }
        self.joined.push(revision);
        self.record(revision, AccessOutcome::Handed);
    }

    /// The change System State started has ended: its revision and every one
    /// admitted while it ran ([`Self::join_started`]), in order, once.
    pub(crate) fn finish(&mut self) -> Vec<u64> {
        self.started
            .take()
            .into_iter()
            .chain(std::mem::take(&mut self.joined))
            .collect()
    }

    /// Append the debugger row's `outcome` for setup revision `revision` to
    /// the access record (`aterm_types::macos_setup::access_record`), which
    /// System State's Permissions stage reads so a row waiting on a person is
    /// never retried. Off the UI thread, after every line asked for before it
    /// ([`AccessWriter`]); nothing where no folder is named.
    pub(crate) fn record(&self, revision: u64, outcome: AccessOutcome) {
        if let Some(dir) = &self.access_dir {
            self.access_writer.append(dir.clone(), revision, outcome);
        }
    }

    /// Wait until every line [`Self::record`] took is written, or its write
    /// failed: a test's read of the record. A one-minute hang detector.
    #[cfg(test)]
    pub(crate) fn record_written_for_test(&self) {
        self.access_writer.drained_for_test();
    }

    /// Whether a look is due at `now`.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.queued
            && (self.next_look.is_none_or(|at| now >= at)
                || (self.waiting == Some(StepWait::Recording)
                    && self
                        .prepared
                        .as_ref()
                        .is_some_and(|(slot, _)| slot.get().is_some())))
    }

    /// Look again at `at`.
    pub(crate) const fn look_later(&mut self, at: Instant) {
        self.next_look = Some(at);
    }

    /// The look waits on `why`: look again at `at`.
    pub(crate) fn wait(&mut self, why: StepWait, at: Instant) -> bool {
        self.look_later(at);
        self.waiting.replace(why) != Some(why)
    }

    /// The start at `now` was refused before anything was written or started ([`StartEnd::Refused`]).
    pub(crate) fn refused(&mut self, now: Instant) -> bool {
        let since = *self.refused_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= REFUSED_FOR {
            self.end();
            return false;
        }
        self.look_later(now + crate::devtools_helper::RECHECK_EVERY);
        true
    }

    /// The spend's fresh reading did not read the rule after the disk half recorded this Mac (a `-status` that timed out, did not start or answered what the classifier does not claim: [`DebuggerMode::Unknown`] with an age). `true`: it is read again; `false`, the step over and the record standing, once [`UNREAD_READS`] such reads have lasted [`REFUSED_FOR`] from the first. The span is never restarted.
    pub(crate) fn unread(&mut self, now: Instant) -> bool {
        let since = *self.unread_since.get_or_insert(now);
        self.unread_reads = self.unread_reads.saturating_add(1);
        if self.unread_reads >= UNREAD_READS && now.saturating_duration_since(since) >= REFUSED_FOR
        {
            self.end();
            return false;
        }
        true
    }

    /// The park's deadline: the next look, while the step is queued.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.next_look.filter(|_| self.queued)
    }

    /// The host key, once warm.
    pub(crate) fn warm_host(&self) -> Option<&'static str> {
        (self.host)()
    }

    /// A cold host key's read: `Some(done)` the first time.
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

    /// Whether the record names `host` ([`read_asked`]).
    pub(crate) fn asked(&self, host: &'static str) -> Option<bool> {
        self.asked
            .as_ref()
            .filter(|(read_for, _)| *read_for == host)
            .and_then(|(_, answer)| answer.get().copied())
    }

    /// The record's read for `host`, the first time it is asked for that host: `Some((slot, where))`.
    pub(crate) fn start_record_read(
        &mut self,
        host: &'static str,
    ) -> Option<(Arc<std::sync::OnceLock<bool>>, RecordRead)> {
        if self
            .asked
            .as_ref()
            .is_some_and(|(read_for, _)| *read_for == host)
        {
            return None;
        }
        let slot = Arc::new(std::sync::OnceLock::new());
        self.asked = Some((host, Arc::clone(&slot)));
        Some((slot, self.record_read))
    }

    /// Where this queue does its disk work: the record's read, the start's disk half and a settle's record.
    pub(crate) const fn record_read(&self) -> RecordRead {
        self.record_read
    }

    /// The start's disk half, once per start, for a change that will start on `read`: `Some(slot)` when none is out or untaken.
    pub(crate) fn start_prepare(
        &mut self,
        read: DebuggerFacts,
    ) -> Option<Arc<std::sync::OnceLock<StartPrep>>> {
        if self.prepared.is_some() {
            return None;
        }
        let slot = Arc::new(std::sync::OnceLock::new());
        self.prepared = Some((Arc::clone(&slot), read));
        Some(slot)
    }

    /// Whether the start's disk half is out: started and not ended.
    pub(crate) fn preparing(&self) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|(slot, _)| slot.get().is_none())
    }

    /// The disk half's answer once it has ended, with the reading its change starts on, taken: the next start starts another.
    pub(crate) fn take_prepared(&mut self) -> Option<(StartPrep, DebuggerFacts)> {
        let (slot, read) = self.prepared.as_ref()?;
        let answer = (*slot.get()?, *read);
        self.prepared = None;
        Some(answer)
    }

    /// The disk half's answer, taken by a look that could not spend it yet, put back.
    pub(crate) fn keep_prepared(&mut self, prepared: StartPrep, read: DebuggerFacts) {
        let slot = Arc::new(std::sync::OnceLock::new());
        let _ = slot.set(prepared);
        self.prepared = Some((slot, read));
        // No longer the disk half's wait: a kept answer is due at its time
        // ([`Self::due`]), never at every wake.
        if self.waiting == Some(StepWait::Recording) {
            self.waiting = None;
        }
    }

    /// Whether a start's disk half is out, or its answer waits to be spent.
    pub(crate) const fn starting(&self) -> bool {
        self.prepared.is_some()
    }

    /// The step is over for this process (or parked, [`Self::park`]).
    pub(crate) const fn end(&mut self) {
        self.queued = false;
        self.next_look = None;
        self.announced = None;
    }
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
    /// Anything under way (the alert, the re-read, the child, the re-probe) or the predecessor's records unread: no button is live.
    pub(crate) busy: bool,
    /// The last recorded mode, when aterm first saw it, and who set it: "turned on from aterm on `<date>`" / "set outside aterm".
    pub(crate) recorded: Option<ObservedMarker>,
    /// Drift: allowed since this time, off now (in this process).
    pub(crate) drift_since: Option<u64>,
    /// What the last change in this process came to.
    pub(crate) last_outcome: Option<Outcome>,
    /// Why the last confirmed change (a press, or System State's standing yes) started nothing.
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

    /// Whether the row and the alert must add that this aterm is not hardened, so any program the owner runs could take control of it.
    pub(crate) fn not_hardened(&self) -> bool {
        self.self_hardened != SelfHardening::Hardened
    }
}

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

    #[cfg(unix)]
    #[test]
    fn a_busy_change_lock_is_waited_out_only_for_its_patience() {
        let dir = scratch("patience");
        let config = dir.join("aterm.toml");
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("a fresh lock is free");
        };
        assert!(matches!(
            take_change_lock(&config, std::time::Duration::from_millis(1)),
            LockTry::Busy
        ));
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
        assert_eq!(
            recording.take(),
            [Flush::Durable, Flush::Durable],
            "the change marker, then its folder's entry"
        );
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
        let _patience = patience_for_test(Duration::ZERO);
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
        let dead = read_records(&config, |_| false);
        assert_eq!(dead.inherited, None);
        assert_eq!(dead.stale, Some(marker), "its direction and child are kept");
        assert_eq!(dead.observed, Some(observed));
        assert!(
            dir.join(CHANGE_MARKER).exists(),
            "a stale marker waits for its change to be recorded"
        );
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

    #[cfg(unix)]
    #[test]
    fn a_change_lock_held_before_its_marker_is_written_is_read_again() {
        let _patience = patience_for_test(Duration::ZERO);
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

    #[cfg(unix)]
    #[test]
    fn the_worker_holds_the_lock_publishes_the_child_then_the_result_and_leaves_the_marker() {
        let _patience = patience_for_test(Duration::ZERO);
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

        slot.arm(4);
        let mut over_live = job(DebuggerChange::On, 4, &slot);
        over_live.config_path = Some(config.clone());
        over_live.alive = |c| c.pid == 77;
        run_devtools_job(over_live, |_, _, _| {
            panic!("nothing runs over a live change")
        });
        assert_eq!(slot.take(4).refused, Some(WorkerRefusal::PeerChanging));

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

        slot.arm(6);
        slot.publish_result(5, DevtoolsChangeRun::SpawnFailed);
        slot.publish_refused(5, WorkerRefusal::NoLock);
        assert_eq!(slot.take(6), SlotRead::default());
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
        assert_eq!(
            watch.on_observed(DebuggerMode::Disabled, None, 300),
            DriftStep::default()
        );
        for mode in [DebuggerMode::Unrecognized, DebuggerMode::Unknown] {
            assert_eq!(watch.on_observed(mode, None, 301), DriftStep::default());
        }
        let on = watch.on_observed(DebuggerMode::Enabled, Some(DebuggerChange::On), 400);
        assert_eq!(on.record, Some(DebuggerRecord::On));
        assert_eq!(on.write.map(|m| m.origin), Some(Origin::Aterm));
        assert_eq!(watch.drift_since(), None);
        let off = watch.on_observed(DebuggerMode::Disabled, Some(DebuggerChange::Off), 500);
        assert_eq!(off.record, Some(DebuggerRecord::Off));
        assert_eq!(watch.drift_since(), None);
        let on_disk = watch.recorded();
        assert_eq!(
            watch.on_observed(DebuggerMode::Enabled, None, 600),
            DriftStep::default()
        );
        assert!(!watch.seeded(), "it waits for a fresh read");
        let outside = watch.seed(on_disk, 601, false);
        assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
        assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
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
        assert_eq!(
            b.observe(probe(DebuggerMode::Enabled), 800).drift,
            DriftStep::default()
        );
        let outside = reread(&mut b, 800).drift;
        assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
        assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_peer_reading_between_a_changes_result_and_its_record_files_it_as_aterms() {
        let dir = scratch("result-window");
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
        apply(&b.observe(probe(DebuggerMode::Disabled), 210));
        assert!(b.records_due(210));
        assert_eq!(
            reread(&mut b, 210).drift.record,
            Some(DebuggerRecord::WasOnNowOff)
        );
        apply(&a.observe(probe(DebuggerMode::Disabled), 211));
        assert!(a.records_due(211), "A judges it against a fresh read");
        let out = a.ask_records();
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
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead {
            stale: ChangeMarker::new(DebuggerChange::On, child(32), 1),
            ..RecordsRead::default()
        });
        lane.poll(1);
        let ended = lane.observe(probe(DebuggerMode::Disabled), 2);
        assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
    }

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

    fn ready() -> DebuggerStepFacts {
        DebuggerStepFacts {
            windowed: true,
            macos: true,
            privacy: true,
            notice: true,
            config: true,
            admitted: true,
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
            frontmost: true,
            dialog_up: false,
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

    #[test]
    fn the_debugger_step_names_every_outcome_in_its_order() {
        use StepVerdict::{Drop, Park, Pause, Settle, Start, Wait};
        type Row = (&'static str, fn(&mut DebuggerStepFacts), StepVerdict);
        assert_eq!(admit_debugger_step(&ready()), Start);
        let rows: &[Row] = &[
            ("no standing yes", |f| f.admitted = false, Park),
            ("headless", |f| f.windowed = false, Drop(StepDrop::Headless)),
            ("not macOS", |f| f.macos = false, Drop(StepDrop::NotMacos)),
            (
                "privacy off",
                |f| f.privacy = false,
                Pause(StepPause::PrivacyOff),
            ),
            (
                "notice off",
                |f| f.notice = false,
                Pause(StepPause::NoticeOff),
            ),
            (
                "no config folder",
                |f| f.config = false,
                Drop(StepDrop::NoConfig),
            ),
            (
                "host cold",
                |f| f.host = HostAsked::Cold,
                Wait(StepWait::HostCold),
            ),
            (
                "no host",
                |f| f.host = HostAsked::Unnamed,
                Drop(StepDrop::NoHost),
            ),
            (
                "asked",
                |f| f.host = HostAsked::Asked,
                Drop(StepDrop::Asked),
            ),
            (
                "records unread",
                |f| f.records_read = false,
                Wait(StepWait::RecordsUnread),
            ),
            (
                "probe unread",
                |f| f.probe = None,
                Wait(StepWait::ProbePending),
            ),
            (
                "probe pending",
                |f| f.probe = Some(DebuggerProbe::pending()),
                Wait(StepWait::ProbePending),
            ),
            (
                "under way",
                |f| f.under_way = true,
                Settle(StepSettle::Underway),
            ),
            (
                "answered",
                |f| f.answered = true,
                Settle(StepSettle::Answered),
            ),
            (
                "enabled",
                |f| f.probe = Some(probe(DebuggerMode::Enabled)),
                Settle(StepSettle::AlreadyOn),
            ),
            ("drift", |f| f.drift = true, Settle(StepSettle::TurnedOff)),
            (
                "recorded on",
                |f| f.recorded = recorded(DebuggerMode::Enabled, Origin::Outside),
                Settle(StepSettle::TurnedOff),
            ),
            (
                "turned off by aterm",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Aterm),
                Settle(StepSettle::TurnedOff),
            ),
            (
                "seen turned off outside aterm",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Outside),
                Settle(StepSettle::TurnedOff),
            ),
            (
                "unrecognized",
                |f| f.probe = Some(probe(DebuggerMode::Unrecognized)),
                Drop(StepDrop::Unrecognized),
            ),
            (
                "status unread",
                |f| f.probe = Some(probe(DebuggerMode::Unknown)),
                Drop(StepDrop::Unread),
            ),
            (
                "probe timed out",
                |f| f.probe = Some(DebuggerProbe::refused(DebuggerProbeLabel::TimedOut)),
                Drop(StepDrop::Unread),
            ),
            (
                "not a member",
                |f| f.probe = off_as(DebuggerMember::NotMember),
                Drop(StepDrop::NotMember),
            ),
            (
                "membership unknown",
                |f| f.probe = off_as(DebuggerMember::Unknown),
                Drop(StepDrop::NotMember),
            ),
            (
                "not hardened",
                |f| f.self_hardened = SelfHardening::NotHardened,
                Drop(StepDrop::NotHardened),
            ),
            (
                "hardening unknown",
                |f| f.self_hardened = SelfHardening::Unknown,
                Drop(StepDrop::NotHardened),
            ),
            (
                "full disk access undecided",
                |f| f.fda_decided = false,
                Wait(StepWait::BehindFullDiskAccess),
            ),
            (
                "developer tools unread",
                |f| f.devtools_decided = false,
                Wait(StepWait::BehindDeveloperTools),
            ),
            ("a page", |f| f.page = true, Wait(StepWait::BehindPage)),
            (
                "the warm-up",
                |f| f.warmup = true,
                Wait(StepWait::BehindWarmup),
            ),
            (
                "a macOS dialog up",
                |f| f.dialog_up = true,
                Wait(StepWait::DialogUp),
            ),
            (
                "System State not frontmost",
                |f| f.frontmost = false,
                Wait(StepWait::NotFrontmost),
            ),
            (
                "owner quiet",
                |f| f.owner_quiet = true,
                Wait(StepWait::OwnerQuiet),
            ),
            (
                "the probe stale at the moment",
                |f| f.probe_fresh = false,
                Wait(StepWait::ProbeStale),
            ),
            (
                "first sight off",
                |f| f.recorded = recorded(DebuggerMode::Disabled, Origin::Unknown),
                Start,
            ),
            (
                "a developer",
                |f| f.probe = off_as(DebuggerMember::Developer),
                Start,
            ),
        ];
        for (name, change, expected) in rows {
            let mut facts = ready();
            change(&mut facts);
            assert_eq!(admit_debugger_step(&facts), *expected, "{name}");
        }
        let every = [
            Start,
            Park,
            Drop(StepDrop::Headless),
            Drop(StepDrop::NotMacos),
            Pause(StepPause::PrivacyOff),
            Pause(StepPause::NoticeOff),
            Drop(StepDrop::NoConfig),
            Drop(StepDrop::NoHost),
            Drop(StepDrop::Asked),
            Drop(StepDrop::Unrecognized),
            Drop(StepDrop::Unread),
            Drop(StepDrop::NotMember),
            Drop(StepDrop::NotHardened),
            Wait(StepWait::HostCold),
            Wait(StepWait::RecordsUnread),
            Wait(StepWait::ProbePending),
            Wait(StepWait::BehindFullDiskAccess),
            Wait(StepWait::BehindDeveloperTools),
            Wait(StepWait::BehindPage),
            Wait(StepWait::BehindWarmup),
            Wait(StepWait::DialogUp),
            Wait(StepWait::NotFrontmost),
            Wait(StepWait::OwnerQuiet),
            Wait(StepWait::ProbeStale),
            Settle(StepSettle::Underway),
            Settle(StepSettle::Answered),
            Settle(StepSettle::AlreadyOn),
            Settle(StepSettle::TurnedOff),
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
                Start => "start",
                Park => "park",
                Drop(why) => why.as_str(),
                Pause(why) => why.as_str(),
                Wait(why) => why.as_str(),
                Settle(why) => why.as_str(),
            };
            assert!(words.insert(word), "{word} names one outcome");
        }

        let order: &[Row] = &[
            (
                "a gate before the standing yes",
                |f| {
                    f.windowed = false;
                    f.admitted = false;
                },
                Drop(StepDrop::Headless),
            ),
            // A switch off pauses only a step that has its admission.
            (
                "the standing yes before a switch",
                |f| {
                    f.notice = false;
                    f.admitted = false;
                },
                Park,
            ),
            // Nothing is read, and nothing settled or written, without one.
            (
                "the standing yes before the host",
                |f| {
                    f.admitted = false;
                    f.host = HostAsked::Cold;
                    f.under_way = true;
                },
                Park,
            ),
            (
                "the standing yes before a settle",
                |f| {
                    f.admitted = false;
                    f.probe = Some(probe(DebuggerMode::Enabled));
                },
                Park,
            ),
            (
                "a switch before the host",
                |f| {
                    f.notice = false;
                    f.host = HostAsked::Cold;
                },
                Pause(StepPause::NoticeOff),
            ),
            // A pause answers nothing: a Mac the record names, or debugger
            // access already on, is judged once the switches are on again.
            (
                "a switch before the record",
                |f| {
                    f.privacy = false;
                    f.host = HostAsked::Asked;
                },
                Pause(StepPause::PrivacyOff),
            ),
            (
                "a switch before a settle",
                |f| {
                    f.notice = false;
                    f.probe = Some(probe(DebuggerMode::Enabled));
                },
                Pause(StepPause::NoticeOff),
            ),
            (
                "the config before the host",
                |f| {
                    f.config = false;
                    f.host = HostAsked::Cold;
                },
                Drop(StepDrop::NoConfig),
            ),
            (
                "the record before the reads",
                |f| {
                    f.host = HostAsked::Asked;
                    f.records_read = false;
                    f.probe = None;
                },
                Drop(StepDrop::Asked),
            ),
            (
                "the records before the probe",
                |f| {
                    f.records_read = false;
                    f.probe = None;
                    f.under_way = true;
                },
                Wait(StepWait::RecordsUnread),
            ),
            (
                "the probe before a settle",
                |f| {
                    f.probe = Some(DebuggerProbe::pending());
                    f.under_way = true;
                },
                Wait(StepWait::ProbePending),
            ),
            (
                "under way before the rule",
                |f| {
                    f.under_way = true;
                    f.probe = Some(probe(DebuggerMode::Unrecognized));
                },
                Settle(StepSettle::Underway),
            ),
            (
                "under way before answered",
                |f| {
                    f.under_way = true;
                    f.answered = true;
                },
                Settle(StepSettle::Underway),
            ),
            (
                "the probe before answered",
                |f| {
                    f.answered = true;
                    f.probe = Some(DebuggerProbe::pending());
                },
                Wait(StepWait::ProbePending),
            ),
            (
                "answered before an unrecognized rule",
                |f| {
                    f.answered = true;
                    f.probe = Some(probe(DebuggerMode::Unrecognized));
                },
                Settle(StepSettle::Answered),
            ),
            (
                "answered before an unread rule",
                |f| {
                    f.answered = true;
                    f.probe = Some(probe(DebuggerMode::Unknown));
                },
                Settle(StepSettle::Answered),
            ),
            (
                "answered before the account",
                |f| {
                    f.answered = true;
                    f.probe = off_as(DebuggerMember::NotMember);
                },
                Settle(StepSettle::Answered),
            ),
            (
                "answered before aterm's hardening",
                |f| {
                    f.answered = true;
                    f.self_hardened = SelfHardening::NotHardened;
                },
                Settle(StepSettle::Answered),
            ),
            (
                "answered before on",
                |f| {
                    f.answered = true;
                    f.probe = Some(probe(DebuggerMode::Enabled));
                },
                Settle(StepSettle::Answered),
            ),
            (
                "on before aterm's hardening",
                |f| {
                    f.probe = Some(probe(DebuggerMode::Enabled));
                    f.self_hardened = SelfHardening::NotHardened;
                },
                Settle(StepSettle::AlreadyOn),
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
                Settle(StepSettle::TurnedOff),
            ),
            (
                "the account before the hardening",
                |f| {
                    f.probe = off_as(DebuggerMember::NotMember);
                    f.self_hardened = SelfHardening::Unknown;
                },
                Drop(StepDrop::NotMember),
            ),
            (
                "a drop before the pages",
                |f| {
                    f.self_hardened = SelfHardening::NotHardened;
                    f.fda_decided = false;
                },
                Drop(StepDrop::NotHardened),
            ),
            (
                "full disk access before developer tools",
                |f| {
                    f.fda_decided = false;
                    f.devtools_decided = false;
                    f.page = true;
                },
                Wait(StepWait::BehindFullDiskAccess),
            ),
            (
                "the pages before the moment",
                |f| {
                    f.page = true;
                    f.frontmost = false;
                    f.dialog_up = true;
                },
                Wait(StepWait::BehindPage),
            ),
            (
                "another macOS dialog before the page",
                |f| {
                    f.dialog_up = true;
                    f.frontmost = false;
                },
                Wait(StepWait::DialogUp),
            ),
            (
                "the moment before the owner's quiet",
                |f| {
                    f.frontmost = false;
                    f.owner_quiet = true;
                },
                Wait(StepWait::NotFrontmost),
            ),
            (
                "the moment before a stale probe",
                |f| {
                    f.owner_quiet = true;
                    f.probe_fresh = false;
                },
                Wait(StepWait::OwnerQuiet),
            ),
            (
                "a stale reading of on settles",
                |f| {
                    f.probe = Some(probe(DebuggerMode::Enabled));
                    f.probe_fresh = false;
                },
                Settle(StepSettle::AlreadyOn),
            ),
        ];
        for (name, change, expected) in order {
            let mut facts = ready();
            change(&mut facts);
            assert_eq!(admit_debugger_step(&facts), *expected, "{name}");
        }
    }

    /// EVERY WAY THE STEP ENDS WITH AN ADMISSION IN HAND SETTLES THE ROW FOR ITS
    /// REVISION, OR SAYS WHY THE ROW SPEAKS FOR ITSELF (design §6: the
    /// Permissions stage completes when every row is in place, not needed, or
    /// handed, declined or triggered in this revision). A later revision on a
    /// Mac whose step is over — cancelled in macOS's dialog once — is declined,
    /// so its stage never waits on the row for good. A `[privacy]` switch off is
    /// a pause, which records nothing (the published opt-out is the row's wanted
    /// state while it is off). Not needed (a build that is not hardened, an
    /// account in neither group) is the row's own reading, and a rule read
    /// again later says nothing yet.
    #[test]
    fn every_end_with_an_admission_says_what_the_row_is() {
        use AccessOutcome::{Declined, Granted, Handed};
        for (why, outcome) in [
            (StepSettle::Underway, Handed),
            (StepSettle::Answered, Declined),
            (StepSettle::AlreadyOn, Granted),
            (StepSettle::TurnedOff, Declined),
        ] {
            assert_eq!(why.outcome(), outcome, "{why:?}");
        }
        for (why, outcome) in [
            (StepDrop::Asked, Some(Declined)),
            (StepDrop::Headless, None),
            (StepDrop::NotMacos, None),
            (StepDrop::NoConfig, None),
            (StepDrop::NoHost, None),
            (StepDrop::Unrecognized, None),
            (StepDrop::Unread, None),
            (StepDrop::NotMember, None),
            (StepDrop::NotHardened, None),
        ] {
            assert_eq!(why.outcome(), outcome, "{why:?}");
        }
    }

    #[test]
    fn a_waiting_step_looks_again_soon_only_for_a_read_or_a_moment() {
        use crate::devtools_helper::{PANEL_TICK, RECHECK_EVERY};
        let soon = [
            StepWait::HostCold,
            StepWait::ProbePending,
            StepWait::ProbeStale,
            StepWait::Announced,
            StepWait::Recording,
            StepWait::NotFrontmost,
            StepWait::DialogUp,
        ];
        for why in soon {
            assert_eq!(why.look_again(), PANEL_TICK, "{why:?}");
        }
        assert!(
            ASK_PROBE_FRESH > PANEL_TICK,
            "the reading a stale probe waits for is fresh at the next look"
        );
        let later = [
            StepWait::RecordsUnread,
            StepWait::BehindFullDiskAccess,
            StepWait::BehindDeveloperTools,
            StepWait::BehindPage,
            StepWait::BehindWarmup,
            StepWait::OwnerQuiet,
            StepWait::ProbeUnread,
        ];
        for why in later {
            assert_eq!(why.look_again(), RECHECK_EVERY, "{why:?}");
        }
        assert!(!PANEL_TICK.is_zero() && !RECHECK_EVERY.is_zero());
        let words: std::collections::BTreeSet<_> =
            soon.iter().chain(&later).map(|why| why.as_str()).collect();
        assert_eq!(
            words.len(),
            soon.len() + later.len(),
            "each wait logs its own word"
        );
    }

    #[test]
    fn the_asked_record_is_durable_one_line_per_mac_and_bounded() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = scratch("asked");
        let config = dir.join("aterm.toml");
        let path = dir.join(ASKED_RECORD);
        assert!(!read_asked(&config, "MAC-A"), "a genuine absence");
        let recording = Recording::start();
        note_asked(&config, "MAC-A", 100).unwrap();
        let durable: &[Flush] = if cfg!(unix) {
            &[Flush::Durable, Flush::Durable]
        } else {
            &[Flush::Durable]
        };
        assert_eq!(
            recording.take(),
            durable,
            "written ahead, durably: the record, then (unix) its folder's entry"
        );
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
        std::fs::write(&path, "MAC-B 1\nMAC-C 2\n").unwrap();
        assert!(!read_asked(&config, "MAC-A"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn askable() -> DebuggerChangeLane {
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        lane.settle_records(RecordsRead::default());
        lane.poll(1);
        lane.observe(probe(DebuggerMode::Disabled), 2);
        lane
    }

    /// The window's own admission, as a test stands one in.
    #[cfg(unix)]
    fn admitted() -> crate::workstation_setup::AdmittedAccess {
        crate::workstation_setup::AdmittedAccess::for_test(1)
    }

    #[cfg(unix)]
    #[test]
    fn the_standing_yes_writes_ahead_and_starts_what_a_press_starts() {
        let dir = scratch("offer");
        let config = dir.join("aterm.toml");
        let mut lane = askable();
        let ticket = lane
            .start_now(&config, "MAC-A", admitted())
            .expect("the standing yes gives the ticket");
        assert!(read_asked(&config, "MAC-A"), "written ahead of the change");
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
            askable().start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Asked),
            "a relaunch on the same config starts nothing"
        );
        let mut other = askable();
        assert!(
            other.start_now(&config, "MAC-B", admitted()).is_ok(),
            "another Mac on a synced config"
        );
        assert_eq!(
            askable().start_now(&config, "MAC-B", admitted()),
            Err(StartEnd::Asked),
            "once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_start_writes_nothing_and_forgo_writes_only_the_record() {
        let dir = scratch("offer-refused");
        let config = dir.join("aterm.toml");
        let unwritten = |dir: &Path| !dir.join(ASKED_RECORD).exists();
        let unread = &mut DebuggerChangeLane::with_liveness(|_| false);
        assert_eq!(
            unread.start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Refused(Refusal::RecordsUnread))
        );
        let mut on = askable();
        on.observe(probe(DebuggerMode::Enabled), 3);
        assert_eq!(
            on.start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Refused(Refusal::State(
                DebuggerChange::On,
                DebuggerMode::Enabled
            )))
        );
        let mut busy = askable();
        busy.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .unwrap();
        assert_eq!(
            busy.start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Refused(Refusal::InFlight))
        );
        {
            let LockTry::Held(_peer) = take_change_lock(&config, Duration::ZERO) else {
                panic!("the scratch lock is free");
            };
            assert_eq!(
                askable().start_now(&config, "MAC-A", admitted()),
                Err(StartEnd::Refused(Refusal::InFlight)),
                "a peer's worker holds the lock"
            );
        }
        let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        std::fs::write(dir.join(CHANGE_MARKER), marker.encode()).unwrap();
        assert_eq!(
            askable().start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Refused(Refusal::InFlight)),
            "a change marker on disk"
        );
        std::fs::remove_file(dir.join(CHANGE_MARKER)).unwrap();
        assert!(unwritten(&dir), "no refusal wrote the record");

        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let beyond = locked.join("sub").join("aterm.toml");
        assert!(!read_asked(&beyond, "MAC-A"), "absent");
        assert_eq!(
            askable().start_now(&beyond, "MAC-A", admitted()),
            Err(StartEnd::NoLock)
        );
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let sealed = dir.join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        let sealed_config = sealed.join("aterm.toml");
        let LockTry::Held(made) = take_change_lock(&sealed_config, Duration::ZERO) else {
            panic!("the scratch lock is free");
        };
        drop(made);
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        assert_eq!(
            askable().start_now(&sealed_config, "MAC-A", admitted()),
            Err(StartEnd::NotRecorded)
        );
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(&sealed)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [CHANGE_LOCK], "the lock alone");

        assert!(DebuggerChangeLane::forgo(None, "MAC-A").is_err());
        DebuggerChangeLane::forgo(Some(&config), "MAC-A").unwrap();
        assert!(read_asked(&config, "MAC-A"));
        assert!(!dir.join(CHANGE_MARKER).exists(), "the record alone");
        assert_eq!(
            askable().start_now(&config, "MAC-A", admitted()),
            Err(StartEnd::Asked),
            "settled: never started after"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_step_queue_looks_when_due_and_reads_the_record_once() {
        let idle = DebuggerStepQueue::new(false);
        assert!(!idle.queued() && !idle.due(Instant::now()));
        assert_eq!(idle.next_deadline(), None);
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        let now = Instant::now();
        assert!(queue.due(now) && queue.host_read_ended());
        assert_eq!(
            queue.next_deadline(),
            None,
            "the first look is the next park's"
        );
        assert_eq!(queue.start_host_read().map(|_| ()), None, "read already");
        assert!(
            queue.wait(StepWait::NotFrontmost, now + RESULT_RECHECK),
            "a new wait"
        );
        assert!(
            !queue.wait(StepWait::NotFrontmost, now + RESULT_RECHECK),
            "the same wait is not logged twice"
        );
        assert!(!queue.due(now) && queue.due(now + RESULT_RECHECK));
        assert_eq!(queue.next_deadline(), Some(now + RESULT_RECHECK));
        assert!(queue.wait(StepWait::BehindPage, now + RESULT_RECHECK));
        let dir = scratch("queue");
        let config = dir.join("aterm.toml");
        assert_eq!(queue.asked("MAC-A"), None, "not read yet");
        let (slot, read) = queue.start_record_read("MAC-A").expect("the first time");
        assert_eq!(read, RecordRead::Inline, "a stand-in reads where it asks");
        assert!(queue.start_record_read("MAC-A").is_none(), "out already");
        assert_eq!(queue.asked("MAC-A"), None, "out, not ended");
        let _ = slot.set(read_asked(&config, "MAC-A"));
        assert_eq!(queue.asked("MAC-A"), Some(false));
        note_asked(&config, "MAC-A", 1).unwrap();
        assert_eq!(queue.asked("MAC-A"), Some(false), "read once per process");
        assert_eq!(
            queue.asked("MAC-B"),
            None,
            "another host's line is its own read"
        );
        assert!(queue.start_record_read("MAC-B").is_some());
        assert_eq!(
            DebuggerStepQueue::new(true)
                .start_record_read("MAC-A")
                .map(|(_, read)| read),
            Some(RecordRead::Thread),
            "a launch reads it off the event loop"
        );
        let read = DebuggerFacts::default();
        assert_eq!(queue.take_prepared(), None, "none started");
        assert!(!queue.preparing() && !queue.starting());
        let prepared = queue.start_prepare(read).expect("the first time");
        assert!(queue.preparing() && queue.starting(), "out");
        assert!(queue.start_prepare(read).is_none(), "out already");
        let at = now + RESULT_RECHECK + RESULT_RECHECK;
        queue.wait(StepWait::Recording, at);
        assert!(!queue.due(now) && queue.take_prepared().is_none(), "out");
        let _ = prepared.set(StartPrep::Recorded);
        assert!(!queue.preparing() && queue.starting(), "ended, not taken");
        assert!(queue.due(now), "its end is the next look");
        assert!(queue.start_prepare(read).is_none(), "not taken yet");
        assert_eq!(queue.take_prepared(), Some((StartPrep::Recorded, read)));
        assert_eq!(queue.take_prepared(), None, "taken once");
        assert!(
            !queue.due(now) && !queue.starting(),
            "taken: the look's own time again"
        );
        // Put back, it waits for its time, then is taken first.
        queue.keep_prepared(StartPrep::Recorded, read);
        assert!(queue.starting() && !queue.preparing());
        assert!(!queue.due(now), "a kept answer waits for its time");
        assert_eq!(queue.take_prepared(), Some((StartPrep::Recorded, read)));
        assert!(
            queue.start_prepare(read).is_some(),
            "the next start starts another"
        );
        queue.end();
        assert!(!queue.queued() && !queue.due(now + RESULT_RECHECK));
        assert_eq!(queue.next_deadline(), None);
        let mut cold = DebuggerStepQueue::new(true);
        let done = cold.start_host_read().expect("the first time");
        assert!(cold.start_host_read().is_none() && !cold.host_read_ended());
        done.store(true, std::sync::atomic::Ordering::Release);
        assert!(cold.host_read_ended());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_step_parks_without_a_standing_yes_and_comes_back_with_one() {
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        let now = Instant::now();
        queue.unpark(1);
        assert!(queue.queued() && !queue.parked(), "nothing to unpark");
        queue.wait(StepWait::NotFrontmost, now + RESULT_RECHECK);
        queue.park();
        assert!(!queue.queued() && queue.parked());
        assert!(!queue.due(now + RESULT_RECHECK) && queue.next_deadline().is_none());
        queue.unpark(1);
        assert!(queue.queued() && !queue.parked() && queue.due(now));
        let (slot, _) = queue.start_record_read("MAC-A").expect("the first read");
        let _ = slot.set(false);
        assert_eq!(queue.asked("MAC-A"), Some(false));
        queue.end();
        queue.unpark(1);
        assert!(
            !queue.queued(),
            "an ended step stays ended for its revision"
        );
        queue.unpark(2);
        assert!(
            queue.queued() && queue.due(now),
            "a new approval asks again"
        );
        assert_eq!(queue.asked("MAC-A"), None, "its record is read again");
        assert!(queue.start_record_read("MAC-A").is_some());
        queue.unpark(2);
        assert!(queue.queued(), "still queued: nothing changes");
        queue.note_started(3);
        queue.end();
        queue.unpark(3);
        assert!(!queue.queued(), "a press spent that revision");
        let mut idle = DebuggerStepQueue::new(false);
        idle.unpark(1);
        idle.unpark(2);
        assert!(!idle.queued(), "never queued headless");
        // A start's disk half answered before a park is spent after it: a Mac
        // it recorded is never left without its change.
        let mut kept = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        kept.keep_prepared(StartPrep::Recorded, DebuggerFacts::default());
        kept.park();
        assert!(kept.starting(), "kept through the park");
        kept.unpark(1);
        assert!(kept.due(now) && kept.take_prepared().is_some());
        // A person's Turn On… answers the question: nothing kept is spent.
        kept.keep_prepared(StartPrep::Recorded, DebuggerFacts::default());
        kept.answered_by_press();
        assert!(!kept.starting() && !kept.queued());
    }

    #[test]
    fn a_privacy_switch_off_pauses_the_step_and_on_again_resumes_it() {
        let now = Instant::now();
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        queue.resume();
        assert!(queue.queued() && !queue.paused(), "nothing to resume");
        queue.keep_prepared(StartPrep::Recorded, DebuggerFacts::default());
        queue.wait(StepWait::NotFrontmost, now + RESULT_RECHECK);
        queue.pause();
        assert!(!queue.queued() && queue.paused());
        assert!(!queue.due(now + RESULT_RECHECK) && queue.next_deadline().is_none());
        assert!(queue.starting(), "a kept answer stays kept");
        queue.resume();
        assert!(queue.queued() && !queue.paused() && queue.due(now));
        assert!(queue.take_prepared().is_some());
        queue.pause();
        queue.answered_by_press();
        queue.resume();
        assert!(!queue.queued(), "a press ended it for good");
    }

    #[test]
    fn a_new_approval_while_the_change_runs_joins_it() {
        use aterm_types::macos_setup::{Access, access_record};
        let root = crate::test_scratch::outlived_dir("aterm-access-");
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"))
            .recording_in(Some(root.path().to_path_buf()));
        queue.unpark(3);
        queue.note_started(3);
        queue.end();
        for revision in [3, 4, 4, 5, 4] {
            queue.join_started(revision);
        }
        assert!(!queue.queued(), "the change answers them: no step");
        queue.record_written_for_test();
        let latest = |revision| access_record::latest(root.path(), revision, Access::Debugger);
        assert_eq!(latest(3), None, "the start records its own hand-over");
        assert_eq!(latest(4), Some(AccessOutcome::Handed));
        assert_eq!(latest(5), Some(AccessOutcome::Handed));
        assert_eq!(queue.finish(), [3, 4, 5], "the change's outcome is theirs");
        assert!(queue.finish().is_empty(), "once");
        queue.unpark(6);
        assert!(
            queue.queued(),
            "after the change, a new approval is the step's"
        );
    }

    #[test]
    fn the_page_names_the_dialog_for_a_presented_frame_before_the_change() {
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        let (one, two) = (crate::WindowId(1), crate::WindowId(2));
        let at = Instant::now();
        let later = at + ANNOUNCE_FOR;
        assert!(!queue.announced() && !queue.shown(one, 9, later));
        assert!(queue.announce(one, 4, at), "news");
        assert!(queue.announced());
        assert!(
            !queue.announce(one, 5, at + ANNOUNCE_FOR),
            "the same window"
        );
        assert!(!queue.shown(one, 4, later + ANNOUNCE_FOR), "no frame since");
        assert!(!queue.shown(one, 5, at), "not for long enough");
        assert!(!queue.shown(two, 5, later), "another window");
        assert!(queue.shown(one, 5, later));
        assert!(queue.announce(two, 7, later), "moved: the count restarts");
        assert!(!queue.shown(one, 9, later + ANNOUNCE_FOR));
        assert!(!queue.shown(two, 8, later), "not for long enough there");
        assert!(queue.shown(two, 8, later + ANNOUNCE_FOR));
        queue.withdraw();
        assert!(!queue.announced() && !queue.shown(two, 9, later + ANNOUNCE_FOR));
        assert!(queue.announce(one, 1, at));
        queue.park();
        assert!(!queue.announced(), "a parked step names nothing");
        assert_eq!(queue.started(), None);
        queue.note_started(3);
        assert_eq!(queue.started(), Some(3));
        assert_eq!(queue.finish(), [3]);
        assert!(queue.finish().is_empty(), "once");
    }

    #[test]
    fn refused_starts_hold_the_step_for_a_bounded_span() {
        use crate::devtools_helper::RECHECK_EVERY;
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        let first = Instant::now();
        assert!(queue.refused(first), "the first refusal looks again");
        assert!(queue.queued() && !queue.due(first));
        assert_eq!(queue.next_deadline(), Some(first + RECHECK_EVERY));
        let mut at = first;
        while at + RECHECK_EVERY < first + REFUSED_FOR {
            at += RECHECK_EVERY;
            assert!(queue.due(at));
            queue.wait(StepWait::NotFrontmost, at + RECHECK_EVERY);
            assert!(queue.refused(at), "still within the span");
        }
        assert!(!queue.refused(first + REFUSED_FOR), "the span is over");
        assert!(!queue.queued() && queue.next_deadline().is_none());
    }

    #[test]
    fn reads_that_read_nothing_hold_a_recorded_step_for_a_bounded_span() {
        use crate::devtools_helper::RECHECK_EVERY;
        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        let first = Instant::now();
        assert!(queue.unread(first), "the first read again");
        let mut at = first;
        while at + RECHECK_EVERY < first + REFUSED_FOR {
            at += RECHECK_EVERY;
            queue.wait(StepWait::ProbeUnread, at + RECHECK_EVERY);
            assert!(queue.unread(at), "still within the span");
        }
        assert!(!queue.unread(first + REFUSED_FOR), "the span is over");
        assert!(!queue.queued() && queue.next_deadline().is_none());

        let mut queue = DebuggerStepQueue::queued_for_test(|| Some("MAC-A"));
        assert!(queue.unread(first), "the first read again");
        let back = first + REFUSED_FOR + RECHECK_EVERY;
        for read in 2..UNREAD_READS {
            assert!(
                queue.unread(back),
                "read {read} past the span: fewer than UNREAD_READS"
            );
        }
        assert!(queue.queued());
        assert!(!queue.unread(back), "UNREAD_READS past the span: over");
        assert!(!queue.queued());
    }

    #[cfg(unix)]
    #[test]
    fn the_start_judges_the_disk_as_the_worker_does() {
        let root = aterm_tempfile::TempDir::new().expect("owned offer-disk fixture");
        let dir = root.path();
        let config = dir.join("aterm.toml");
        let marker_at = dir.join(CHANGE_MARKER);
        let lock_at = dir.join(CHANGE_LOCK);
        let asked = |host| read_asked(&config, host);
        let mut contender = askable();
        contender.settle_records(read_records(&config, |_| false));
        contender.poll(3);
        assert!(
            !contender.records_due(now_secs()),
            "an idle lane reads nothing more"
        );
        let LockTry::Held(claim) = take_change_lock(&config, Duration::ZERO) else {
            panic!("a fresh lock can be taken");
        };
        assert_eq!(
            contender.start_now(&config, "MAC-CLAIM", admitted()),
            Err(StartEnd::Refused(Refusal::InFlight)),
            "another lane cannot start before the claimed record is written"
        );
        assert!(!asked("MAC-CLAIM"), "the refused contender wrote nothing");
        assert!(
            contender.records_due(now_secs()),
            "a held lock asks the park for a records read"
        );
        let peer = ChangeMarker::new(DebuggerChange::On, child(5151), 9).unwrap();
        std::fs::write(&marker_at, peer.encode()).unwrap();
        contender.settle_records(read_records(&config, |_| true));
        contender.poll(now_secs());
        assert!(
            contender.under_way(),
            "the peer's running change is followed: the next look settles"
        );
        std::fs::remove_file(&marker_at).unwrap();
        drop(claim);
        assert!(
            askable()
                .start_now(&config, "MAC-CLAIM", admitted())
                .is_ok()
        );
        assert_eq!(
            askable().start_now(&config, "MAC-CLAIM", admitted()),
            Err(StartEnd::Asked),
            "the durable claim is visible after the lock is released"
        );
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
                lane.start_now(&config, host, admitted()).is_ok(),
                "{name} at the marker's path is no change"
            );
            assert!(asked(host), "{name}: asked");
            if marker_at.is_dir() && !marker_at.is_symlink() {
                std::fs::remove_dir(&marker_at).unwrap();
            } else {
                std::fs::remove_file(&marker_at).unwrap();
            }
        }

        let mut lane = askable();
        lane.settle_records(read_records(&config, |_| false));
        lane.poll(3);
        assert!(
            !lane.records_due(now_secs()),
            "an idle lane reads nothing more"
        );
        let gone = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
        std::fs::write(&marker_at, gone.encode()).unwrap();
        assert_eq!(
            lane.start_now(&config, "MAC-E", admitted()),
            Err(StartEnd::Refused(Refusal::InFlight))
        );
        assert!(!asked("MAC-E"), "nothing written");
        assert!(lane.records_due(now_secs()), "the park reads the records");
        lane.settle_records(read_records(&config, |_| false));
        lane.poll(now_secs());
        assert!(lane.under_way(), "adopted: the next look settles");
        let mut released = askable();
        released.released = Some(child(4242));
        assert!(
            released.start_now(&config, "MAC-E", admitted()).is_ok(),
            "a gone child this lane follows is no change"
        );
        std::fs::remove_file(&marker_at).unwrap();

        std::fs::remove_file(&lock_at).unwrap();
        std::os::unix::fs::symlink("/dev/null", &lock_at).unwrap();
        assert_eq!(
            askable().start_now(&config, "MAC-F", admitted()),
            Err(StartEnd::NoLock)
        );
        assert!(!asked("MAC-F"), "nothing written");
        std::fs::remove_file(&lock_at).unwrap();
    }
}

/// Tier-1 `debugger_access_conforms_to_the_model` (design §5.4): drives the production composition.
#[cfg(test)]
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

    /// Normalise a mode by the change: its precondition is 0, its target 1, an unrecognised rule 2, and not read 3.
    pub(crate) fn norm(mode: DebuggerMode, change: DebuggerChange) -> i64 {
        match mode {
            m if m == change.precondition() => 0,
            m if m == change.target() => 1,
            DebuggerMode::Unrecognized => 2,
            _ => 3,
        }
    }

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

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Read(i64),
        Spawn(Vec<String>),
    }

    struct Env {
        change: DebuggerChange,
        world: AtomicI64,
        log: Mutex<Vec<Event>>,
        stage: AtomicI64,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct Seen {
        clobbered: i64,
        fresh: i64,
        launches: i64,
        unconsented: i64,
        armed: i64,
        unarmed: i64,
        misadopted: i64,
        reasked: i64,
        overlapped: i64,
        /// The debugger step took a yes with no admission from the window's
        /// gate (`workstation_setup::gate_admission_for_test` gave none).
        unadmitted: i64,
    }

    /// The projection's inputs besides the lane.
    pub(crate) struct Observed<'a> {
        world: i64,
        cached: DebuggerProbe,
        lock: i64,
        inflight: i64,
        stale: i64,
        asked: i64,
        seen: &'a Seen,
    }

    /// The projection (the anchors' `project`).
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
        action = "PlanConsentAgain",
        reason = "Buggy = 2 mutant: start_from_plan reads the asked record before it writes \
                  it, and writes it before the yes is spent; the negative control starts over \
                  a record it never reads, and the harness fails on AtMostOneOffer."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "PlanConsentWhileInFlight",
        reason = "Buggy = 2 mutant: start_from_plan is refused while a change is under way \
                  (the press's own refusal, then a change marker or held lock on disk); the \
                  negative control starts over our running child, and the harness fails on \
                  NoOfferOverAChange."
    )]
    #[aterm_spec_anchor::spec_unmodeled(
        machine = "DebuggerAccessChange",
        action = "ConsentUnadmitted",
        reason = "Buggy = 2 mutant: start_from_plan takes its yes only from an AdmittedAccess, \
                  which only the window's PermissionGate makes for its own admission; the \
                  negative control starts the change in the Admitted = 0 scenario (a control \
                  client's act, a status poll, a hand-edited record), and the harness fails on \
                  ConsentOnlyWhenAdmitted."
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
        state.insert("unadmitted", observed.seen.unadmitted);
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
        SkipRecheck,
        IgnorePeerLock,
        PressWhileInFlight,
        ClaimFromExit,
        DropPollInFlight,
        RetryAfterCancel,
        AdoptLive,
        PlanConsentAgain,
        PlanConsentWhileInFlight,
        ConsentUnadmitted,
    }

    /// What the debugger step came to in the rig.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Plan {
        Started,
        Parked,
        End(StartEnd),
    }

    struct Worker {
        exit: mpsc::Sender<DevtoolsChangeRun>,
        handle: std::thread::JoinHandle<()>,
        env: Arc<Env>,
        child: DevtoolsChild,
    }

    /// The harness: the real lane, the environment, and the model state it must keep agreeing with.
    pub(crate) struct Rig {
        model: Model,
        lane: DebuggerChangeLane,
        change: DebuggerChange,
        fault: Fault,
        dir: PathBuf,
        config: PathBuf,
        world: i64,
        cached: DebuggerProbe,
        seen: Seen,
        consumed: usize,
        worker: Option<Worker>,
        peer: Option<(ChangeLock, DevtoolsChild)>,
        peer_gone: Option<DevtoolsChild>,
        inherited: Option<DevtoolsChild>,
        state: State,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
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
            if !self.dir.as_os_str().is_empty() {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
    }

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

        /// A fresh lane: records read, nothing observed — the model's initial state.
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

        /// A fresh lane in a process whose window holds no admission of a setup
        /// plan (the model's `Admitted = 0` scenario: a control client's act, a
        /// status poll or a hand-edited record admitted it, or nothing did).
        fn unadmitted(change: DebuggerChange, world: DebuggerMode, fault: Fault) -> Self {
            let mut rig = Self::new(change, world, fault);
            rig.model = aterm_spec::interp::with_consts(&rig.model, &[("Admitted", 0)]);
            rig
        }

        /// The model's `Admitted`: whether this window holds its own admission.
        fn admitted(&self) -> bool {
            self.model
                .consts
                .iter()
                .find(|(name, _)| *name == "Admitted")
                .is_some_and(|&(_, value)| value == 1)
        }

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

        fn relaunch(mut self) -> Self {
            assert!(
                self.worker.is_none() && self.peer.is_none() && self.inherited.is_none(),
                "the rig relaunches with nothing running"
            );
            let mut lane = DebuggerChangeLane::with_liveness(alive);
            lane.flow.direction = self.change;
            let mut rig = Self {
                // The same scenario: the model's constants carry over.
                model: self.model.clone(),
                lane,
                change: self.change,
                fault: self.fault,
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
            match try_change_lock(&self.config) {
                LockTry::Busy => 1,
                LockTry::Held(_) if marker_live => 1,
                LockTry::Held(_) => 0,
                LockTry::Unavailable(error) => panic!("the scratch lock: {error}"),
            }
        }

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

        fn stale_now(&self) -> i64 {
            i64::from(
                read_debugger_marker(&self.config, CHANGE_MARKER)
                    .and_then(|text| ChangeMarker::parse(&text))
                    .is_some_and(|marker| {
                        !alive(&marker.child) && !self.lane.follows(&marker.child)
                    }),
            )
        }

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

        fn probe(&mut self) {
            let read = scripted(denorm(self.world, self.change));
            self.step("Probe", |rig| rig.observe(read));
        }

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
            let patience = PATIENCE_FOR_TEST.with(std::cell::Cell::get);
            let handle = std::thread::Builder::new()
                .name("rig-consent-devtools".into())
                .spawn(move || {
                    let _patience = patience.map(patience_for_test);
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
                self.seen.fresh = 0;
                if let Some(worker) = self.worker.take() {
                    worker.handle.join().expect("the worker ends");
                }
            }
        }

        fn press(&mut self, answer: OwnerAnswer) {
            let pre = self.now();
            assert_eq!(
                pre, self.state,
                "press: the projection drifted from the model"
            );
            if self.fault == Fault::PressWhileInFlight
                && matches!(self.lane.flow.phase, Phase::InFlight { .. })
            {
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

        /// System State's debugger step at its moment, in one step: the window's admission from a real `PermissionGate`, spent as the yes by `DebuggerChangeLane::start_now`.
        fn plan_consent(&mut self) -> Plan {
            assert_eq!(
                self.change,
                DebuggerChange::On,
                "aterm never turns debugger access off by itself"
            );
            let pre = self.now();
            assert_eq!(
                pre, self.state,
                "plan consent: the projection drifted from the model"
            );
            let enabled = self.model.action_enabled("PlanConsent", &pre);
            let admission = crate::workstation_setup::gate_admission_for_test(self.admitted());
            assert_eq!(
                admission.is_some(),
                self.admitted(),
                "the gate holds the admission the scenario names"
            );
            let unadmitted = admission.is_none();
            let fault = self.fault;
            let mut forced = false;
            let started = match (fault, admission) {
                (Fault::PlanConsentWhileInFlight, Some(_))
                    if matches!(self.lane.flow.phase(), Phase::InFlight { .. }) =>
                {
                    forced = true;
                    note_asked(&self.config, RIG_HOST, now_secs()).expect("the scratch record");
                    self.lane
                        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                        .map_err(StartEnd::Refused)
                }
                (Fault::ConsentUnadmitted, None) => {
                    forced = true;
                    note_asked(&self.config, RIG_HOST, now_secs()).expect("the scratch record");
                    self.lane
                        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
                        .map_err(StartEnd::Refused)
                }
                (_, None) => {
                    assert!(
                        !enabled,
                        "PlanConsent: the model starts what no admission allows"
                    );
                    assert_eq!(self.now(), pre, "a parked step changed nothing");
                    return Plan::Parked;
                }
                (_, Some(admitted)) => {
                    if fault == Fault::PlanConsentAgain {
                        let _ = std::fs::remove_file(self.dir.join(ASKED_RECORD));
                    }
                    self.lane.start_now(&self.config, RIG_HOST, admitted)
                }
            };
            // Whether the step went past its gates: its record written and a
            // yes spent, or (the in-flight fault) its record written over a
            // change under way.
            let reached = started.is_ok() || forced;
            if reached {
                if pre["asked"] == 1 {
                    self.seen.reasked = 1;
                }
                if pre["phase"] != 0 || pre["lock"] != 0 || pre["inflight"] != 0 {
                    self.seen.overlapped = 1;
                }
                if unadmitted {
                    self.seen.unadmitted = 1;
                }
            }
            if started.is_ok() {
                // A ticket is minted only for a yes: the step took one.
                self.seen.fresh = 1;
            }
            if reached && !enabled {
                // A step the model forbids is named by the law it breaks.
                let post = self.now();
                for invariant in &self.model.invariants {
                    assert!(
                        self.model.check_invariant(invariant.name, &post),
                        "PlanConsent broke {}: {pre:?} -> {post:?}",
                        invariant.name
                    );
                }
            }
            assert_eq!(
                reached,
                enabled,
                "PlanConsent: the composition {} what the model {}: {pre:?}",
                if reached { "took" } else { "refused" },
                if enabled { "allows" } else { "forbids" },
            );
            let ticket = match started {
                Ok(ticket) => ticket,
                Err(end) => {
                    assert_eq!(self.now(), pre, "a refused start ({end:?}) changed nothing");
                    return Plan::End(end);
                }
            };
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
                self.chain(&pre, &["PlanConsent", "Recheck", "Launch"])
            } else {
                self.chain(&pre, &["PlanConsent", "Recheck"])
            };
            let post = self.now();
            assert!(
                targets.contains(&post),
                "plan consent: {pre:?} -> {post:?} is none of {targets:?}"
            );
            self.state = post;
            self.check_invariants();
            Plan::Started
        }

        fn forgo(&mut self) {
            self.step("Forgo", |rig| {
                let before = rig.now();
                DebuggerChangeLane::forgo(Some(&rig.config), RIG_HOST)
                    .expect("the scratch record is writable");
                rig.now() != before
            });
        }

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
                    rig.lane.flow.observed = change.target();
                }
                rig.lane.flow.phase() != before
            });
        }

        fn environment(&mut self, action: &str, world: i64) {
            self.step(action, |rig| {
                rig.world = world;
                if let Some(worker) = rig.worker.as_ref() {
                    worker.env.world.store(world, Ordering::SeqCst);
                }
                true
            });
        }

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

        fn reread(&mut self, action: &str) {
            self.step(action, |rig| {
                let before = rig.now();
                let liveness: fn(&DevtoolsChild) -> bool = if rig.fault == Fault::AdoptLive {
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

    fn scenarios(fault: Fault) {
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.press(OwnerAnswer::yes_for_test());
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.press(OwnerAnswer::yes_for_test());
            rig.probe();
            rig.notice();
            rig.exit("ChildApplied");
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.notice();
            assert!(
                rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: the marker outlives the taken result"
            );
            rig.press(OwnerAnswer::yes_for_test());
            rig.probe();
            assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(change)));
            assert_eq!(rig.recorded(), Some((1, Origin::Aterm)), "{change:?}");
            assert!(
                !rig.dir.join(CHANGE_MARKER).exists(),
                "{change:?}: and goes once the re-probe is recorded"
            );
            assert_eq!(rig.state["claim"], 1, "{change:?}: the re-probe says it");

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

            for (moved, world) in [("Customize", 2), ("SetOutside", 1)] {
                let mut rig = Rig::new(change, change.precondition(), fault);
                rig.probe();
                rig.environment(moved, world);
                rig.press(OwnerAnswer::yes_for_test());
                assert_eq!(rig.seen.launches, 0, "{change:?} {moved}: nothing launched");
                rig.probe();
                rig.press(OwnerAnswer::yes_for_test());
            }

            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.press(OwnerAnswer::yes_for_test());
            rig.environment("Customize", 2);
            rig.exit("ChildClobbers");
            rig.notice();
            rig.probe();
            assert_eq!(rig.state["clobbered"], 1);

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
            rig.peer_launch();
            rig.exit("ChildApplied");
            rig.notice();
            rig.probe();

            let mut rig = Rig::inherited(change, change.precondition());
            rig.press(OwnerAnswer::yes_for_test());
            rig.peer_launch();
            rig.notice();
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

            let mut rig = Rig::new(change, change.precondition(), fault);
            rig.probe();
            rig.reread("Adopt");
            rig.peer_launch();
            rig.reread("Follow");
            rig.press(OwnerAnswer::yes_for_test());
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
            rig.press(OwnerAnswer::yes_for_test());
            rig.reread("Adopt");
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
            rig.reread("Adopt");
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
        debugger_step_scenarios(fault);
    }

    fn debugger_step_scenarios(fault: Fault) {
        let on = DebuggerChange::On;
        let yes = OwnerAnswer::yes_for_test;
        let unread = Plan::End(StartEnd::Refused(Refusal::State(on, DebuggerMode::Unknown)));
        let in_flight = Plan::End(StartEnd::Refused(Refusal::InFlight));
        let asked = Plan::End(StartEnd::Asked);

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        assert_eq!(
            rig.plan_consent(),
            unread,
            "nothing is started before the probe"
        );
        assert_eq!(rig.state["asked"], 0, "and nothing is recorded");
        rig.probe();
        assert_eq!(rig.plan_consent(), Plan::Started);
        assert_eq!(rig.plan_consent(), in_flight);
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
            rig.plan_consent(),
            asked,
            "once per Mac: drift never re-enables"
        );
        rig.forgo();

        // Cancel in macOS's dialog: settled for good, and the button is the
        // owner's still.
        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        assert_eq!(rig.plan_consent(), Plan::Started);
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::NotChanged(on)));
        assert_eq!(rig.plan_consent(), asked, "never retried");
        let mut rig = rig.relaunch();
        assert_eq!(rig.plan_consent(), unread);
        rig.probe();
        assert_eq!(rig.plan_consent(), asked, "the next launch starts nothing");
        rig.press(yes());
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.seen.launches, 2, "the button is the owner's still");

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        rig.peer_launch();
        assert_eq!(
            rig.plan_consent(),
            in_flight,
            "a peer's change holds the lock"
        );
        rig.peer_exit();
        assert_eq!(
            rig.plan_consent(),
            in_flight,
            "its marker waits for that peer"
        );
        rig.peer_settle();
        assert_eq!(rig.plan_consent(), Plan::Started);

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.probe();
        rig.press(yes());
        assert_eq!(rig.plan_consent(), in_flight, "our own child is in flight");
        rig.forgo();
        rig.exit("ChildUnchanged");
        rig.notice();
        rig.probe();
        assert_eq!(rig.plan_consent(), asked, "settled under way");

        let mut rig = Rig::new(on, DebuggerMode::Disabled, fault);
        rig.forgo();
        rig.probe();
        assert_eq!(rig.plan_consent(), asked, "settled, never started");

        let mut rig = Rig::inherited(on, DebuggerMode::Disabled);
        assert_eq!(rig.plan_consent(), in_flight, "a predecessor's child runs");
        rig.forgo();

        // No admission of the window's own: parked, nothing written; the
        // owner's press is theirs still.
        let mut rig = Rig::unadmitted(on, DebuggerMode::Disabled, fault);
        rig.probe();
        assert_eq!(rig.plan_consent(), Plan::Parked);
        assert_eq!(rig.state["asked"], 0, "nothing recorded");
        rig.press(yes());
        rig.exit("ChildApplied");
        rig.notice();
        rig.probe();
        assert_eq!(rig.lane.flow.last(), Some(Outcome::Changed(on)));
    }

    #[cfg(unix)]
    #[test]
    fn debugger_access_conforms_to_the_model() {
        let _patience = patience_for_test(Duration::ZERO);
        scenarios(Fault::None);
    }

    #[cfg(unix)]
    #[test]
    fn the_harness_fails_every_faulty_composition() {
        let _patience = patience_for_test(Duration::ZERO);
        for fault in [
            Fault::SkipRecheck,
            Fault::IgnorePeerLock,
            Fault::PressWhileInFlight,
            Fault::ClaimFromExit,
            Fault::DropPollInFlight,
            Fault::RetryAfterCancel,
            Fault::AdoptLive,
            Fault::PlanConsentAgain,
            Fault::PlanConsentWhileInFlight,
            Fault::ConsentUnadmitted,
        ] {
            let caught = std::panic::catch_unwind(|| match fault {
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
                Fault::PlanConsentAgain => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.plan_consent();
                    rig.exit("ChildUnchanged");
                    rig.notice();
                    rig.probe();
                    let mut rig = rig.relaunch();
                    rig.probe();
                    rig.plan_consent();
                }
                Fault::PlanConsentWhileInFlight => {
                    let mut rig = Rig::new(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.press(OwnerAnswer::yes_for_test());
                    rig.plan_consent();
                }
                Fault::ConsentUnadmitted => {
                    let mut rig =
                        Rig::unadmitted(DebuggerChange::On, DebuggerMode::Disabled, fault);
                    rig.probe();
                    rig.plan_consent();
                }
                _ => scenarios(fault),
            });
            let caught_by = match fault {
                Fault::PlanConsentAgain => Some("PlanConsent broke AtMostOneOffer"),
                Fault::PlanConsentWhileInFlight => Some("PlanConsent broke NoOfferOverAChange"),
                Fault::ConsentUnadmitted => Some("PlanConsent broke ConsentOnlyWhenAdmitted"),
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
    action = "PlanConsentAgain",
    reason = "Buggy = 2 mutant: start_from_plan reads the asked record before it writes \
              it, and writes it before the yes is spent; the negative control starts over \
              a record it never reads, and the harness fails on AtMostOneOffer."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "PlanConsentWhileInFlight",
    reason = "Buggy = 2 mutant: start_from_plan is refused while a change is under way \
              (the press's own refusal, then a change marker or held lock on disk); the \
              negative control starts over our running child, and the harness fails on \
              NoOfferOverAChange."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "DebuggerAccessChange",
    action = "ConsentUnadmitted",
    reason = "Buggy = 2 mutant: start_from_plan takes its yes only from an AdmittedAccess, \
              which only the window's PermissionGate makes for its own admission; the \
              negative control starts the change in the Admitted = 0 scenario (a control \
              client's act, a status poll, a hand-edited record), and the harness fails on \
              ConsentOnlyWhenAdmitted."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn debugger_access_change_waivers() {}
