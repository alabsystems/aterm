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

    /// Time since the probe completed (`None` while pending, refused, or inert).
    pub(crate) fn probe_age(&self) -> Option<Duration> {
        self.probe_age_ms
            .map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX)))
    }

    /// Time since a read that brought no reading ended: `-status` timed out, did not start, or
    /// answered what the classifier does not claim (`None` for a reading, and for a row
    /// [`Self::probe_age`] gives no age).
    pub(crate) fn unread_age(&self) -> Option<Duration> {
        self.probe_age()
            .filter(|_| self.probe.mode == consent::DebuggerMode::Unknown)
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

    /// Publish a completed debugger result on a test instance, read as just taken until
    /// [`aterm_spec::wait::MUST_HAPPEN`] has passed. The debugger step ages a reading by the
    /// wall clock against [`crate::debugger_access::ASK_PROBE_FRESH`] while its test moves the
    /// step's own clock: stamped now, the looks after it would have five seconds to spend it, a
    /// latency budget that a look's durable write, queued behind every other test's under load,
    /// outruns.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn publish_debugger_for_test(
        &mut self,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
    ) {
        let at = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        self.publish_debugger_at_for_test(probe, self_hardened, at);
    }

    /// Publish a completed debugger result aged by `age` on a test instance.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn publish_debugger_aged_for_test(
        &mut self,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
        age: Duration,
    ) {
        let at = Instant::now()
            .checked_sub(age)
            .expect("a monotonic clock that far back");
        self.publish_debugger_at_for_test(probe, self_hardened, at);
    }

    #[cfg(all(test, target_os = "macos"))]
    fn publish_debugger_at_for_test(
        &mut self,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
        at: Instant,
    ) {
        self.probes.live = true;
        let _ = self.shared.hardening.set(self_hardened);
        self.shared.debugger.seed_for_test(
            DebuggerEntry {
                probe,
                self_hardened,
            },
            at,
        );
    }

    /// Whether no debugger worker is in flight and no entry is cached.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn debugger_slot_idle_for_test(&self) -> bool {
        let slot = self.shared.debugger.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    }

    /// A debugger worker is out.
    pub(crate) fn debugger_in_flight(&self) -> bool {
        self.shared.debugger.slot.lock_or_recover().in_flight
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
mod tests {
    use aterm_containment::consent::{DebuggerMember, DebuggerMode};

    use super::*;
    use crate::control::Scope;
    use crate::control_privacy::{
        ConsentObservation, ConsentTuple, ConsentWait, ConsentWaitDecision, observe_debugger,
        observe_fda,
    };
    use crate::privacy_report::PrivacyForm;
    fn wake_on(kind: SlotKind, tx: std::sync::mpsc::Sender<()>) -> SlotWake {
        Arc::new(move |published| {
            if published == kind {
                let _ = tx.send(());
            }
        })
    }

    fn census_answer(found: Vec<aterm_containment::consent::Claimant>) -> Claimants {
        Claimants {
            found,
            enumeration: aterm_containment::consent::Enumeration::Complete,
        }
    }

    #[test]
    fn the_census_runs_on_a_worker_and_survives_focus_and_config_invalidation() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting_census(_id: &str) -> Claimants {
            RUNS.fetch_add(1, Ordering::AcqRel);
            census_answer(Vec::new())
        }
        RUNS.store(0, Ordering::Release);
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.census = counting_census;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Census, tx));
        let wait = || {
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the worker publishes")
        };

        let (answer, pending) = state.claimants(Some("x"), true);
        assert!(
            answer.is_none() && pending,
            "the reading thread never waits"
        );
        wait();
        let (answer, pending) = state.claimants(Some("x"), true);
        assert!(answer.is_some() && !pending);
        assert_eq!(RUNS.load(Ordering::Acquire), 1);

        state.invalidate();
        let (answer, _) = state.claimants(Some("x"), true);
        assert!(
            answer.is_some(),
            "focus and config changes do not change the disk"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

        state.invalidate_census();
        let (answer, pending) = state.claimants(Some("x"), true);
        assert!(answer.is_none() && pending);
        wait();
        assert!(state.claimants(Some("x"), true).0.is_some());
        assert_eq!(RUNS.load(Ordering::Acquire), 2);
    }

    #[test]
    fn a_census_out_when_the_disk_changes_publishes_nothing() {
        use std::sync::atomic::AtomicBool;
        static RELEASE: AtomicBool = AtomicBool::new(false);
        fn held(_id: &str) -> Claimants {
            while !RELEASE.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            census_answer(Vec::new())
        }
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.census = held;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Census, tx));
        let wait = || {
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the worker publishes")
        };
        assert_eq!(state.claimants(Some("x"), true), (None, true));
        state.invalidate_census();
        RELEASE.store(true, Ordering::Release);
        wait();
        let (answer, pending) = state.claimants(Some("x"), true);
        assert!(answer.is_none() && pending, "the stale answer is dropped");
        wait();
        assert!(state.claimants(Some("x"), true).0.is_some());
    }

    #[test]
    fn the_census_is_off_without_an_id_a_live_instance_or_the_master_switch() {
        static RAN: AtomicU64 = AtomicU64::new(0);
        fn never(_id: &str) -> Claimants {
            RAN.fetch_add(1, Ordering::AcqRel);
            Claimants::default()
        }
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.census = never;
        assert_eq!(state.claimants(None, true), (None, false));
        assert_eq!(state.claimants(Some("x"), false), (None, false));
        let mut inert = ConsentState::inert();
        inert.probes.census = never;
        assert_eq!(inert.claimants(Some("x"), true), (None, false));
        assert_eq!(RAN.load(Ordering::Acquire), 0);
    }

    #[test]
    fn consent_at_risk_is_a_conjunction_and_is_silent_without_a_cwd() {
        let roots = vec![PathBuf::from("/u/me/Protected")];
        let inside = Some("/u/me/Protected/work");
        assert!(consent_at_risk(FsConsent::Unknown, inside, &roots));
        assert!(consent_at_risk(FsConsent::Denied, inside, &roots));
        assert!(!consent_at_risk(FsConsent::Covered, inside, &roots));
        assert!(!consent_at_risk(
            FsConsent::Unknown,
            Some("/u/me/src"),
            &roots
        ));
        for verdict in [FsConsent::Unknown, FsConsent::Denied, FsConsent::Covered] {
            assert!(!consent_at_risk(verdict, None, &roots));
        }
    }

    fn fake_debugger(runs: &std::sync::atomic::AtomicUsize) -> DebuggerProbe {
        runs.fetch_add(1, Ordering::AcqRel);
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

    #[test]
    fn the_debugger_probe_runs_on_a_worker_and_focus_gain_retires_it() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&RUNS)
        }
        fn hardened() -> SelfHardening {
            HARDENING_READS.fetch_add(1, Ordering::AcqRel);
            SelfHardening::NotHardened
        }
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.debugger = counting;
        state.probes.hardening = hardened;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Debugger, tx));
        let wait = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };

        let cold = state.debugger(ProbeGate::on());
        assert_eq!(cold, DebuggerRow::awaiting(SelfHardening::Unknown));
        assert!(cold.pending(), "the reading thread never waits");
        wait();
        let warm = state.debugger(ProbeGate::on());
        assert_eq!(warm.probe.mode, DebuggerMode::Disabled);
        assert_eq!(warm.probe.member, DebuggerMember::Admin);
        assert_eq!(warm.self_hardened, SelfHardening::NotHardened);
        assert!(warm.probe_age_ms.is_some(), "a completed probe has an age");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);

        state.invalidate();
        assert!(!state.debugger(ProbeGate::on()).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

        state.invalidate_debugger();
        state.shared.debugger.clear_floor();
        assert!(state.debugger(ProbeGate::on()).pending());
        wait();
        assert!(!state.debugger(ProbeGate::on()).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 2);
        assert_eq!(
            HARDENING_READS.load(Ordering::Acquire),
            1,
            "the hardening bits are fixed at exec: read once"
        );
    }

    #[test]
    fn a_pending_debugger_row_keeps_the_hardening_already_read() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&RUNS)
        }
        fn hardened() -> SelfHardening {
            SelfHardening::Hardened
        }
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.debugger = counting;
        state.probes.hardening = hardened;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Debugger, tx));
        let wait = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };

        let cold = state.debugger(ProbeGate::on());
        assert!(cold.pending());
        assert_eq!(
            cold.self_hardened(),
            SelfHardening::Unknown,
            "nothing read it"
        );
        wait();
        assert_eq!(
            state.debugger(ProbeGate::on()).self_hardened(),
            SelfHardening::Hardened
        );

        state.invalidate_debugger();
        state.shared.debugger.clear_floor();
        let retired = state.debugger(ProbeGate::on());
        assert_eq!(retired, DebuggerRow::awaiting(SelfHardening::Hardened));
        assert!(retired.pending(), "the rule itself is read again");
        wait();
        assert_eq!(RUNS.load(Ordering::Acquire), 2);

        let closed = ProbeGate {
            enabled: false,
            check: true,
        };
        assert_eq!(
            state.debugger(closed).self_hardened(),
            SelfHardening::Unknown,
            "a closed gate reports nothing, the hardening included"
        );
    }

    #[test]
    fn the_master_switch_makes_the_debugger_row_unknown() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&RUNS)
        }
        fn hardened() -> SelfHardening {
            HARDENING_READS.fetch_add(1, Ordering::AcqRel);
            SelfHardening::Hardened
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.debugger = counting;
        app.consent.probes.hardening = hardened;
        app.set_privacy_for_test(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let lines = app
            .privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
            .0;
        let debugger = lines
            .iter()
            .find(|l| l.starts_with("debugger "))
            .expect("the posture carries a debugger row");
        assert!(
            debugger.starts_with("debugger mode=unknown ")
                && debugger.contains(" probe=refused_disabled "),
            "{debugger}"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 0, "nothing was spawned");
        assert_eq!(HARDENING_READS.load(Ordering::Acquire), 0);

        let state = &app.consent;
        state.shared.debugger.seed_for_test(
            DebuggerEntry {
                probe: fake_debugger(&AtomicUsize::new(0)),
                self_hardened: SelfHardening::Hardened,
            },
            Instant::now(),
        );
        let closed = ProbeGate {
            enabled: false,
            check: true,
        };
        assert_eq!(
            state.debugger(closed),
            DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled)
        );
        assert!(
            state.shared.debugger.slot.lock_or_recover().entry.is_none(),
            "the closed gate retired the warm answer"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_polled_consent_paths_never_dispatch_the_debugger_probe() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&RUNS)
        }
        fn hardened() -> SelfHardening {
            HARDENING_READS.fetch_add(1, Ordering::AcqRel);
            SelfHardening::Hardened
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.debugger = counting;
        app.consent.probes.hardening = hardened;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_slot_wake(wake_on(SlotKind::Debugger, tx));
        assert!(
            app.consent_policy().gate.permits(),
            "the gate is open, so only the path can keep the probe quiet"
        );
        let slot_idle = |app: &crate::App| {
            let slot = app.consent.shared.debugger.slot.lock_or_recover();
            !slot.in_flight && slot.entry.is_none()
        };
        for _ in 0..3 {
            let _ = observe_fda(&app.consent, Attribution::Live, Instant::now());
            let _ = app.session_consent(0, None);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 0, "a poll spawned the probe");
        assert_eq!(HARDENING_READS.load(Ordering::Acquire), 0);
        assert!(slot_idle(&app), "a poll left a debugger worker in flight");

        let _ = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the full report's worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert!(!slot_idle(&app), "the full report's answer is cached");
    }

    #[test]
    fn status_reads_the_fda_cache_and_never_dispatches_the_probe() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> FdaProbe {
            RUNS.fetch_add(1, Ordering::AcqRel);
            consent::classify_probe(consent::ProbeOutcome::Ok)
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.fda = counting;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_slot_wake(wake_on(SlotKind::Fda, tx));
        app.set_privacy_for_test(crate::app_config::PrivacyConfig {
            probe_interval_ms: Some(3_600_000),
            ..Default::default()
        });
        let policy = app.consent_policy();
        assert!(
            policy.gate.permits(),
            "the gate is open, so only the path can keep the probe quiet"
        );
        assert!(app.pool.get(0).is_some(), "the read reaches the cache");
        let slot_idle = |app: &crate::App| {
            let slot = app.consent.shared.fda.slot.lock_or_recover();
            !slot.in_flight && slot.entry.is_none() && slot.retry_after.is_none()
        };

        for _ in 0..3 {
            let _ = app.session_consent(0, None);
        }
        assert_eq!(
            RUNS.load(Ordering::Acquire),
            0,
            "a status read spawned the probe"
        );
        assert!(slot_idle(&app), "a status read claimed the worker");

        let _ = app.consent.fda(policy.gate, policy.interval);
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the dispatching read's worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        for _ in 0..3 {
            let _ = app.session_consent(0, None);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "a status read refreshed");
        assert_eq!(
            app.consent.fda_cached(policy.gate, policy.interval).0,
            consent::classify_probe(consent::ProbeOutcome::Ok),
            "the passive read serves the published verdict"
        );
    }

    fn fake_devtools(runs: &std::sync::atomic::AtomicUsize, at_ms: i64) -> DevToolProbe {
        runs.fetch_add(1, Ordering::AcqRel);
        DevToolProbe {
            state: aterm_containment::DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(3),
            not_allowed: Some(3),
            answered_at_ms: Some(at_ms),
        }
    }

    fn fake_subject() -> Option<&'static str> {
        Some("com.example.subject")
    }

    #[test]
    fn the_devtools_probe_runs_on_a_worker_and_focus_gain_retires_it() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static SUBJECT_SEEN: Mutex<Option<String>> = Mutex::new(None);
        fn counting(_gate: ProbeGate, subject: &str) -> DevToolProbe {
            *SUBJECT_SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(subject.to_owned());
            fake_devtools(&RUNS, unix_now_ms() - 120_000)
        }
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state.probes.devtools = counting;
        state.probes.subject = fake_subject;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Devtools, tx));
        let wait = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };

        let cold = state.devtools(ProbeGate::on(), false);
        assert!(cold.pending(), "the reading thread never waits");
        assert_eq!(cold.probe.state, aterm_containment::DevToolState::Unknown);
        wait();
        let warm = state.devtools(ProbeGate::on(), false);
        assert_eq!(warm.probe.state, aterm_containment::DevToolState::Unlisted);
        assert_eq!(warm.probe.requests, Some(3));
        assert!(warm.probe_age_ms.is_some(), "a completed probe has an age");
        let answer_age = warm.answer_age_ms.expect("tccd's answer has an age");
        assert!(
            answer_age >= 120_000,
            "aged against the wall clock: {answer_age}"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert_eq!(
            SUBJECT_SEEN
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_deref(),
            Some("com.example.subject"),
            "the worker asks about the subject arm's bundle id"
        );

        state.invalidate();
        state.invalidate_debugger();
        assert!(!state.devtools(ProbeGate::on(), false).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "served, not rerun");

        // A read made just before: its retry floor still holds.
        state.shared.devtools.slot.lock_or_recover().retry_after =
            Some(Instant::now() + Duration::from_secs(60));
        state.invalidate_devtools();
        assert!(state.devtools(ProbeGate::on(), false).pending());
        assert!(
            state.devtools_slot_idle_for_test(),
            "the control: retired inside the floor, no read is sent and nothing will answer"
        );
        state.refresh_devtools();
        assert!(state.devtools(ProbeGate::on(), false).pending());
        wait();
        assert!(!state.devtools(ProbeGate::on(), false).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 2, "a refresh reads at once");

        {
            let mut slot = state.shared.devtools.slot.lock_or_recover();
            let entry = slot.entry.as_mut().expect("a cached answer");
            entry.at = Instant::now()
                .checked_sub(DEVTOOLS_TTL + Duration::from_secs(1))
                .expect("the clock is past one TTL");
            slot.retry_after = None;
        }
        for _ in 0..5 {
            assert!(!state.devtools(ProbeGate::on(), false).pending());
        }
        wait();
        assert_eq!(RUNS.load(Ordering::Acquire), 3, "one refresh per TTL");
    }

    #[test]
    fn the_master_switch_and_observer_off_make_the_devtools_row_unknown() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            fake_devtools(&RUNS, 0)
        }
        fn no_subject() -> Option<&'static str> {
            None
        }
        fn unusable_subject() -> Option<&'static str> {
            Some("a b")
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.devtools = counting;
        app.consent.probes.subject = fake_subject;
        let row = |app: &crate::App| {
            app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
                .0
                .into_iter()
                .find(|l| l.starts_with("devtools "))
                .expect("the posture carries a devtools row")
        };
        let refused = |app: &crate::App, label: &str| {
            let row = row(app);
            assert!(
                row.starts_with("devtools state=unknown ")
                    && row.contains(&format!(" probe={label} "))
                    && row.contains(&format!(" list_probe={label} ")),
                "{row}"
            );
        };
        let warm = |app: &crate::App| {
            app.consent.shared.devtools.seed_for_test(
                DevToolsEntry {
                    probe: fake_devtools(&AtomicUsize::new(0), 0),
                    list: DevToolList::pending(),
                    subject: fake_subject(),
                },
                Instant::now(),
            );
        };
        let cold = |app: &crate::App| {
            app.consent
                .shared
                .devtools
                .slot
                .lock_or_recover()
                .entry
                .is_none()
        };

        for privacy in [
            crate::app_config::PrivacyConfig {
                enabled: Some(false),
                ..Default::default()
            },
            crate::app_config::PrivacyConfig {
                check: Some(false),
                ..Default::default()
            },
        ] {
            app.set_privacy_for_test(privacy);
            warm(&app);
            refused(&app, "refused_disabled");
            assert!(cold(&app), "the closed gate retired the warm answer");
        }
        app.set_privacy_for_test(crate::app_config::PrivacyConfig {
            observer: Some(false),
            ..Default::default()
        });
        warm(&app);
        refused(&app, "refused_observer_off");
        assert!(cold(&app), "observer = false retired the warm answer");
        app.set_privacy_for_test(crate::app_config::PrivacyConfig::default());
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_slot_wake(wake_on(SlotKind::Devtools, tx));
        app.consent.probes.subject = unusable_subject;
        assert!(
            row(&app).contains(" probe=refused_no_subject "),
            "{}",
            row(&app)
        );
        assert!(
            app.consent.devtools_slot_idle_for_test(),
            "no worker was dispatched for it"
        );
        app.consent.probes.subject = no_subject;
        app.consent.probes.warm_subject = no_subject;
        assert!(row(&app).contains(" probe=pending "), "{}", row(&app));
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes");
        assert!(
            row(&app).contains(" subject=- ") && row(&app).contains(" probe=refused_no_subject "),
            "{}",
            row(&app)
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 0, "nothing was spawned");

        app.consent.probes.subject = fake_subject;
        app.consent.invalidate_devtools();
        app.consent.shared.devtools.clear_floor();
        assert!(row(&app).contains(" probe=pending "), "{}", row(&app));
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert!(row(&app).contains(" probe=observed "), "{}", row(&app));
    }

    #[test]
    fn a_cold_identity_is_read_on_the_devtools_worker() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static WARMS: AtomicUsize = AtomicUsize::new(0);
        static SUBJECT_SEEN: Mutex<Option<String>> = Mutex::new(None);
        fn counting(_gate: ProbeGate, subject: &str) -> DevToolProbe {
            *SUBJECT_SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(subject.to_owned());
            fake_devtools(&RUNS, 0)
        }
        fn cold_identity() -> Option<&'static str> {
            None
        }
        fn warming() -> Option<&'static str> {
            WARMS.fetch_add(1, Ordering::AcqRel);
            fake_subject()
        }
        let mut state = ConsentState::inert();
        state.set_devtools_arms_for_test(counting, cold_identity);
        state.probes.warm_subject = warming;
        let (tx, rx) = std::sync::mpsc::channel();
        state.set_slot_wake(wake_on(SlotKind::Devtools, tx));
        let wait = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };

        let first = state.devtools(ProbeGate::on(), false);
        assert!(first.pending(), "a cold identity is not refused: {first:?}");
        assert_eq!(first.subject, None);
        wait();
        let warm = state.devtools(ProbeGate::on(), false);
        assert_eq!(warm.probe.label, DevToolProbeLabel::Observed);
        assert_eq!(
            warm.subject,
            Some("com.example.subject"),
            "the row names what the worker asked about"
        );
        assert_eq!(
            SUBJECT_SEEN
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_deref(),
            Some("com.example.subject")
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert_eq!(WARMS.load(Ordering::Acquire), 1);
    }

    #[test]
    fn the_polled_consent_paths_never_dispatch_the_devtools_probe() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            fake_devtools(&RUNS, 0)
        }
        fn quiet_debugger(_gate: ProbeGate) -> DebuggerProbe {
            DebuggerProbe::refused(DebuggerProbeLabel::StatusUnrecognized)
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.devtools = counting;
        app.consent.probes.debugger = quiet_debugger;
        app.consent.probes.subject = fake_subject;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_slot_wake(wake_on(SlotKind::Devtools, tx));
        assert!(app.consent_policy().gate.permits());
        let slot_idle = |app: &crate::App| {
            let slot = app.consent.shared.devtools.slot.lock_or_recover();
            !slot.in_flight && slot.entry.is_none()
        };
        for _ in 0..3 {
            let _ = observe_fda(&app.consent, Attribution::Live, Instant::now());
            let _ = observe_debugger(&app.consent, Instant::now());
            let _ = app.session_consent(0, None);
            let _ = app.consent.debugger_cached(ProbeGate::on());
            let _ = app.consent.devtools_cached(ProbeGate::on(), false);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 0, "a poll spawned the probe");
        assert!(slot_idle(&app), "a poll left a devtools worker in flight");

        let _ = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the full report's worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert!(!slot_idle(&app), "the full report's answer is cached");
    }

    #[test]
    fn await_consent_kind_debugger_latches_on_a_mode_change_without_a_probe_per_poll() {
        use std::sync::atomic::{AtomicU8, AtomicUsize};
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static MODE: AtomicU8 = AtomicU8::new(0);
        fn fake_status(_gate: ProbeGate) -> DebuggerProbe {
            RUNS.fetch_add(1, Ordering::AcqRel);
            DebuggerProbe {
                mode: if MODE.load(Ordering::Acquire) == 0 {
                    DebuggerMode::Disabled
                } else {
                    DebuggerMode::Enabled
                },
                member: DebuggerMember::Admin,
                label: DebuggerProbeLabel::StatusOk,
            }
        }
        fn hardened() -> SelfHardening {
            SelfHardening::Hardened
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.debugger = fake_status;
        app.consent.probes.hardening = hardened;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_slot_wake(wake_on(SlotKind::Debugger, tx));
        let published = || {
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the worker publishes")
        };
        let clear_floor = |app: &crate::App| {
            app.consent.shared.debugger.clear_floor();
        };
        let observe = |app: &crate::App| observe_debugger(&app.consent, Instant::now()).0;
        let mode = |mode| ConsentObservation::Complete(ConsentTuple::Debugger { mode });
        let fda_seq = |app: &crate::App| app.consent.publication().0[ChangeKind::Fda as usize];
        let fda_before = fda_seq(&app);
        let now = Instant::now();
        let mut wait = ConsentWait::new(now, Duration::from_secs(300));

        assert_eq!(observe(&app), ConsentObservation::Pending);
        assert_eq!(
            wait.observe_at(ConsentObservation::Pending, now),
            ConsentWaitDecision::Wait
        );
        published();
        let baseline = observe(&app);
        assert_eq!(baseline, mode(DebuggerMode::Disabled));
        assert_eq!(wait.observe_at(baseline, now), ConsentWaitDecision::Wait);
        MODE.store(1, Ordering::Release);
        for _ in 0..20 {
            clear_floor(&app);
            let (same, due) = observe_debugger(&app.consent, Instant::now());
            let expires = app
                .consent
                .shared
                .debugger
                .slot
                .lock_or_recover()
                .entry
                .as_ref()
                .map(|entry| entry.at + DEBUGGER_TTL);
            assert_eq!(due, expires, "the refresh is due when the answer expires");
            assert_eq!(wait.observe_at(same, now), ConsentWaitDecision::Wait);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "a read spawned the probe");

        app.consent.invalidate_debugger();
        clear_floor(&app);
        let retired = observe(&app);
        assert_eq!(retired, ConsentObservation::Pending);
        assert_eq!(wait.observe_at(retired, now), ConsentWaitDecision::Wait);
        published();
        let fresh = observe(&app);
        assert_eq!(
            wait.observe_at(fresh, now),
            ConsentWaitDecision::Changed(ConsentTuple::Debugger {
                mode: DebuggerMode::Enabled
            })
        );
        assert_eq!(
            ConsentTuple::Debugger {
                mode: DebuggerMode::Enabled
            }
            .to_string(),
            "debugger_mode=enabled"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 2);

        {
            let mut slot = app.consent.shared.debugger.slot.lock_or_recover();
            let entry = slot.entry.as_mut().expect("a cached answer");
            entry.at = Instant::now()
                .checked_sub(DEBUGGER_TTL + Duration::from_secs(1))
                .expect("the clock is past one TTL");
            slot.retry_after = None;
        }
        for _ in 0..5 {
            assert_eq!(
                observe(&app),
                mode(DebuggerMode::Enabled),
                "a stale answer is served while it refreshes"
            );
        }
        published();
        assert_eq!(RUNS.load(Ordering::Acquire), 3, "one refresh per TTL");
        assert_eq!(
            fda_seq(&app),
            fda_before,
            "debugger churn published no Full Disk Access change"
        );
    }

    #[test]
    fn a_slot_refresh_is_always_due_after_now() {
        let ttl = DEBUGGER_TTL;
        let second = Duration::from_secs(1);
        let t0 = Instant::now();
        let cell = SlotCell::<u8>::new(SlotKind::Debugger, Stale::Serve);
        assert_eq!(
            cell.next_refresh_deadline(t0, ttl),
            t0 + ttl,
            "nothing held"
        );
        let ticket = cell
            .read(t0, ttl, true, SlotLock::Try)
            .1
            .expect("a cold read admits");
        assert_eq!(
            cell.next_refresh_deadline(t0, ttl),
            t0 + ttl,
            "a worker is out"
        );
        let t1 = t0 + second;
        assert!(cell.complete(ticket, Some(1), t1));
        assert_eq!(
            cell.next_refresh_deadline(t1 + second, ttl),
            t1 + ttl,
            "the answer's expiry"
        );

        let stale = t1 + ttl * 2;
        let (served, refresh) = cell.read(stale, ttl, true, SlotLock::Try);
        assert!(
            served.is_some() && refresh.is_some(),
            "stale: served, refreshed"
        );
        assert!(t1 + ttl <= stale, "the naive rule's expiry is in the past");
        assert_eq!(
            cell.next_refresh_deadline(stale, ttl),
            stale + ttl,
            "never the past expiry"
        );

        cell.spawn_failed(refresh.expect("admitted"));
        assert_eq!(
            cell.read(stale, ttl, true, SlotLock::Try).1,
            None,
            "a refused worker backs off"
        );
        assert_eq!(
            cell.next_refresh_deadline(stale + second, ttl),
            stale + ttl,
            "by one TTL"
        );
        assert!(cell.retire(), "a held answer is forgotten");
        assert!(!cell.retire(), "a second retirement forgets nothing");
        assert_eq!(
            cell.next_refresh_deadline(stale + second, ttl),
            stale + ttl,
            "the floor survives the retirement"
        );
        assert_eq!(
            cell.next_refresh_deadline(stale + ttl, ttl),
            stale + ttl * 2,
            "past the floor, one TTL on"
        );
    }

    #[test]
    fn await_consent_kind_debugger_never_latches_without_a_reading() {
        let app = crate::App::headless_for_test();
        assert!(
            !app.consent.probes.live,
            "a headless app has the inert arms"
        );
        for _ in 0..3 {
            assert_eq!(
                observe_debugger(&app.consent, Instant::now()),
                (ConsentObservation::Pending, None)
            );
        }
        let mut closed = crate::App::headless_for_test();
        closed.consent.probes.live = true;
        closed.set_privacy_for_test(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        });
        assert_eq!(
            observe_debugger(&closed.consent, Instant::now()),
            (ConsentObservation::Pending, None)
        );
        let slot = closed.consent.shared.debugger.slot.lock_or_recover();
        assert!(
            slot.entry.is_none() && !slot.in_flight,
            "a closed gate dispatches nothing"
        );
    }

    #[test]
    fn the_master_switch_makes_every_consent_field_unknown() {
        let mut app = crate::App::headless_for_test();
        assert_eq!(app.session_consent(0, None).attribution, Attribution::Live);
        assert_eq!(
            app.session_consent(4242, None).attribution,
            Attribution::Unknown
        );
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .handoff_local_id = Some(3);
        assert_eq!(
            app.session_consent(0, None).attribution,
            Attribution::Adopted,
            "with the lane ON the adoption record shows"
        );

        let panel = app.consent_panel_facts();
        assert!(panel.enabled);
        assert_eq!((panel.sessions_total, panel.sessions_adopted), (1, 1));

        app.set_privacy_for_test(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let off = app.session_consent(0, None);
        let panel_off = app.consent_panel_facts();
        assert!(!panel_off.enabled);
        assert_eq!(panel_off.probe, ProbeLabel::RefusedDisabled);
        assert_eq!(panel_off.fda, FdaState::Unknown);
        assert_eq!(
            (panel_off.sessions_total, panel_off.sessions_adopted),
            (1, 0)
        );
        assert_eq!(
            (panel_off.install, panel_off.running),
            (panel.install, panel.running)
        );
        assert_eq!(
            off.attribution,
            Attribution::Unknown,
            "the switch outranks the record"
        );
        assert_eq!(off.fs_consent, FsConsent::Unknown);
        assert!(!off.at_risk);

        let lines = app
            .privacy_report_for_test(PrivacyForm::Lines, Scope::Owner)
            .0;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("full_disk_access=unknown") && l.contains("probe=")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("attribution_root=unknown")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("=denied")),
            "a switched-off lane never reads as a denial: {lines:?}"
        );
    }

    /// A config reload's `[privacy] protected_roots` reach the wake files' admission too
    /// (`reconcile_consent_config`), which guards them beside its fixed set. NEGATIVE CONTROL:
    /// before the reload the root is not among them.
    #[test]
    fn a_config_reload_sets_the_wake_files_guarded_roots() {
        use crate::app_colorscheme::install_resolved_colors::apply_config_text;
        let root = PathBuf::from(if cfg!(windows) {
            "C:/opt/aterm-wake-privacy"
        } else {
            "/opt/aterm-wake-privacy"
        });
        let privacy = format!(
            "[privacy]\ncheck = false\nprotected_roots = [\"{}\"]\n",
            root.display()
        );
        let dir = aterm_tempfile::Builder::new()
            .prefix("wake-privacy")
            .tempdir()
            .expect("a scratch folder");
        let mut app = crate::App::headless_for_test();
        assert!(
            crate::occupancy_wakes::privacy().is_none_or(|roots| !roots.contains(&root)),
            "NEGATIVE CONTROL: not before the reload"
        );
        apply_config_text(&mut app, dir.path(), &privacy);
        assert!(
            crate::occupancy_wakes::privacy().is_some_and(|roots| roots.contains(&root)),
            "{:?}",
            crate::occupancy_wakes::privacy()
        );
    }

    #[test]
    fn a_config_reload_republishes_the_privacy_mirror() {
        use crate::app_colorscheme::install_resolved_colors::apply_config_text;
        let root = format!(
            "{}/aterm-privacy-mirror",
            if cfg!(windows) { "C:/tmp" } else { "/tmp" }
        );
        let privacy = format!("[privacy]\ncheck = false\nprotected_roots = [\"{root}\"]\n");
        let config: crate::app_config::Config = aterm_toml::from_str(&privacy).expect("valid toml");
        let expected = ConsentPolicy::from_config(&config);
        assert_eq!(expected.protected_roots(), [PathBuf::from(&root)]);
        let cwd = format!("{root}/repo");
        let dir = std::env::temp_dir().join(format!("aterm-privacy-mirror-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let mut app = crate::App::headless_for_test();
        assert_ne!(*app.consent.policy(), expected, "precondition");
        assert!(!app.session_consent(0, Some(&cwd)).at_risk, "precondition");

        apply_config_text(&mut app, &dir, &privacy);
        assert_eq!(*app.consent.policy(), expected);
        assert!(
            app.session_consent(0, Some(&cwd)).at_risk,
            "status judges the cwd against the republished roots"
        );

        let published = app.consent.policy();
        apply_config_text(
            &mut app,
            &dir,
            &format!("{privacy}[claude_footer]\nusage = false\n"),
        );
        assert_eq!(
            app.config.claude_footer.as_ref().and_then(|f| f.usage),
            Some(false),
            "the unrelated generation was admitted"
        );
        assert!(
            Arc::ptr_eq(&published, &app.consent.policy()),
            "an equal section keeps the published mirror"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod asynchronous_probe_tests {
    use aterm_containment::consent::DebuggerMode;

    use super::*;
    use std::sync::mpsc;

    fn synthetic_state() -> ConsentState {
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state
    }

    fn grant() -> FdaProbe {
        consent::classify_probe(consent::ProbeOutcome::Ok)
    }

    fn complete(state: &ConsentState, ticket: Ticket, probe: FdaProbe, at: Instant) {
        state
            .shared
            .finish(&state.shared.fda, ticket, Some(probe), at);
    }

    fn debugger_entry() -> DebuggerEntry {
        DebuggerEntry {
            probe: DebuggerProbe::refused(DebuggerProbeLabel::TimedOut),
            self_hardened: SelfHardening::Unknown,
        }
    }

    fn devtools_entry() -> DevToolsEntry {
        DevToolsEntry {
            probe: DevToolProbe::refused(DevToolProbeLabel::TimedOut),
            list: DevToolList::refused(DevToolListLabel::TimedOut),
            subject: None,
        }
    }

    fn probe_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AsyncConsentProbePublication {
                const Buggy = 0;
                const Serve = 0;
                var epoch = 0;
                var request_epoch = 0;
                var flight = 0;
                var cached = 0;
                var cached_epoch = 0;
                var ready = 1;
                action Start when (flight == 0 && (cached == 0 || Serve == 1) && ready == 1) {
                    flight = 1;
                    request_epoch = epoch;
                    ready = 0;
                }
                action Cooldown when (ready == 0) {
                    ready = 1;
                }
                action Invalidate when (epoch <= 1) {
                    epoch = epoch + 1;
                    cached = 0;
                    cached_epoch = 0;
                }
                action Complete when (flight == 1) {
                    flight = 0;
                    cached = if request_epoch == epoch || Buggy == 1 { 1 } else { 0 };
                    cached_epoch = if request_epoch == epoch || Buggy == 1 { request_epoch } else { 0 };
                }
                action Expire when (cached == 1) {
                    cached = 0;
                    cached_epoch = 0;
                }
                invariant Bounds: epoch <= 2 && request_epoch <= 2 && flight <= 1 && cached <= 1 && ready <= 1;
                invariant CurrentPublication: cached == 0 || cached_epoch == epoch;
                invariant OneWorkerOrCache: Serve == 1 || flight + cached <= 1;
            }
        }
    }

    fn probe_model_serving(serve: bool) -> aterm_spec::derive::Model {
        aterm_spec::interp::with_consts(&probe_model(), &[("Serve", i64::from(serve))])
    }

    #[test]
    fn async_probe_model_proves_and_catches_stale_publication() {
        for serve in [false, true] {
            let model = probe_model_serving(serve);
            aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        }
    }

    fn admission_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AsyncConsentProbeAdmission {
                const Buggy = 0;
                var flight = 0;
                var ready = 1;
                var elapsed = 1;
                action Start when (flight == 0 && ready == 1) {
                    flight = 1;
                    ready = 0;
                    elapsed = 0;
                }
                action Complete when (flight == 1) { flight = 0; }
                action Invalidate when (ready == 0) {
                    ready = if Buggy == 1 { 1 } else { 0 };
                }
                action Cooldown when (elapsed == 0) {
                    ready = 1;
                    elapsed = 1;
                }
                action Forget when (ready == 0) {
                    ready = 1;
                    elapsed = 1;
                }
                invariant FloorSurvivesInvalidation: ready == 0 || elapsed == 1;
                invariant Bounds: flight <= 1 && ready <= 1 && elapsed <= 1;
            }
        }
    }

    #[test]
    fn admission_model_proves_and_catches_focus_churn_bypassing_the_floor() {
        let model = admission_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn focus_churn_conforms_to_the_admission_floor() {
        fn churn<E: Clone>(cell: &SlotCell<E>, value: E, forget: Option<&dyn Fn()>) {
            let model = admission_model();
            let now = Instant::now();
            let ttl = Duration::from_secs(5);
            let mut expected = model.init_state();
            let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
            assert!(model.fire("Start", &mut expected));
            assert!(cell.complete(ticket.expect("a cold read admits"), Some(value), now));
            assert!(model.fire("Complete", &mut expected));
            for index in 0..100 {
                let at = now + Duration::from_millis(index);
                let _ = cell.retire();
                assert!(model.fire("Invalidate", &mut expected));
                let (answer, ticket) = cell.read(at, ttl, true, SlotLock::Try);
                assert!(answer.is_none(), "{:?}: a retired answer", cell.kind);
                assert_eq!(ticket.is_some(), model.action_enabled("Start", &expected));
                assert!(!cell.slot.lock_or_recover().in_flight);
                assert_eq!(cell.next_refresh_deadline(at, ttl), now + PROBE_RETRY_FLOOR);
                let mut cleared_floor = expected.clone();
                cleared_floor.insert("ready", 1);
                assert!(!model.check_invariant("FloorSurvivesInvalidation", &cleared_floor));
            }
            let at = if let Some(forget) = forget {
                forget();
                assert!(model.fire("Forget", &mut expected));
                let mut as_invalidation = expected.clone();
                as_invalidation.insert("elapsed", 0);
                assert!(!model.check_invariant("FloorSurvivesInvalidation", &as_invalidation));
                now + Duration::from_millis(100)
            } else {
                assert!(model.fire("Cooldown", &mut expected));
                now + PROBE_RETRY_FLOOR
            };
            let (_, ticket) = cell.read(at, ttl, true, SlotLock::Try);
            assert_eq!(ticket.is_some(), model.action_enabled("Start", &expected));
            assert!(
                ticket.is_some(),
                "{:?}: the read after the floor",
                cell.kind
            );
        }
        let state = synthetic_state();
        churn(&state.shared.fda, grant(), None);
        churn(&state.shared.debugger, debugger_entry(), None);
        churn(&state.shared.devtools, devtools_entry(), None);
        churn(&state.shared.census, Claimants::default(), None);
        let state = synthetic_state();
        churn(
            &state.shared.census,
            Claimants::default(),
            Some(&|| state.invalidate_census()),
        );
    }

    #[test]
    fn a_publication_between_read_and_park_is_not_lost() {
        let state = synthetic_state();
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let seen = state.publication();
        let ((probe, _), request) = state.read_at(ProbeGate::on(), ttl, now);
        assert_eq!(probe.label, ProbeLabel::Pending, "the read saw no answer");
        complete(&state, request.expect("a cold read admits"), grant(), now);
        assert!(
            state.wait_for_change(seen, &[ChangeKind::Fda], aterm_spec::wait::MUST_HAPPEN),
            "the completion between the read and the park is seen"
        );
        assert!(
            !state.wait_for_change(
                seen,
                &[ChangeKind::Debugger, ChangeKind::Policy],
                Duration::ZERO
            ),
            "only the watched kinds count"
        );

        let late = state.publication();
        assert!(!state.wait_for_change(late, &[ChangeKind::Fda], Duration::ZERO));

        let (tx, rx) = mpsc::channel();
        let waiter = state.clone();
        std::thread::spawn(move || {
            let _ = tx.send(waiter.wait_for_change(
                late,
                &[ChangeKind::Fda],
                aterm_spec::wait::MUST_HAPPEN,
            ));
        });
        state.invalidate();
        assert!(
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the waiter returns"),
            "the retired grant reached the parked waiter"
        );
    }

    #[test]
    fn a_no_op_retire_publishes_nothing() {
        let seq = |state: &ConsentState, kind: ChangeKind| state.publication().0[kind as usize];
        let closed = ProbeGate {
            enabled: false,
            check: true,
        };
        let ttl = Duration::from_secs(5);
        let state = synthetic_state();

        let before = seq(&state, ChangeKind::Debugger);
        for _ in 0..3 {
            let _ = state.debugger(closed);
        }
        assert_eq!(
            seq(&state, ChangeKind::Debugger),
            before,
            "cold: nothing to forget"
        );
        state
            .shared
            .debugger
            .seed_for_test(debugger_entry(), Instant::now());
        for _ in 0..3 {
            let _ = state.debugger(closed);
        }
        assert_eq!(seq(&state, ChangeKind::Debugger), before + 1, "warm: once");

        let before = seq(&state, ChangeKind::Devtools);
        let _ = state.devtools(closed, false);
        assert_eq!(seq(&state, ChangeKind::Devtools), before);
        state
            .shared
            .devtools
            .seed_for_test(devtools_entry(), Instant::now());
        let _ = state.devtools(ProbeGate::on(), true);
        let _ = state.devtools(ProbeGate::on(), true);
        assert_eq!(seq(&state, ChangeKind::Devtools), before + 1);

        let before = seq(&state, ChangeKind::Census);
        state.invalidate_census();
        assert_eq!(seq(&state, ChangeKind::Census), before);
        state
            .shared
            .census
            .seed_for_test(Claimants::default(), Instant::now());
        state.invalidate_census();
        state.invalidate_census();
        assert_eq!(seq(&state, ChangeKind::Census), before + 1);

        let before = seq(&state, ChangeKind::Fda);
        for _ in 0..3 {
            state.invalidate();
            let _ = state.read_at(closed, ttl, Instant::now());
        }
        assert_eq!(
            seq(&state, ChangeKind::Fda),
            before,
            "cold: nothing to forget"
        );
        {
            let _held = state.shared.fda.slot.lock_or_recover();
            for _ in 0..3 {
                state.invalidate();
            }
        }
        assert_eq!(seq(&state, ChangeKind::Fda), before, "contended and cold");
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), ttl, now);
        complete(&state, request.expect("admitted"), grant(), now);
        assert_eq!(seq(&state, ChangeKind::Fda), before + 1, "the completion");
        state.invalidate();
        state.invalidate();
        assert_eq!(seq(&state, ChangeKind::Fda), before + 2, "the grant, once");

        let before = seq(&state, ChangeKind::Policy);
        state.publish_policy((*state.policy()).clone());
        assert_eq!(seq(&state, ChangeKind::Policy), before, "an equal policy");
        let off = crate::app_config::Config {
            privacy: Some(crate::app_config::PrivacyConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        state.publish_policy(ConsentPolicy::from_config(&off));
        assert_eq!(seq(&state, ChangeKind::Policy), before + 1);
    }

    fn section(privacy: crate::app_config::PrivacyConfig) -> ConsentPolicy {
        ConsentPolicy::from_config(&crate::app_config::Config {
            privacy: Some(privacy),
            ..Default::default()
        })
    }

    fn checks_off() -> ConsentPolicy {
        section(crate::app_config::PrivacyConfig {
            check: Some(false),
            ..Default::default()
        })
    }

    #[test]
    fn a_worker_probes_only_under_the_published_gate() {
        use crate::app_config::PrivacyConfig;
        use std::sync::atomic::AtomicUsize;
        static FDA: AtomicUsize = AtomicUsize::new(0);
        static DEBUGGER: AtomicUsize = AtomicUsize::new(0);
        static CENSUS: AtomicUsize = AtomicUsize::new(0);
        static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
        fn fda(_gate: ProbeGate) -> FdaProbe {
            FDA.fetch_add(1, Ordering::AcqRel);
            grant()
        }
        fn debugger(_gate: ProbeGate) -> DebuggerProbe {
            DEBUGGER.fetch_add(1, Ordering::AcqRel);
            DebuggerProbe {
                mode: DebuggerMode::Disabled,
                member: consent::DebuggerMember::Admin,
                label: DebuggerProbeLabel::StatusOk,
            }
        }
        fn census(_bundle_id: &str) -> Claimants {
            CENSUS.fetch_add(1, Ordering::AcqRel);
            Claimants::default()
        }
        const SUBJECT: &str = "com.example.subject";
        fn cold_subject() -> Option<&'static str> {
            None
        }
        fn warm_subject() -> Option<&'static str> {
            DEVTOOLS.fetch_add(1, Ordering::AcqRel);
            Some(SUBJECT)
        }
        fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            DEVTOOLS.fetch_add(1, Ordering::AcqRel);
            DevToolProbe::refused(DevToolProbeLabel::TimedOut)
        }
        fn list(_gate: ProbeGate, _subject: &str) -> DevToolList {
            DEVTOOLS.fetch_add(1, Ordering::AcqRel);
            DevToolList::refused(DevToolListLabel::RefusedDisabled)
        }
        fn held<E>(cell: &SlotCell<E>) -> (bool, bool) {
            let slot = cell.slot.lock_or_recover();
            (slot.in_flight, slot.entry.is_some())
        }
        let runs = || [&FDA, &DEBUGGER, &CENSUS, &DEVTOOLS].map(|arm| arm.load(Ordering::Acquire));
        let cases = [
            (section(PrivacyConfig::default()), [1, 1, 1, 3]),
            (checks_off(), [0, 0, 1, 0]),
            (
                section(PrivacyConfig {
                    enabled: Some(false),
                    ..Default::default()
                }),
                [0, 0, 0, 0],
            ),
            (
                section(PrivacyConfig {
                    observer: Some(false),
                    ..Default::default()
                }),
                [1, 1, 1, 0],
            ),
        ];
        for (published, expected) in cases {
            let mut state = synthetic_state();
            state.probes.fda = fda;
            state.probes.debugger = debugger;
            state.probes.census = census;
            state.probes.devtools = devtools;
            state.probes.devtools_list = list;
            state.probes.subject = cold_subject;
            state.probes.warm_subject = warm_subject;
            state.publish_policy(published.clone());
            let before = runs();
            let seen = state.publication();
            let (probe, _) = state.fda(ProbeGate::on(), Duration::from_secs(5));
            assert_eq!(probe.label, ProbeLabel::Pending, "an FDA worker is out");
            assert!(
                state.debugger(ProbeGate::on()).pending(),
                "a debugger worker is out"
            );
            assert!(
                state.claimants(Some(SUBJECT), true).1,
                "a census worker is out"
            );
            assert!(
                state.devtools(ProbeGate::on(), false).pending(),
                "a Developer Tools worker is out"
            );
            for kind in [
                ChangeKind::Fda,
                ChangeKind::Debugger,
                ChangeKind::Census,
                ChangeKind::Devtools,
            ] {
                assert!(
                    state.wait_for_change(seen, &[kind], aterm_spec::wait::MUST_HAPPEN),
                    "the {kind:?} worker frees its slot under {published:?}"
                );
            }
            let after = runs();
            let ran: [usize; 4] = std::array::from_fn(|arm| after[arm] - before[arm]);
            assert_eq!(ran, expected, "the arms run under {published:?}");
            assert_eq!(
                [
                    held(&state.shared.fda),
                    held(&state.shared.debugger),
                    held(&state.shared.census),
                    held(&state.shared.devtools),
                ],
                expected.map(|runs| (false, runs > 0)),
                "every slot is free, with an answer only from a probe, under {published:?}"
            );
        }
    }

    fn at_the_first_bump(
        state: &ConsentState,
        publish: impl FnOnce() + Send + 'static,
    ) -> (bool, ProbeGate) {
        let seen = state.publication();
        let held = state.shared.publications.lock.lock_or_recover();
        std::thread::spawn(publish);
        let bumped = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the publisher's first bump",
            || Some(state.publication()).filter(|now| *now != seen),
        );
        let gate = state
            .shared
            .policy
            .try_lock()
            .map(|published| published.gate)
            .expect("the section is not held while waiters are told");
        drop(held);
        for kind in [ChangeKind::Policy, ChangeKind::Fda] {
            assert!(
                state.wait_for_change(seen, &[kind], aterm_spec::wait::MUST_HAPPEN),
                "{kind:?} is told"
            );
        }
        let fda = ChangeKind::Fda as usize;
        (bumped.0[fda] != seen.0[fda], gate)
    }

    #[test]
    fn a_new_gate_is_published_before_its_retirement_wakes_anyone() {
        let answered = || {
            let state = synthetic_state();
            let now = Instant::now();
            let (_, request) = state.read_at(ProbeGate::on(), Duration::from_secs(5), now);
            complete(&state, request.expect("admitted"), grant(), now);
            state
        };

        let state = answered();
        let publisher = state.clone();
        let (fda_first, gate) = at_the_first_bump(&state, move || {
            publisher.publish_policy(checks_off());
        });
        assert!(!fda_first, "the section is told first");
        assert!(!gate.permits(), "nothing is told before the swap");
        assert!(
            state.shared.fda.slot.lock_or_recover().entry.is_none(),
            "the gate change retired the answer"
        );

        let state = answered();
        let publisher = state.clone();
        let (fda_first, gate) = at_the_first_bump(&state, move || {
            publisher.invalidate();
            publisher.publish_policy(checks_off());
        });
        assert!(
            fda_first && gate.permits(),
            "the retirement shows under the replaced gate"
        );

        let state = answered();
        let before = state.publication();
        let mut slower = (*state.policy()).clone();
        slower.interval *= 2;
        state.publish_policy(slower);
        assert!(
            state.shared.fda.slot.lock_or_recover().entry.is_some(),
            "the same gate keeps the answer"
        );
        assert_eq!(
            state.publication().0[ChangeKind::Fda as usize],
            before.0[ChangeKind::Fda as usize]
        );
    }

    #[test]
    fn async_probe_real_publication_conforms_to_the_bounded_model() {
        fn replay<E: Clone + PartialEq + std::fmt::Debug>(kind: SlotKind, stale: Stale, value: E) {
            let serve = stale == Stale::Serve;
            for invalidations_before in 0..=2 {
                for invalidations_during in 0..=2 - invalidations_before {
                    let cell = SlotCell::new(kind, stale);
                    let now = Instant::now();
                    let ttl = Duration::from_secs(5);
                    let model = probe_model_serving(serve);
                    let mut expected = model.init_state();
                    let observed_at = std::cell::Cell::new(now);
                    let check = |expected: &std::collections::BTreeMap<&'static str, i64>| {
                        let slot = cell.slot.lock_or_recover();
                        let epoch = cell.epoch.load(Ordering::Acquire);
                        assert_eq!(epoch as i64, expected["epoch"]);
                        assert_eq!(slot.request_epoch as i64, expected["request_epoch"]);
                        assert_eq!(i64::from(slot.in_flight), expected["flight"]);
                        assert_eq!(i64::from(slot.entry.is_some()), expected["cached"]);
                        assert_eq!(
                            i64::from(slot.retry_after.is_none_or(|at| at <= observed_at.get())),
                            expected["ready"]
                        );
                        assert_eq!(
                            slot.entry.as_ref().map_or(0, |entry| entry.epoch) as i64,
                            expected["cached_epoch"]
                        );
                        assert!(model.check_invariant("CurrentPublication", expected));
                        assert!(model.check_invariant("OneWorkerOrCache", expected));
                    };
                    for _ in 0..invalidations_before {
                        assert!(!cell.retire(), "nothing is held to forget");
                        assert!(model.fire("Invalidate", &mut expected));
                        check(&expected);
                    }
                    let (pending, ticket) = cell.read(now, ttl, true, SlotLock::Try);
                    assert_eq!(pending, None);
                    let ticket = ticket.expect("a cold read admits");
                    assert!(model.fire("Start", &mut expected));
                    check(&expected);
                    for _ in 0..invalidations_during {
                        let _ = cell.retire();
                        assert!(model.fire("Invalidate", &mut expected));
                        check(&expected);
                    }
                    for _ in 0..100 {
                        let (pending, next) = cell.read(now, ttl, true, SlotLock::Try);
                        assert_eq!(pending, None);
                        assert!(next.is_none());
                        assert!(!model.action_enabled("Start", &expected));
                        check(&expected);
                    }
                    let mut old_unchecked_publisher = expected.clone();
                    old_unchecked_publisher.insert("flight", 0);
                    old_unchecked_publisher.insert("cached", 1);
                    old_unchecked_publisher.insert("cached_epoch", ticket.epoch as i64);
                    if invalidations_during != 0 {
                        assert!(
                            !model.check_invariant("CurrentPublication", &old_unchecked_publisher)
                        );
                    }
                    assert!(cell.complete(ticket, Some(value.clone()), now));
                    assert!(model.fire("Complete", &mut expected));
                    check(&expected);
                    if invalidations_during != 0 {
                        let (pending, blocked) = cell.read(now, ttl, true, SlotLock::Try);
                        assert_eq!(pending, None);
                        assert!(
                            blocked.is_none(),
                            "invalidation retains the admission floor"
                        );
                        assert!(!model.action_enabled("Start", &expected));
                        observed_at.set(now + PROBE_RETRY_FLOOR);
                        assert!(model.fire("Cooldown", &mut expected));
                        let (pending, current) =
                            cell.read(observed_at.get(), ttl, true, SlotLock::Try);
                        assert_eq!(pending, None);
                        assert!(model.fire("Start", &mut expected));
                        check(&expected);
                        assert!(cell.complete(
                            current.expect("admitted at the floor"),
                            Some(value.clone()),
                            observed_at.get()
                        ));
                        assert!(model.fire("Complete", &mut expected));
                        check(&expected);
                    }
                    assert_eq!(
                        cell.read(observed_at.get(), ttl, true, SlotLock::Try)
                            .0
                            .map(|(served, _)| served),
                        Some(value.clone())
                    );
                    observed_at.set(observed_at.get() + ttl);
                    assert!(model.fire("Cooldown", &mut expected));
                    let (answer, refresh) = cell.read(observed_at.get(), ttl, true, SlotLock::Try);
                    assert!(refresh.is_some());
                    if serve {
                        assert_eq!(answer.map(|(served, _)| served), Some(value.clone()));
                    } else {
                        assert_eq!(answer, None);
                        assert!(model.fire("Expire", &mut expected));
                    }
                    assert!(model.fire("Start", &mut expected));
                    check(&expected);
                }
            }
        }
        replay(SlotKind::Fda, Stale::Drop, grant());
        replay(SlotKind::Debugger, Stale::Serve, debugger_entry());
    }

    #[test]
    fn disabled_gates_override_warm_grants_and_late_workers() {
        for gate in [
            ProbeGate {
                enabled: false,
                check: true,
            },
            ProbeGate {
                enabled: true,
                check: false,
            },
            ProbeGate {
                enabled: false,
                check: false,
            },
        ] {
            for finish_before_disable in [false, true] {
                let state = synthetic_state();
                let now = Instant::now();
                let ttl = Duration::from_secs(5);
                let (_, request) = state.read_at(ProbeGate::on(), ttl, now);
                let request = request.unwrap();
                if finish_before_disable {
                    complete(&state, request, grant(), now);
                }
                let (disabled, next) = state.read_at(gate, ttl, now);
                assert_eq!(disabled.0.label, ProbeLabel::RefusedDisabled);
                assert_eq!(disabled.0.state, FdaState::Unknown);
                assert!(next.is_none());
                if !finish_before_disable {
                    assert!(state.shared.fda.slot.lock().unwrap().in_flight);
                    complete(&state, request, grant(), now);
                }
                let (reenabled, request) =
                    state.read_at(ProbeGate::on(), ttl, now + PROBE_RETRY_FLOOR);
                assert_eq!(reenabled.0.label, ProbeLabel::Pending);
                assert!(request.is_some(), "reenabling requires a new probe");
            }
        }
    }

    #[test]
    fn contended_retire_refuses_old_grants() {
        fn contend<E: Clone>(cell: &SlotCell<E>, value: E) {
            let now = Instant::now();
            let ttl = Duration::from_secs(5);
            let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
            assert!(cell.complete(ticket.expect("a cold read admits"), Some(value), now));
            assert!(cell.read(now, ttl, false, SlotLock::Try).0.is_some());
            let guard = cell.slot.lock_or_recover();
            let (answer, ticket) = cell.read(now, ttl, true, SlotLock::Try);
            assert!(
                answer.is_none() && ticket.is_none(),
                "{:?}: the read does not wait",
                cell.kind
            );
            assert!(
                !cell.retire(),
                "{:?}: the retirement does not wait",
                cell.kind
            );
            drop(guard);
            assert!(cell.slot.lock_or_recover().entry.is_some());
            for admit in [false, true] {
                assert!(
                    cell.read(now, ttl, admit, SlotLock::Try).0.is_none(),
                    "{:?}: the retired answer is served",
                    cell.kind
                );
            }
        }
        contend(&SlotCell::new(SlotKind::Fda, Stale::Drop), grant());
        contend(
            &SlotCell::new(SlotKind::Debugger, Stale::Serve),
            debugger_entry(),
        );
    }

    #[test]
    fn a_read_overtaken_by_a_retirement_reads_again() {
        fn overtake<E: Clone + Send + Sync + 'static>(kind: SlotKind, stale: Stale, value: E) {
            let now = Instant::now();
            let ttl = Duration::from_secs(5);
            let expired = now + ttl.max(PROBE_RETRY_FLOOR);
            let retired_while_held = || {
                let cell = Arc::new(SlotCell::new(kind, stale));
                cell.seed_for_test(value.clone(), now);
                let retirer = Arc::clone(&cell);
                *cell.held_read_hook.lock_or_recover() = Some(Box::new(move || {
                    assert!(!retirer.retire(), "the read holds the lock");
                }));
                cell
            };
            let ran = |cell: &SlotCell<E>| {
                assert!(
                    cell.held_read_hook.lock_or_recover().is_none(),
                    "the retirement ran"
                );
            };

            let cell = retired_while_held();
            let ((answer, _), epoch) = cell.read_once(now, ttl, false, SlotLock::Wait);
            assert!(
                answer.is_some() && cell.epoch.load(Ordering::Acquire) != epoch,
                "{kind:?}: one pass serves the answer the retirement retired"
            );
            let cell = retired_while_held();
            let ((answer, ticket), _) = cell.read_once(expired, ttl, true, SlotLock::Wait);
            assert_eq!(
                answer.is_some(),
                stale == Stale::Serve,
                "{kind:?}: one pass serves the expired answer where stale answers are served"
            );
            assert!(cell.complete(
                ticket.expect("an expired answer admits"),
                Some(value.clone()),
                expired
            ));
            assert!(
                cell.read(expired, ttl, false, SlotLock::Wait).0.is_none(),
                "{kind:?}: one pass's claim keeps nothing"
            );

            for admit in [false, true] {
                let cell = retired_while_held();
                assert!(
                    cell.read(now, ttl, admit, SlotLock::Wait).0.is_none(),
                    "{kind:?}: the read served a retired answer (admit={admit})"
                );
                ran(&cell);
            }
            let cell = retired_while_held();
            let (answer, ticket) = cell.read(expired, ttl, true, SlotLock::Wait);
            ran(&cell);
            assert!(
                answer.is_none(),
                "{kind:?}: the read that claimed the refresh served a retired answer"
            );
            assert!(cell.complete(
                ticket.expect("the claim is kept"),
                Some(value.clone()),
                expired
            ));
            assert!(
                cell.read(expired, ttl, false, SlotLock::Wait).0.is_some(),
                "{kind:?}: the claimed worker's answer is kept"
            );
        }
        overtake(SlotKind::Fda, Stale::Drop, grant());
        overtake(SlotKind::Debugger, Stale::Serve, debugger_entry());
    }

    #[test]
    fn an_off_loop_read_waits_out_a_held_slot_lock() {
        let state = synthetic_state();
        let ttl = Duration::from_secs(5);
        let (_, ticket) = state.read_at(ProbeGate::on(), ttl, Instant::now());
        complete(
            &state,
            ticket.expect("a cold read admits"),
            grant(),
            Instant::now(),
        );
        let guard = state.shared.fda.slot.lock().unwrap();
        assert_eq!(
            state.fda(ProbeGate::on(), ttl).0.label,
            ProbeLabel::Pending,
            "the event loop never waits on the lock"
        );
        let (started_tx, started_rx) = mpsc::channel();
        let (read_tx, read_rx) = mpsc::channel();
        let reader = state.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            read_tx
                .send(reader.fda_off_loop(ProbeGate::on(), ttl))
                .unwrap();
        });
        started_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        assert!(read_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        let (probe, _age) = read_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert_eq!(probe.state, FdaState::Granted);
    }

    #[test]
    fn pending_deadlines_stay_future_and_cold_inert_reads_stay_inert() {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::from_millis(1), now);
        assert!(request.is_some());
        for step in 0..100 {
            let at = now + Duration::from_millis(step);
            assert_eq!(
                state.next_refresh_deadline(at, Duration::from_millis(1)),
                Some(at + PROBE_RETRY_FLOOR)
            );
            assert!(
                state
                    .read_at(ProbeGate::on(), Duration::from_millis(1), at)
                    .1
                    .is_none()
            );
        }
        let inert = ConsentState::inert();
        assert_eq!(
            inert.fda(ProbeGate::on(), Duration::ZERO).0.label,
            ProbeLabel::RefusedDisabled
        );
        assert_eq!(inert.next_refresh_deadline(now, Duration::ZERO), None);
    }

    #[test]
    fn zero_interval_cannot_turn_completion_into_a_probe_wake_loop() {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::ZERO, now);
        complete(&state, request.unwrap(), grant(), now);
        for elapsed in [Duration::ZERO, PROBE_RETRY_FLOOR - Duration::from_nanos(1)] {
            let (answer, next) = state.read_at(ProbeGate::on(), Duration::ZERO, now + elapsed);
            assert_eq!(answer.0, grant());
            assert!(next.is_none(), "completion must not immediately re-arm");
            assert_eq!(
                state.next_refresh_deadline(now + elapsed, Duration::ZERO),
                Some(now + PROBE_RETRY_FLOOR)
            );
        }
        let (expired, refresh) =
            state.read_at(ProbeGate::on(), Duration::ZERO, now + PROBE_RETRY_FLOOR);
        assert_eq!(expired.0.label, ProbeLabel::Pending);
        assert!(refresh.is_some(), "minimum freshness still expires");
    }

    #[test]
    fn fda_cached_never_admits_and_serves_only_a_fresh_entry() {
        let state = synthetic_state();
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let cached = |at: Instant| state.cached_at(ProbeGate::on(), ttl, at);
        let untouched = |epoch: u64, held: bool| {
            let slot = state.shared.fda.slot.lock().unwrap();
            assert!(!slot.in_flight, "a passive read claimed the worker");
            assert_eq!(
                slot.entry.is_some(),
                held,
                "a passive read dropped the entry"
            );
            assert_eq!(state.shared.fda.epoch.load(Ordering::Acquire), epoch);
        };

        let cold = state.shared.fda.epoch.load(Ordering::Acquire);
        assert_eq!(cached(now), (FdaProbe::pending(), Duration::ZERO));
        untouched(cold, false);
        assert!(state.shared.fda.slot.lock().unwrap().retry_after.is_none());

        let (_, ticket) = state.read_at(ProbeGate::on(), ttl, now);
        complete(
            &state,
            ticket.expect("a cold dispatching read admits"),
            grant(),
            now,
        );
        let epoch = state.shared.fda.epoch.load(Ordering::Acquire);
        let age = ttl - Duration::from_nanos(1);
        assert_eq!(cached(now + age), (grant(), age));
        let closed = ProbeGate {
            enabled: false,
            check: true,
        };
        assert_eq!(
            state.cached_at(closed, ttl, now).0.label,
            ProbeLabel::RefusedDisabled
        );
        assert_eq!(cached(now + ttl).0.label, ProbeLabel::Pending);
        untouched(epoch, true);
        assert_eq!(cached(now).0, grant(), "the entry is still served");

        let (answer, refresh) = state.read_at(ProbeGate::on(), ttl, now + ttl);
        assert_eq!(answer.0.label, ProbeLabel::Pending);
        assert!(
            refresh.is_some(),
            "the dispatching read admits at the interval"
        );

        let state = synthetic_state();
        let (_, ticket) = state.read_at(ProbeGate::on(), ttl, now);
        complete(&state, ticket.expect("admitted"), grant(), now);
        let guard = state.shared.fda.slot.lock().unwrap();
        state.invalidate();
        drop(guard);
        assert!(state.shared.fda.slot.lock().unwrap().entry.is_some());
        assert_eq!(
            state.cached_at(ProbeGate::on(), ttl, now).0.label,
            ProbeLabel::Pending
        );
    }

    #[test]
    fn failed_worker_creation_releases_its_slot_with_bounded_backoff() {
        fn refused<E: Clone>(cell: &SlotCell<E>, ttl: Duration) {
            let now = Instant::now();
            let (_, ticket) = cell.read(now, ttl, true, SlotLock::Try);
            cell.spawn_failed(ticket.expect("a cold read admits"));
            let backoff = ttl.max(PROBE_RETRY_FLOOR);
            for offset in [Duration::ZERO, backoff - Duration::from_nanos(1)] {
                let (answer, next) = cell.read(now + offset, ttl, true, SlotLock::Try);
                assert!(answer.is_none() && next.is_none(), "{:?}", cell.kind);
                assert!(
                    !cell.slot.lock_or_recover().in_flight,
                    "{:?}: no worker owns the slot",
                    cell.kind
                );
            }
            assert!(
                cell.read(now + backoff, ttl, true, SlotLock::Try)
                    .1
                    .is_some(),
                "{:?}: the read after the backoff admits",
                cell.kind
            );
        }
        let state = synthetic_state();
        refused(&state.shared.fda, Duration::ZERO);
        refused(&state.shared.debugger, DEBUGGER_TTL);
    }

    #[test]
    fn a_census_completion_never_ticks_the_fda_card() {
        let state = synthetic_state();
        let (tx, rx) = mpsc::channel();
        state.set_slot_wake(Arc::new(move |kind| {
            let _ = tx.send(kind);
        }));
        let next = || {
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the worker publishes")
        };
        assert!(
            state.claimants(Some("com.example.subject"), true).1,
            "a census worker is out"
        );
        let census = next();
        assert_eq!(census, SlotKind::Census);
        assert!(matches!(census.wake(), Wake::ConsentCensusReady));

        let _ = state.fda(ProbeGate::on(), Duration::from_secs(5));
        let fda = next();
        assert_eq!(fda, SlotKind::Fda);
        assert!(matches!(fda.wake(), Wake::ConsentProbeReady));
    }

    struct BlockedProbe {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    static BLOCKED_PROBE: Mutex<Option<BlockedProbe>> = Mutex::new(None);
    static BLOCKED_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn blocked_probe(_gate: ProbeGate) -> FdaProbe {
        BLOCKED_CALLS.fetch_add(1, Ordering::SeqCst);
        let fixture = BLOCKED_PROBE
            .lock()
            .unwrap()
            .take()
            .expect("only one worker");
        fixture.entered.send(()).unwrap();
        fixture
            .release
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        grant()
    }

    #[test]
    fn a_blocked_real_worker_cannot_block_reads_or_multiply_on_invalidation() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        *BLOCKED_PROBE.lock().unwrap() = Some(BlockedProbe {
            entered: entered_tx,
            release: release_rx,
        });
        let mut state = synthetic_state();
        state.probes.fda = blocked_probe;
        let weak = Arc::downgrade(&state.shared);
        state.set_slot_wake(Arc::new(move |kind| {
            assert_eq!(kind, SlotKind::Fda);
            assert!(
                weak.upgrade().unwrap().fda.slot.try_lock().is_ok(),
                "wake runs unlocked"
            );
            done_tx.send(()).unwrap();
        }));
        let ttl = Duration::from_secs(5);
        assert_eq!(state.fda(ProbeGate::on(), ttl).0.label, ProbeLabel::Pending);
        entered_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        for _ in 0..100 {
            assert_eq!(state.fda(ProbeGate::on(), ttl).0.label, ProbeLabel::Pending);
            state.invalidate();
        }
        assert!(state.shared.fda.slot.lock().unwrap().in_flight);
        assert_eq!(BLOCKED_CALLS.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(!state.shared.fda.slot.lock().unwrap().in_flight);
        assert!(
            state.shared.fda.slot.lock().unwrap().entry.is_none(),
            "stale grant refused"
        );
    }
}
