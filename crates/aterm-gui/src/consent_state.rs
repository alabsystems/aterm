// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App consent model: shared probe slots, `[privacy]` policy mirror, and session `status` fields.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};

use aterm_types::MutexExt as _;
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    self, Attribution, Claimants, ConsentPosture, DebuggerProbe, DebuggerProbeLabel, DevToolList,
    DevToolListLabel, DevToolProbe, DevToolProbeLabel, DrClass, FdaProbe, FdaState, FsConsent,
    PostureInputs, ProbeGate, ProbeLabel, Responsible, ResponsibleError, SelfHardening,
    SpikeEvidence,
};

use crate::signing_identity::{
    SigningIdentity, install_posture_once, running_bundle_id, running_bundle_id_if_warm,
    signing_identity_if_warm,
};
use crate::{App, Wake};

/// Injected OS consent probe functions (`live` for windowed instances, `inert` for headless/tests).
#[derive(Clone, Copy)]
pub(crate) struct ConsentProbes {
    /// Full Disk Access probe (`aterm __tcc-open`).
    pub(crate) fda: fn(ProbeGate) -> FdaProbe,
    /// `responsibility_get_pid_responsible_for_pid` query for each pid in order.
    pub(crate) responsible: fn(&[i32]) -> Vec<Result<i32, ResponsibleError>>,
    /// Bundles on disk claiming this process's `CFBundleIdentifier` and their DRs.
    pub(crate) census: fn(&str) -> Claimants,
    /// `DevToolsSecurity -status` and developer-group membership query.
    pub(crate) debugger: fn(ProbeGate) -> DebuggerProbe,
    /// Process code-signing hardening flags (`csops`).
    pub(crate) hardening: fn() -> SelfHardening,
    /// Bounded `/usr/bin/log show` of `tccd` answers for the given bundle id.
    pub(crate) devtools: fn(ProbeGate, &str) -> DevToolProbe,
    /// Read-only query of the subject's row in the system `TCC.db`.
    pub(crate) devtools_list: fn(ProbeGate, &str) -> DevToolList,
    /// `AXIsProcessTrusted` with no prompt option: aterm's own Accessibility grant, read
    /// in-process by the permission walk's live arm (`permission_walk::WalkReach::live`) behind
    /// the in-bundle fence ([`accessibility_with`]).
    pub(crate) accessibility: fn(ProbeGate) -> AxProbe,
    /// Running bundle's `CFBundleIdentifier` if already warm (`None` when cold).
    pub(crate) subject: fn() -> Option<&'static str>,
    /// Running bundle's `CFBundleIdentifier`, warming the identity if needed (worker-only).
    pub(crate) warm_subject: fn() -> Option<&'static str>,
    /// Whether these are live OS probes.
    pub(crate) live: bool,
}

fn inert_census(_bundle_id: &str) -> Claimants {
    Claimants::default()
}

const fn inert_debugger(_gate: ProbeGate) -> DebuggerProbe {
    DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled)
}

const fn inert_hardening() -> SelfHardening {
    SelfHardening::Unknown
}

const fn inert_devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
    DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
}

const fn inert_devtools_list(_gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(DevToolListLabel::RefusedDisabled)
}

const fn inert_accessibility(_gate: ProbeGate) -> AxProbe {
    AxProbe::RefusedDisabled
}

/// The permission walk's own live read of aterm's Accessibility grant, behind the in-bundle
/// fence of the other consent probes ([`accessibility_with`] over [`consent::fence_exe`]), so the
/// report and the walk read the grant through one arm. It raises no dialog.
fn live_accessibility(_gate: ProbeGate) -> AxProbe {
    accessibility_with(
        consent::fence_exe().as_deref(),
        crate::permission_walk::WalkReach::live().ax_trusted,
    )
}

/// The Accessibility read with the executable and the read injected. It calls `read` only when
/// `exe` lies inside a `.app`, as `consent::probe_fda_with` opens nothing outside one: macOS
/// answers `AXIsProcessTrusted` for the app it holds responsible for this process, which for an
/// executable outside a bundle is the program that started it (a terminal), not aterm.
pub(crate) fn accessibility_with(exe: Option<&Path>, read: impl FnOnce() -> bool) -> AxProbe {
    let Some(exe) = exe else {
        return AxProbe::RefusedNoExe;
    };
    if !consent::path_is_in_app_bundle(exe) {
        return AxProbe::RefusedOutOfBundle;
    }
    AxProbe::Read { trusted: read() }
}

/// One read of aterm's own Accessibility grant: the `accessibility` row of `privacy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxProbe {
    /// macOS answered `AXIsProcessTrusted`: whether this process holds the grant.
    Read {
        /// The answer.
        trusted: bool,
    },
    /// Not read: `[privacy] enabled` or `[privacy] check` is off, or the instance is headless.
    RefusedDisabled,
    /// Not read: the running executable could not be resolved.
    RefusedNoExe,
    /// Not read: the running executable is not inside a `.app` bundle, so macOS would answer
    /// for another app.
    RefusedOutOfBundle,
    /// Not macOS: there is no Accessibility grant here.
    UnsupportedPlatform,
}

impl AxProbe {
    /// The `grant=` token, in the `devtools` row's words: `unknown` when nothing was read.
    pub(crate) const fn grant(self) -> &'static str {
        match self {
            Self::Read { trusted: true } => "granted",
            Self::Read { trusted: false } => "not_granted",
            Self::RefusedDisabled
            | Self::RefusedNoExe
            | Self::RefusedOutOfBundle
            | Self::UnsupportedPlatform => "unknown",
        }
    }

    /// The `probe=` token, in the words of the other consent probes' labels.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::RefusedDisabled => "refused_disabled",
            Self::RefusedNoExe => "refused_no_exe",
            Self::RefusedOutOfBundle => "refused_out_of_bundle",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

impl ConsentProbes {
    /// Live probes for a windowed instance.
    pub(crate) const fn live() -> Self {
        Self {
            fda: consent::probe_fda,
            responsible: consent::responsible_pids_detailed,
            census: consent::claimants_for,
            debugger: consent::probe_debugger,
            hardening: consent::self_hardening,
            devtools: consent::probe_devtool,
            devtools_list: consent::probe_devtool_list,
            accessibility: live_accessibility,
            subject: running_bundle_id_if_warm,
            warm_subject: running_bundle_id,
            live: true,
        }
    }

    /// Syscall-free probes for headless instances and unit tests.
    pub(crate) const fn inert() -> Self {
        Self {
            fda: inert_fda_probe,
            responsible: inert_responsible,
            census: inert_census,
            debugger: inert_debugger,
            hardening: inert_hardening,
            devtools: inert_devtools,
            devtools_list: inert_devtools_list,
            accessibility: inert_accessibility,
            subject: running_bundle_id_if_warm,
            warm_subject: running_bundle_id_if_warm,
            live: false,
        }
    }

    /// Live probes on windowed macOS; inert when headless; unsupported off macOS.
    pub(crate) const fn for_instance(headless: bool) -> Self {
        if headless {
            Self::inert()
        } else if cfg!(target_os = "macos") {
            Self::live()
        } else {
            Self {
                fda: unsupported_fda_probe,
                responsible: inert_responsible,
                census: inert_census,
                debugger: unsupported_debugger_probe,
                hardening: inert_hardening,
                devtools: unsupported_devtools_probe,
                devtools_list: unsupported_devtools_list,
                accessibility: unsupported_accessibility,
                subject: running_bundle_id_if_warm,
                warm_subject: running_bundle_id_if_warm,
                live: false,
            }
        }
    }

    fn inactive_fda(self, gate: ProbeGate) -> FdaProbe {
        if !gate.permits() {
            inert_fda_probe(gate)
        } else {
            debug_assert!(!self.live);
            (self.fda)(gate)
        }
    }
}

const fn unsupported_fda_probe(gate: ProbeGate) -> FdaProbe {
    FdaProbe::refused(if gate.permits() {
        ProbeLabel::UnsupportedPlatform
    } else {
        ProbeLabel::RefusedDisabled
    })
}

const fn unsupported_debugger_probe(gate: ProbeGate) -> DebuggerProbe {
    DebuggerProbe::refused(if gate.permits() {
        DebuggerProbeLabel::UnsupportedPlatform
    } else {
        DebuggerProbeLabel::RefusedDisabled
    })
}

const fn unsupported_devtools_probe(gate: ProbeGate, _subject: &str) -> DevToolProbe {
    DevToolProbe::refused(if gate.permits() {
        DevToolProbeLabel::UnsupportedPlatform
    } else {
        DevToolProbeLabel::RefusedDisabled
    })
}

const fn unsupported_devtools_list(gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(if gate.permits() {
        DevToolListLabel::UnsupportedPlatform
    } else {
        DevToolListLabel::RefusedDisabled
    })
}

const fn unsupported_accessibility(gate: ProbeGate) -> AxProbe {
    if gate.permits() {
        AxProbe::UnsupportedPlatform
    } else {
        AxProbe::RefusedDisabled
    }
}

const fn inert_fda_probe(_gate: ProbeGate) -> FdaProbe {
    FdaProbe::refused(ProbeLabel::RefusedDisabled)
}

fn inert_responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
    vec![Err(ResponsibleError::Unsupported); pids.len()]
}

const PROBE_RETRY_FLOOR: Duration = Duration::from_millis(500);

/// Probe slot identifier for publications and GUI wakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotKind {
    /// The Full Disk Access grant.
    Fda,
    /// The claimant census.
    Census,
    /// The debugger rule.
    Debugger,
    /// The Developer Tools list.
    Devtools,
}

impl SlotKind {
    const fn change(self) -> ChangeKind {
        match self {
            Self::Fda => ChangeKind::Fda,
            Self::Census => ChangeKind::Census,
            Self::Debugger => ChangeKind::Debugger,
            Self::Devtools => ChangeKind::Devtools,
        }
    }

    /// Event-loop [`Wake`] posted when this slot's worker completes.
    pub(crate) const fn wake(self) -> Wake {
        match self {
            Self::Fda => Wake::ConsentProbeReady,
            Self::Census => Wake::ConsentCensusReady,
            Self::Debugger => Wake::DebuggerProbeReady,
            Self::Devtools => Wake::DevToolsProbeReady,
        }
    }
}

type SlotWake = Arc<dyn Fn(SlotKind) + Send + Sync>;

/// Policy for entries that have reached their TTL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stale {
    /// Serve the stale entry with its age while a worker refreshes it.
    Serve,
    /// Drop the expired entry and report pending until the refresh completes.
    Drop,
}

/// Lock acquisition mode for slot reads.
#[derive(Clone, Copy, Debug)]
enum SlotLock {
    /// Non-blocking `try_lock` for event-loop FDA reads; returns pending on contention.
    Try,
    /// Blocking lock for off-loop control workers and background slots.
    Wait,
}

/// Worker admission ticket carrying the epoch observed by the admitting read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    epoch: u64,
}

type SlotRead<E> = (Option<(E, Duration)>, Option<Ticket>);

/// A published answer, the epoch it was published under, and when.
#[derive(Debug)]
struct Entry<E> {
    value: E,
    epoch: u64,
    at: Instant,
}

/// What a slot's lock guards.
#[derive(Debug)]
struct Slot<E> {
    in_flight: bool,
    /// The epoch of the in-flight worker's ticket.
    request_epoch: u64,
    entry: Option<Entry<E>>,
    /// Earliest instant the next worker may start.
    retry_after: Option<Instant>,
}

struct SlotCell<E> {
    kind: SlotKind,
    stale: Stale,
    epoch: AtomicU64,
    /// `epoch + 1` of a ticket whose thread spawn failed (`0` when none).
    failed: AtomicU64,
    slot: Mutex<Slot<E>>,
    /// A test's step, run by the next read while it holds the lock.
    #[cfg(test)]
    held_read_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl<E> SlotCell<E> {
    const fn new(kind: SlotKind, stale: Stale) -> Self {
        Self {
            kind,
            stale,
            epoch: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            slot: Mutex::new(Slot {
                in_flight: false,
                request_epoch: 0,
                entry: None,
                retry_after: None,
            }),
            #[cfg(test)]
            held_read_hook: Mutex::new(None),
        }
    }

    fn try_hold(&self) -> Option<MutexGuard<'_, Slot<E>>> {
        match self.slot.try_lock() {
            Ok(slot) => Some(slot),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    fn restamp(&self, ticket: Ticket, lock: SlotLock) -> Ticket {
        let slot = match lock {
            SlotLock::Try => self.try_hold(),
            SlotLock::Wait => Some(self.slot.lock_or_recover()),
        };
        let Some(mut slot) = slot else {
            return ticket;
        };
        if !slot.in_flight || slot.request_epoch != ticket.epoch {
            return ticket;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        slot.request_epoch = epoch;
        Ticket { epoch }
    }

    fn complete(&self, ticket: Ticket, value: Option<E>, now: Instant) -> bool {
        let mut slot = self.slot.lock_or_recover();
        if !slot.in_flight || slot.request_epoch != ticket.epoch {
            return false;
        }
        slot.in_flight = false;
        if let Some(value) = value
            && ticket.epoch == self.epoch.load(Ordering::Acquire)
        {
            slot.entry = Some(Entry {
                value,
                epoch: ticket.epoch,
                at: now,
            });
        }
        true
    }

    fn retire(&self) -> bool {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.try_hold()
            .is_some_and(|mut slot| slot.entry.take().is_some())
    }

    fn clear_floor(&self) {
        self.slot.lock_or_recover().retry_after = None;
    }

    /// Hold `value` as the current answer, published at `at`.
    #[cfg(test)]
    fn seed_for_test(&self, value: E, at: Instant) {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.slot.lock_or_recover().entry = Some(Entry { value, epoch, at });
    }

    fn spawn_failed(&self, ticket: Ticket) {
        self.failed
            .store(ticket.epoch.wrapping_add(1), Ordering::Release);
    }

    fn next_refresh_deadline(&self, now: Instant, ttl: Duration) -> Instant {
        let ttl = ttl.max(PROBE_RETRY_FLOOR);
        let retry = now + ttl;
        let Some(slot) = self.try_hold() else {
            return retry;
        };
        if slot.in_flight {
            return retry;
        }
        if let Some(entry) = &slot.entry
            && entry.epoch == self.epoch.load(Ordering::Acquire)
            && let Some(expires) = entry.at.checked_add(ttl)
            && expires > now
        {
            return expires;
        }
        match slot.retry_after {
            Some(at) if at > now => at,
            _ => retry,
        }
    }
}

impl<E: Clone> SlotCell<E> {
    fn read(&self, now: Instant, ttl: Duration, admit: bool, lock: SlotLock) -> SlotRead<E> {
        loop {
            let (read, epoch) = self.read_once(now, ttl, admit, lock);
            if self.epoch.load(Ordering::Acquire) == epoch {
                return read;
            }
            match read {
                (_, Some(ticket)) => return (None, Some(self.restamp(ticket, lock))),
                (Some(_), None) => {}
                (None, None) => return read,
            }
        }
    }

    fn read_once(
        &self,
        now: Instant,
        ttl: Duration,
        admit: bool,
        lock: SlotLock,
    ) -> (SlotRead<E>, u64) {
        let ttl = ttl.max(PROBE_RETRY_FLOOR);
        let slot = match lock {
            SlotLock::Try => self.try_hold(),
            SlotLock::Wait => Some(self.slot.lock_or_recover()),
        };
        let Some(mut slot) = slot else {
            return ((None, None), 0);
        };
        let epoch = self.epoch.load(Ordering::Acquire);
        let read = if admit {
            self.admit(&mut slot, epoch, now, ttl)
        } else {
            (self.servable(&slot, epoch, now, ttl), None)
        };
        #[cfg(test)]
        {
            let hook = self.held_read_hook.lock_or_recover().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        drop(slot);
        (read, epoch)
    }

    fn servable(
        &self,
        slot: &Slot<E>,
        epoch: u64,
        now: Instant,
        ttl: Duration,
    ) -> Option<(E, Duration)> {
        let entry = slot.entry.as_ref().filter(|entry| entry.epoch == epoch)?;
        let age = now.saturating_duration_since(entry.at);
        (self.stale == Stale::Serve || age < ttl).then(|| (entry.value.clone(), age))
    }

    fn admit(&self, slot: &mut Slot<E>, epoch: u64, now: Instant, ttl: Duration) -> SlotRead<E> {
        let failed = self.failed.swap(0, Ordering::AcqRel);
        if failed != 0 && slot.in_flight && slot.request_epoch.wrapping_add(1) == failed {
            slot.in_flight = false;
            slot.retry_after = Some(now + ttl);
        }
        let answer = self.servable(slot, epoch, now, ttl);
        if answer.is_none() {
            slot.entry = None;
        }
        let fresh = answer.as_ref().is_some_and(|(_, age)| *age < ttl);
        let admitted = !fresh && !slot.in_flight && !slot.retry_after.is_some_and(|at| now < at);
        let ticket = admitted.then(|| {
            slot.in_flight = true;
            slot.request_epoch = epoch;
            slot.retry_after = Some(now + PROBE_RETRY_FLOOR);
            Ticket { epoch }
        });
        (answer, ticket)
    }
}

fn spawn_slot_worker<E: 'static>(
    shared: &Arc<SharedProbe>,
    cell: fn(&SharedProbe) -> &SlotCell<E>,
    ticket: Ticket,
    name: &str,
    work: impl FnOnce(&SharedProbe) -> Option<E> + Send + 'static,
) {
    let worker = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let value = work(&worker);
            worker.finish(cell(&worker), ticket, value, Instant::now());
        });
    if spawned.is_err() {
        cell(shared).spawn_failed(ticket);
    }
}

const CENSUS_TTL: Duration = Duration::from_secs(60);

/// The debugger slot's answer: the probe and aterm's own hardening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DebuggerEntry {
    probe: DebuggerProbe,
    self_hardened: SelfHardening,
}

const DEBUGGER_TTL: Duration = Duration::from_secs(60);

const DEVTOOLS_TTL: Duration = Duration::from_secs(120);

/// Cached Developer Tools probe and resolved bundle identifier.
#[derive(Clone, Copy, Debug)]
struct DevToolsEntry {
    probe: DevToolProbe,
    list: DevToolList,
    subject: Option<&'static str>,
}

/// Reported `devtools` row state and ages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DevToolsRow {
    pub(crate) probe: DevToolProbe,
    /// The switch's row in the system TCC.db (`list=`, `list_probe=`).
    pub(crate) list: DevToolList,
    /// The bundle id the row is about (`subject=`), when there is one.
    pub(crate) subject: Option<&'static str>,
    pub(crate) probe_age_ms: Option<u128>,
    pub(crate) answer_age_ms: Option<u64>,
}

impl DevToolsRow {
    /// Refused row with `label` and optional `subject`.
    pub(crate) const fn refused(label: DevToolProbeLabel, subject: Option<&'static str>) -> Self {
        Self {
            probe: DevToolProbe::refused(label),
            list: DevToolList::refused(match label {
                DevToolProbeLabel::Pending => DevToolListLabel::Pending,
                DevToolProbeLabel::RefusedObserverOff => DevToolListLabel::RefusedObserverOff,
                DevToolProbeLabel::RefusedNoSubject => DevToolListLabel::RefusedNoSubject,
                DevToolProbeLabel::RefusedOutOfBundle => DevToolListLabel::RefusedOutOfBundle,
                DevToolProbeLabel::RefusedNoExe => DevToolListLabel::RefusedNoExe,
                DevToolProbeLabel::UnsupportedPlatform => DevToolListLabel::UnsupportedPlatform,
                _ => DevToolListLabel::RefusedDisabled,
            }),
            subject,
            probe_age_ms: None,
            answer_age_ms: None,
        }
    }

    fn from_probe(
        probe: DevToolProbe,
        list: DevToolList,
        subject: Option<&'static str>,
        age: Duration,
        now_ms: i64,
    ) -> Self {
        let completed = !probe.label.refused() && probe.label != DevToolProbeLabel::Pending;
        Self {
            probe,
            list,
            subject,
            probe_age_ms: completed.then_some(age.as_millis()),
            answer_age_ms: probe
                .answered_at_ms
                .map(|at| consent::devtool_answer_age_ms(at, now_ms)),
        }
    }

    /// No answer yet: the probe is still out (or never dispatched).
    pub(crate) fn pending(&self) -> bool {
        self.probe.label == DevToolProbeLabel::Pending
    }

    /// The probe this row reports.
    pub(crate) const fn probe(&self) -> DevToolProbe {
        self.probe
    }

    /// The switch's row this row reports.
    pub(crate) const fn list(&self) -> DevToolList {
        self.list
    }

    /// The bundle id this row is about, when there is one.
    pub(crate) const fn subject(&self) -> Option<&'static str> {
        self.subject
    }

    /// The joined verdict (`grant=`, [`consent::devtool_grant`]).
    pub(crate) fn grant(&self) -> aterm_containment::DevToolGrant {
        consent::devtool_grant(&self.list, &self.probe)
    }

    /// How old tccd's newest answer was when this row was read.
    pub(crate) const fn answer_age_ms(&self) -> Option<u64> {
        self.answer_age_ms
    }
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Reported `debugger` row state, process hardening, and probe age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DebuggerRow {
    pub(crate) probe: DebuggerProbe,
    pub(crate) self_hardened: SelfHardening,
    pub(crate) probe_age_ms: Option<u128>,
}

impl DebuggerRow {
    /// Refused row with `label` and `Unknown` hardening.
    pub(crate) const fn refused(label: DebuggerProbeLabel) -> Self {
        Self {
            probe: DebuggerProbe::refused(label),
            self_hardened: SelfHardening::Unknown,
            probe_age_ms: None,
        }
    }

    /// Pending row preserving any previously read `self_hardened` state.
    pub(crate) const fn awaiting(self_hardened: SelfHardening) -> Self {
        Self {
            probe: DebuggerProbe::pending(),
            self_hardened,
            probe_age_ms: None,
        }
    }

    fn from_entry(entry: DebuggerEntry, age: Duration) -> Self {
        Self {
            probe: entry.probe,
            self_hardened: entry.self_hardened,
            probe_age_ms: completed_debugger_age_ms(entry.probe.label, age),
        }
    }

    /// No answer yet: the probe is still out (or never dispatched).
    pub(crate) fn pending(&self) -> bool {
        self.probe.label == DebuggerProbeLabel::Pending
    }

    /// The probe this row reports.
    pub(crate) const fn probe(&self) -> DebuggerProbe {
        self.probe
    }

    /// aterm's own hardening, as this row reports it.
    pub(crate) const fn self_hardened(&self) -> SelfHardening {
        self.self_hardened
    }
}

fn completed_debugger_age_ms(label: DebuggerProbeLabel, age: Duration) -> Option<u128> {
    (!label.refused() && label != DebuggerProbeLabel::Pending).then_some(age.as_millis())
}

/// Consent state aspect tracked by [`Publications`] counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    /// The Full Disk Access slot.
    Fda,
    /// The claimant census slot.
    Census,
    /// The debugger slot.
    Debugger,
    /// The Developer Tools slot.
    Devtools,
    /// The `[privacy]` mirror.
    Policy,
}

impl ChangeKind {
    const COUNT: usize = 5;
}

/// The change counters at one instant ([`ConsentState::publication`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Publication([u64; ChangeKind::COUNT]);

/// Condition-variable notification counters for off-loop consent waiters.
#[derive(Default)]
struct Publications {
    seq: [AtomicU64; ChangeKind::COUNT],
    lock: Mutex<()>,
    changed: Condvar,
}

impl Publications {
    fn publish(&self, kind: ChangeKind) {
        self.seq[kind as usize].fetch_add(1, Ordering::Release);
        drop(self.lock.lock_or_recover());
        self.changed.notify_all();
    }

    fn snapshot(&self) -> Publication {
        Publication(std::array::from_fn(|kind| {
            self.seq[kind].load(Ordering::Acquire)
        }))
    }

    fn changed_since(&self, since: Publication, watch: &[ChangeKind]) -> bool {
        watch
            .iter()
            .any(|&kind| self.seq[kind as usize].load(Ordering::Acquire) != since.0[kind as usize])
    }

    fn wait_for_change(&self, since: Publication, watch: &[ChangeKind], timeout: Duration) -> bool {
        let parked = self.lock.lock_or_recover();
        drop(
            self.changed
                .wait_timeout_while(parked, timeout, |()| !self.changed_since(since, watch))
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.changed_since(since, watch)
    }
}

struct SharedProbe {
    /// Published `[privacy]` policy for the current config generation.
    policy: Mutex<Arc<ConsentPolicy>>,
    /// Change counters and condvar for off-loop waiters.
    publications: Publications,
    fda: SlotCell<FdaProbe>,
    census: SlotCell<Claimants>,
    debugger: SlotCell<DebuggerEntry>,
    devtools: SlotCell<DevToolsEntry>,
    /// GUI event-loop completion callback.
    wake: OnceLock<SlotWake>,
    /// Cached process hardening bits, read once by the first debugger worker.
    hardening: OnceLock<SelfHardening>,
}

impl SharedProbe {
    fn new(policy: ConsentPolicy) -> Self {
        Self {
            policy: Mutex::new(Arc::new(policy)),
            publications: Publications::default(),
            fda: SlotCell::new(SlotKind::Fda, Stale::Drop),
            census: SlotCell::new(SlotKind::Census, Stale::Serve),
            debugger: SlotCell::new(SlotKind::Debugger, Stale::Serve),
            devtools: SlotCell::new(SlotKind::Devtools, Stale::Serve),
            wake: OnceLock::new(),
            hardening: OnceLock::new(),
        }
    }

    fn published_policy(&self) -> Arc<ConsentPolicy> {
        Arc::clone(&self.policy.lock_or_recover())
    }

    fn finish<E>(&self, cell: &SlotCell<E>, ticket: Ticket, value: Option<E>, now: Instant) {
        if cell.complete(ticket, value, now) {
            self.publications.publish(cell.kind.change());
            if let Some(wake) = self.wake.get() {
                wake(cell.kind);
            }
        }
    }
}

/// Instance-owned, non-blocking consent state shared between the GUI and control workers.
#[derive(Clone)]
pub(crate) struct ConsentState {
    probes: ConsentProbes,
    shared: Arc<SharedProbe>,
}

impl ConsentState {
    /// Create consent state initialized with `policy`.
    pub(crate) fn new(headless: bool, policy: ConsentPolicy) -> Self {
        Self {
            probes: ConsentProbes::for_instance(headless),
            shared: Arc::new(SharedProbe::new(policy)),
        }
    }

    pub(crate) fn inert() -> Self {
        Self::new(
            true,
            ConsentPolicy::from_config(&crate::app_config::Config::default()),
        )
    }

    /// Latest published [`ConsentPolicy`].
    pub(crate) fn policy(&self) -> Arc<ConsentPolicy> {
        self.shared.published_policy()
    }

    /// Publish a new [`ConsentPolicy`], retiring the FDA slot if the probe gate changed.
    pub(crate) fn publish_policy(&self, policy: ConsentPolicy) {
        let retired = {
            let mut published = self.shared.policy.lock_or_recover();
            if **published == policy {
                return;
            }
            let retired = published.gate != policy.gate && self.shared.fda.retire();
            *published = Arc::new(policy);
            retired
        };
        self.shared.publications.publish(ChangeKind::Policy);
        if retired {
            self.shared.publications.publish(ChangeKind::Fda);
        }
    }

    /// Snapshot current change counters before reading state.
    pub(crate) fn publication(&self) -> Publication {
        self.shared.publications.snapshot()
    }

    /// Wait until any kind in `watch` publishes after `since` or `timeout` elapses.
    pub(crate) fn wait_for_change(
        &self,
        since: Publication,
        watch: &[ChangeKind],
        timeout: Duration,
    ) -> bool {
        self.shared
            .publications
            .wait_for_change(since, watch, timeout)
    }

    /// Register the GUI event-loop completion callback.
    pub(crate) fn set_slot_wake(&self, wake: SlotWake) {
        let _ = self.shared.wake.set(wake);
    }

    fn retire<E>(&self, cell: &SlotCell<E>) {
        if cell.retire() {
            self.shared.publications.publish(cell.kind.change());
        }
    }

    /// Retire cached or in-flight FDA answers while keeping the single-flight slot and floor.
    pub(crate) fn invalidate(&self) {
        self.retire(&self.shared.fda);
    }

    /// Retire the FDA answer and clear its retry floor: a look owed a fresh read now
    /// (`consent_card::SettleLook`) is not held back by a read made just before it, which would
    /// leave the look pending with no answer to wake it.
    pub(crate) fn refresh_fda(&self) {
        self.retire(&self.shared.fda);
        self.shared.fda.clear_floor();
    }

    /// As if the FDA and Developer Tools slots had each sent a read just now: their retry floors
    /// hold a minute (test-only).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn hold_retry_floors_for_test(&self) {
        let until = Some(Instant::now() + Duration::from_secs(60));
        self.shared.fda.slot.lock_or_recover().retry_after = until;
        self.shared.devtools.slot.lock_or_recover().retry_after = until;
    }

    /// The census slot's epoch, for the host's wake test.
    #[cfg(test)]
    pub(crate) fn census_generation_for_test(&self) -> u64 {
        self.shared.census.epoch.load(Ordering::Acquire)
    }

    /// Retire the census slot and clear its retry floor after bundles on disk change.
    pub(crate) fn invalidate_census(&self) {
        self.retire(&self.shared.census);
        self.shared.census.clear_floor();
    }

    /// Read the cached claimant census and dispatch a refresh worker when due.
    pub(crate) fn claimants(
        &self,
        bundle_id: Option<&str>,
        enabled: bool,
    ) -> (Option<(Claimants, Duration)>, bool) {
        let Some(bundle_id) = bundle_id.filter(|_| enabled && self.probes.live) else {
            return (None, false);
        };
        let (answer, ticket) =
            self.shared
                .census
                .read(Instant::now(), CENSUS_TTL, true, SlotLock::Wait);
        if let Some(ticket) = ticket {
            let census = self.probes.census;
            let id = bundle_id.to_owned();
            spawn_slot_worker(
                &self.shared,
                |shared| &shared.census,
                ticket,
                "aterm-claimant-census",
                move |shared| shared.published_policy().enabled.then(|| census(&id)),
            );
        }
        let pending = answer.is_none();
        (answer, pending)
    }

    /// Retire the debugger slot while preserving its in-flight bit and retry floor.
    pub(crate) fn invalidate_debugger(&self) {
        self.retire(&self.shared.debugger);
    }

    /// Read the debugger row and dispatch a refresh worker when due.
    pub(crate) fn debugger(&self, gate: ProbeGate) -> DebuggerRow {
        if !gate.permits() {
            self.invalidate_debugger();
            return DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled);
        }
        if !self.probes.live {
            return self.inert_debugger_row(gate);
        }
        let (answer, ticket) =
            self.shared
                .debugger
                .read(Instant::now(), DEBUGGER_TTL, true, SlotLock::Wait);
        let known = self
            .shared
            .hardening
            .get()
            .copied()
            .unwrap_or(SelfHardening::Unknown);
        if let Some(ticket) = ticket {
            let probe = self.probes.debugger;
            let hardening = self.probes.hardening;
            spawn_slot_worker(
                &self.shared,
                |shared| &shared.debugger,
                ticket,
                "aterm-debugger-probe",
                move |shared| {
                    let gate = shared.published_policy().gate;
                    if !gate.permits() {
                        return None;
                    }
                    crate::qos::set_self(crate::qos::Role::Responsive);
                    let probe = probe(gate);
                    let self_hardened = *shared.hardening.get_or_init(hardening);
                    Some(DebuggerEntry {
                        probe,
                        self_hardened,
                    })
                },
            );
        }
        answer.map_or(DebuggerRow::awaiting(known), |(entry, age)| {
            DebuggerRow::from_entry(entry, age)
        })
    }

    /// Read the cached debugger row without dispatching a worker.
    pub(crate) fn debugger_cached(&self, gate: ProbeGate) -> Option<DebuggerRow> {
        if !gate.permits() {
            return Some(DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled));
        }
        if !self.probes.live {
            return Some(self.inert_debugger_row(gate));
        }
        self.shared
            .debugger
            .read(Instant::now(), DEBUGGER_TTL, false, SlotLock::Wait)
            .0
            .map(|(entry, age)| DebuggerRow::from_entry(entry, age))
    }

    fn inert_debugger_row(&self, gate: ProbeGate) -> DebuggerRow {
        DebuggerRow {
            probe: (self.probes.debugger)(gate),
            self_hardened: (self.probes.hardening)(),
            probe_age_ms: None,
        }
    }

    /// Retire the Developer Tools slot while preserving its in-flight bit and retry floor.
    pub(crate) fn invalidate_devtools(&self) {
        self.retire(&self.shared.devtools);
    }

    /// Retire the Developer Tools slot and clear its retry floor: a fact its reading rests on
    /// changed (Full Disk Access came on), so the next read is made at once, not refused for a
    /// read made just before the change, whose answer the retirement threw away.
    pub(crate) fn refresh_devtools(&self) {
        self.retire(&self.shared.devtools);
        self.shared.devtools.clear_floor();
    }

    /// Read the Developer Tools row and dispatch a refresh worker when due.
    pub(crate) fn devtools(&self, gate: ProbeGate, observer_off: bool) -> DevToolsRow {
        self.devtools_read(gate, observer_off, true)
    }

    /// Read the cached Developer Tools row without dispatching a worker.
    pub(crate) fn devtools_cached(&self, gate: ProbeGate, observer_off: bool) -> DevToolsRow {
        self.devtools_read(gate, observer_off, false)
    }

    fn devtools_read(&self, gate: ProbeGate, observer_off: bool, dispatch: bool) -> DevToolsRow {
        let known = (self.probes.subject)();
        if !gate.permits() {
            self.invalidate_devtools();
            return DevToolsRow::refused(DevToolProbeLabel::RefusedDisabled, known);
        }
        if observer_off {
            self.invalidate_devtools();
            return DevToolsRow::refused(DevToolProbeLabel::RefusedObserverOff, known);
        }
        if !self.probes.live {
            return DevToolsRow::from_probe(
                (self.probes.devtools)(gate, known.unwrap_or_default()),
                (self.probes.devtools_list)(gate, known.unwrap_or_default()),
                known,
                Duration::ZERO,
                unix_now_ms(),
            );
        }
        if known.is_some_and(|id| !consent::devtool_subject_is_usable(id)) {
            return DevToolsRow::refused(DevToolProbeLabel::RefusedNoSubject, known);
        }
        let (answer, ticket) =
            self.shared
                .devtools
                .read(Instant::now(), DEVTOOLS_TTL, dispatch, SlotLock::Wait);
        if let Some(ticket) = ticket {
            let probe = self.probes.devtools;
            let read_list = self.probes.devtools_list;
            let warm_subject = self.probes.warm_subject;
            spawn_slot_worker(
                &self.shared,
                |shared| &shared.devtools,
                ticket,
                "aterm-devtools-probe",
                move |shared| {
                    let policy = shared.published_policy();
                    if !policy.gate.permits() || policy.observer_off {
                        return None;
                    }
                    let gate = policy.gate;
                    let subject = known.or_else(warm_subject);
                    let (read, list) =
                        match subject.filter(|id| consent::devtool_subject_is_usable(id)) {
                            Some(id) => {
                                let list = read_list(gate, id);
                                (probe(gate, id), list)
                            }
                            None => (
                                DevToolProbe::refused(DevToolProbeLabel::RefusedNoSubject),
                                DevToolList::refused(DevToolListLabel::RefusedNoSubject),
                            ),
                        };
                    Some(DevToolsEntry {
                        probe: read,
                        list,
                        subject,
                    })
                },
            );
        }
        answer.map_or(
            DevToolsRow::refused(DevToolProbeLabel::Pending, known),
            |(entry, age)| {
                DevToolsRow::from_probe(entry.probe, entry.list, entry.subject, age, unix_now_ms())
            },
        )
    }

    /// Configure a headless test instance with injected Developer Tools probe and subject.
    #[cfg(test)]
    pub(crate) fn set_devtools_arms_for_test(
        &mut self,
        devtools: fn(ProbeGate, &str) -> DevToolProbe,
        subject: fn() -> Option<&'static str>,
    ) {
        self.probes.live = true;
        self.probes.devtools = devtools;
        self.probes.subject = subject;
    }

    /// Configure a headless test instance with an injected Full Disk Access probe function.
    #[cfg(test)]
    pub(crate) fn set_fda_arm_for_test(&mut self, fda: fn(ProbeGate) -> FdaProbe) {
        self.probes.live = true;
        self.probes.fda = fda;
    }

    /// No Developer Tools worker out and no answer held.
    #[cfg(test)]
    pub(crate) fn devtools_slot_idle_for_test(&self) -> bool {
        let slot = self.shared.devtools.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    }

    /// No Full Disk Access worker out and no answer held.
    #[cfg(test)]
    pub(crate) fn fda_slot_idle_for_test(&self) -> bool {
        let slot = self.shared.fda.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    }

    /// Mutable reference to probe functions for tests.
    #[cfg(test)]
    pub(crate) fn probes_for_test(&mut self) -> &mut ConsentProbes {
        &mut self.probes
    }

    /// Complete the FDA worker `ticket` with `probe` via [`SharedProbe::finish`].
    #[cfg(test)]
    pub(crate) fn complete_fda_for_test(&self, ticket: Ticket, probe: FdaProbe, at: Instant) {
        self.shared
            .finish(&self.shared.fda, ticket, Some(probe), at);
    }

    /// The ticket of the Full Disk Access worker out, if one is.
    #[cfg(test)]
    pub(crate) fn fda_in_flight_for_test(&self) -> Option<Ticket> {
        let slot = self.shared.fda.slot.lock_or_recover();
        slot.in_flight.then_some(Ticket {
            epoch: slot.request_epoch,
        })
    }

    /// Next FDA refresh deadline after `now` (`None` on an inert instance).
    pub(crate) fn next_refresh_deadline(
        &self,
        now: Instant,
        interval: Duration,
    ) -> Option<Instant> {
        self.probes
            .live
            .then(|| self.shared.fda.next_refresh_deadline(now, interval))
    }

    /// Next debugger refresh deadline after `now` (`None` on an inert instance).
    pub(crate) fn next_debugger_refresh_deadline(&self, now: Instant) -> Option<Instant> {
        self.probes.live.then(|| {
            self.shared
                .debugger
                .next_refresh_deadline(now, DEBUGGER_TTL)
        })
    }

    fn fda_at(
        &self,
        gate: ProbeGate,
        interval: Duration,
        now: Instant,
        admit: bool,
        lock: SlotLock,
    ) -> ((FdaProbe, Duration), Option<Ticket>) {
        if !gate.permits() || !self.probes.live {
            if admit {
                self.invalidate();
            }
            return ((self.probes.inactive_fda(gate), Duration::ZERO), None);
        }
        let (answer, ticket) = self.shared.fda.read(now, interval, admit, lock);
        (
            answer.unwrap_or((FdaProbe::pending(), Duration::ZERO)),
            ticket,
        )
    }

    /// Test seam for [`Self::fda_at`] using [`SlotLock::Try`].
    #[cfg(test)]
    pub(crate) fn read_at(
        &self,
        gate: ProbeGate,
        interval: Duration,
        now: Instant,
    ) -> ((FdaProbe, Duration), Option<Ticket>) {
        self.fda_at(gate, interval, now, true, SlotLock::Try)
    }

    /// [`Self::fda_cached`] at a given instant.
    #[cfg(test)]
    fn cached_at(&self, gate: ProbeGate, interval: Duration, now: Instant) -> (FdaProbe, Duration) {
        self.fda_at(gate, interval, now, false, SlotLock::Try).0
    }

    /// The FDA probe's answer no older than `interval`, dispatching a fresh probe when it is
    /// (the Security page's cadence, or the permission walk's quarter second).
    pub(crate) fn fda(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, true, SlotLock::Try)
    }

    /// Blocking off-loop FDA read for control workers ([`SlotLock::Wait`]).
    pub(crate) fn fda_off_loop(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, true, SlotLock::Wait)
    }

    fn fda_cached(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, false, SlotLock::Try)
    }

    /// Observation instant of a fresh cached FDA grant at `now`.
    pub(crate) fn fda_grant_observed_at(
        &self,
        gate: ProbeGate,
        interval: Duration,
    ) -> Option<Instant> {
        let now = Instant::now();
        let ((probe, age), _) = self.fda_at(gate, interval, now, false, SlotLock::Try);
        if probe.state != FdaState::Granted {
            return None;
        }
        now.checked_sub(age)
    }

    fn fda_read(
        &self,
        gate: ProbeGate,
        interval: Duration,
        admit: bool,
        lock: SlotLock,
    ) -> (FdaProbe, Duration) {
        let (answer, ticket) = self.fda_at(gate, interval, Instant::now(), admit, lock);
        if let Some(ticket) = ticket {
            let probe = self.probes.fda;
            spawn_slot_worker(
                &self.shared,
                |shared| &shared.fda,
                ticket,
                "aterm-consent-probe",
                move |shared| {
                    let gate = shared.published_policy().gate;
                    gate.permits().then(|| probe(gate))
                },
            );
        }
        answer
    }

    pub(crate) fn responsible_answers(&self, pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
        (self.probes.responsible)(pids)
    }

    /// Read aterm's own Accessibility grant on the calling thread: one call of the arm while
    /// the gate permits, none while it does not. Nothing is cached, since a read is one
    /// in-process call that raises no dialog.
    pub(crate) fn accessibility(&self, gate: ProbeGate) -> AxProbe {
        if gate.permits() {
            (self.probes.accessibility)(gate)
        } else {
            AxProbe::RefusedDisabled
        }
    }

    /// Read all four consent slots for the full privacy report, dispatching workers as needed.
    pub(crate) fn report_slots(
        &self,
        policy: &ConsentPolicy,
        identity: &SigningIdentity,
    ) -> SlotFacts {
        SlotFacts {
            fda: self.fda_off_loop(policy.gate, policy.interval),
            census: self.claimants(identity.bundle_id.as_deref(), policy.enabled),
            debugger: self.debugger(policy.gate),
            devtools: self.devtools(policy.gate, policy.observer_off),
        }
    }

    /// Whether macOS attributes Full Disk Access for this process to aterm rather than a parent.
    pub(crate) fn window_reads_own_grant(&self) -> bool {
        let me = i32::try_from(std::process::id()).unwrap_or(-1);
        let answer = self
            .responsible_answers(&[me])
            .pop()
            .unwrap_or(Err(ResponsibleError::Unsupported));
        !matches!(
            consent::classify_responsible(me, answer),
            Responsible::Other(_)
        )
    }

    /// Test instance whose responsibility probe returns `answer`.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn set_responsible_for_test(
        &mut self,
        answer: fn(&[i32]) -> Vec<Result<i32, ResponsibleError>>,
    ) {
        self.probes.responsible = answer;
    }

    /// Seed a synthetic cached FDA verdict for tests.
    #[cfg(test)]
    pub(crate) fn with_cached_probe_for_test(probe: FdaProbe) -> Self {
        let mut state = Self::inert();
        state.probes.live = true;
        let now = Instant::now();
        let (_, ticket) = state.read_at(ProbeGate::on(), Duration::from_secs(5), now);
        state.shared.finish(
            &state.shared.fda,
            ticket.expect("fresh request"),
            Some(probe),
            now,
        );
        state
    }
}

impl Default for ConsentState {
    fn default() -> Self {
        Self::inert()
    }
}

impl std::fmt::Debug for ConsentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let empty = self
            .shared
            .fda
            .try_hold()
            .is_none_or(|slot| slot.entry.is_none());
        f.debug_struct("ConsentState")
            .field("probes_live", &self.probes.live)
            .field("cache_empty", &empty)
            .finish()
    }
}

/// Snapshot of the four probe slots read by [`ConsentState::report_slots`].
#[derive(Clone, Debug)]
pub(crate) struct SlotFacts {
    /// The Full Disk Access probe and its age.
    pub(crate) fda: (FdaProbe, Duration),
    /// The census answer with its age, and whether none exists yet.
    pub(crate) census: (Option<(Claimants, Duration)>, bool),
    pub(crate) debugger: DebuggerRow,
    pub(crate) devtools: DevToolsRow,
}

impl SlotFacts {
    /// [`ChangeKind`]s of slots still waiting on their initial result.
    pub(crate) fn pending_kinds(&self) -> Vec<ChangeKind> {
        [
            (self.fda.0.label == ProbeLabel::Pending, ChangeKind::Fda),
            (self.census.1, ChangeKind::Census),
            (self.debugger.pending(), ChangeKind::Debugger),
            (self.devtools.pending(), ChangeKind::Devtools),
        ]
        .into_iter()
        .filter_map(|(pending, kind)| pending.then_some(kind))
        .collect()
    }

    pub(crate) fn pending(&self) -> bool {
        !self.pending_kinds().is_empty()
    }
}

impl App {
    /// Assemble [`ConsentPanelFacts`] for the Security page, dispatching an FDA probe if due.
    pub(crate) fn consent_panel_facts(&self) -> ConsentPanelFacts {
        self.panel_facts(true)
    }

    /// Assemble [`ConsentPanelFacts`] without dispatching an FDA probe.
    pub(crate) fn consent_panel_facts_cached(&self) -> ConsentPanelFacts {
        self.panel_facts(false)
    }

    fn panel_facts(&self, dispatch: bool) -> ConsentPanelFacts {
        let identity = signing_identity_if_warm();
        let enabled = self.config.privacy_enabled();
        let gate = self.config.privacy_probe_gate();
        let interval = Duration::from_millis(self.config.privacy_probe_interval_ms());
        let (probe, _age) = if dispatch {
            self.consent.fda(gate, interval)
        } else {
            self.consent.fda_cached(gate, interval)
        };
        let (install, running) = install_posture_once();
        let sessions_total = self.pool.iter().count();
        let sessions_adopted = self
            .pool
            .iter()
            .filter(|session| {
                enabled && matches!(session_attribution(session), Attribution::Adopted)
            })
            .count();
        ConsentPanelFacts {
            enabled,
            fda: probe.state,
            probe: probe.label,
            dr: identity.dr,
            bundle_id: identity.bundle_id.clone(),
            reset_subject: identity.reset_subject.clone(),
            install,
            running: running.clone(),
            sessions_total,
            sessions_adopted,
        }
    }
}

/// Consent posture and session summary rendered by the Security settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConsentPanelFacts {
    /// Whether `[privacy] enabled` is true.
    pub(crate) enabled: bool,
    /// The cached Full Disk Access verdict.
    pub(crate) fda: FdaState,
    /// Probe outcome label.
    pub(crate) probe: ProbeLabel,
    /// Designated requirement class for a grant.
    pub(crate) dr: DrClass,
    /// Running bundle's `CFBundleIdentifier`.
    pub(crate) bundle_id: Option<String>,
    /// Reset subject for the repair plan.
    pub(crate) reset_subject: Option<consent::ResetSubject>,
    /// Classified install posture token.
    pub(crate) install: &'static str,
    /// Canonical executable path.
    pub(crate) running: Option<String>,
    /// Total live sessions.
    pub(crate) sessions_total: usize,
    pub(crate) sessions_adopted: usize,
}

/// Per-session consent fields reported in `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionConsent {
    /// Session adoption attribution.
    pub(crate) attribution: Attribution,
    /// Filesystem consent state for this session.
    pub(crate) fs_consent: FsConsent,
    /// Whether `consent_at_risk` is emitted in `reasons=`.
    pub(crate) at_risk: bool,
}

impl SessionConsent {
    const fn unknown() -> Self {
        Self {
            attribution: Attribution::Unknown,
            fs_consent: FsConsent::Unknown,
            at_risk: false,
        }
    }
}

/// Resolved `[privacy]` policy published per config generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConsentPolicy {
    /// Combined `enabled` and `check` probe gate.
    pub(crate) gate: ProbeGate,
    /// `probe_interval_ms`.
    pub(crate) interval: Duration,
    /// `report_attribution`, under the master switch.
    pub(crate) report_attribution: bool,
    /// The master switch.
    pub(crate) enabled: bool,
    /// Whether `observer = false` disables Developer Tools log probing.
    pub(crate) observer_off: bool,
    /// Resolved `protected_roots` paths.
    protected_roots: Arc<[PathBuf]>,
}

impl ConsentPolicy {
    /// Resolve `config`'s `[privacy]` policy and protected roots.
    pub(crate) fn from_config(config: &crate::app_config::Config) -> Self {
        Self::over_roots(config, config.privacy_protected_roots().into())
    }

    fn over_roots(config: &crate::app_config::Config, protected_roots: Arc<[PathBuf]>) -> Self {
        Self {
            gate: config.privacy_probe_gate(),
            interval: Duration::from_millis(config.privacy_probe_interval_ms()),
            report_attribution: config.privacy_enabled() && config.privacy_report_attribution(),
            enabled: config.privacy_enabled(),
            observer_off: config.privacy_observer_off(),
            protected_roots,
        }
    }

    /// Resolved protected root paths.
    pub(crate) fn protected_roots(&self) -> &[PathBuf] {
        &self.protected_roots
    }

    /// Return `observed` when `[privacy] enabled` is true, or [`Attribution::Unknown`] when off.
    pub(crate) const fn adoption(&self, observed: Attribution) -> Attribution {
        if self.enabled {
            observed
        } else {
            Attribution::Unknown
        }
    }
}

/// Compute a session's [`Attribution`] and [`FsConsent`] under `policy`.
pub(crate) const fn session_posture(
    policy: &ConsentPolicy,
    observed: Attribution,
    fda: FdaState,
) -> ConsentPosture {
    ConsentPosture::join(PostureInputs {
        adoption: policy.adoption(observed),
        fda,
        responsible: Responsible::Unknown,
        observed_eperm: false,
        evidence: SpikeEvidence::MEASURED,
    })
}

impl App {
    /// Current `[privacy]` policy using the last published protected roots.
    pub(crate) fn consent_policy(&self) -> ConsentPolicy {
        ConsentPolicy::over_roots(
            &self.config,
            Arc::clone(&self.consent.policy().protected_roots),
        )
    }

    /// Republish `[privacy]` policy after a config reload.
    pub(crate) fn reconcile_consent_config(&self) {
        self.consent
            .publish_policy(ConsentPolicy::from_config(&self.config));
        crate::occupancy_wakes::set_privacy(self.config.privacy_protected_roots());
    }

    /// Set `config.privacy` and republish its mirror for tests.
    #[cfg(test)]
    pub(crate) fn set_privacy_for_test(&mut self, privacy: crate::app_config::PrivacyConfig) {
        self.config.privacy = Some(privacy);
        self.reconcile_consent_config();
    }

    /// Next consent probe refresh deadline for active UI observers.
    pub(crate) fn next_consent_probe_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.config.privacy_probe_gate().permits() {
            return None;
        }
        self.consent.next_refresh_deadline(
            now,
            Duration::from_millis(self.config.privacy_probe_interval_ms()),
        )
    }

    /// Non-blocking consent fields for `session`'s `status` record.
    pub(crate) fn session_consent(&self, session: u64, cwd: Option<&str>) -> SessionConsent {
        let Some(pooled) = self.pool.get(session) else {
            return SessionConsent::unknown();
        };
        let policy = self.consent_policy();
        let (probe, _age) = self.consent.fda_cached(policy.gate, policy.interval);
        let posture = session_posture(&policy, session_attribution(pooled), probe.state);
        SessionConsent {
            attribution: posture.attribution,
            fs_consent: posture.fs_consent,
            at_risk: consent_at_risk(posture.fs_consent, cwd, policy.protected_roots()),
        }
    }
}

/// Whether `fs_consent` is not [`FsConsent::Covered`] and `cwd` lies under `protected_roots`.
pub(crate) fn consent_at_risk(
    fs_consent: FsConsent,
    cwd: Option<&str>,
    protected: &[PathBuf],
) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    !matches!(fs_consent, FsConsent::Covered)
        && consent::is_under_protected_root(Path::new(cwd), protected)
}

/// Session attribution from `session`'s handoff adoption record.
pub(crate) const fn session_attribution(session: &crate::Session) -> Attribution {
    if session.handoff_local_id.is_some() {
        Attribution::Adopted
    } else {
        Attribution::Live
    }
}

#[cfg(test)]
#[path = "consent_state_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "consent_state_asynchronous_probe_tests.rs"]
mod asynchronous_probe_tests;
