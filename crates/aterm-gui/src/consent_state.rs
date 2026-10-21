// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's consent model: the probe slots every consent reader shares, the
//! `[privacy]` mirror, and the two additive `status` fields (design §5.2).
//!
//! Every OS consent question goes through [`ConsentProbes`], injected function
//! pointers with an inert arm: a windowed instance gets the live arms; a
//! headless instance and every unit test get arms that answer `unknown` without
//! a syscall, so no test binary reaches `tccd` or `WindowServer` (`AGENTS.md`,
//! the aterm-census guard `input-probe`). A `--headless` instance still answers
//! `privacy`, with `full_disk_access=unknown probe=refused_disabled`: not
//! consulted, which is distinct from a denial.
//!
//! Live reads are asynchronous: a cold, invalidated or expired observation
//! reports `full_disk_access=unknown probe=pending probe_age_ms=-` (JSON age
//! `null`). One instance-owned worker refreshes it and wakes the GUI; a stalled
//! probe cannot spawn more workers. Freshness and retry spacing have a 500 ms
//! floor.
//!
//! Protected paths come only from `aterm_containment::consent`
//! (`protected_roots`, `Folder::path`); this module holds no path literals
//! (the census guard `protected-folder`).

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

// The injected probes

/// Every OS question this feature asks goes through one of these function
/// pointers, injected at `App` construction like `App::lock_modifiers`.
///
/// * [`ConsentProbes::live`]: a windowed instance, the real probes.
/// * [`ConsentProbes::inert`]: a headless instance and every unit test; no
///   syscall.
///
/// `fda` also passes `consent::probe_fda`'s own in-bundle guard, which also
/// protects the bundled CLI (design §3.3 guardrails 1 and 2).
#[derive(Clone, Copy)]
pub(crate) struct ConsentProbes {
    /// One `open(TCC.db, O_RDONLY | O_CLOEXEC)` (the system one too, where
    /// macOS 27 moved the user's), fd closed at once, contents never read,
    /// made by a child of the window (`aterm __tcc-open`).
    pub(crate) fda: fn(ProbeGate) -> FdaProbe,
    /// `responsibility_get_pid_responsible_for_pid`, resolved through `dlsym`
    /// once per call and asked about each pid, answering in order. Not a TCC
    /// or `WindowServer` contact, but it is still an OS call made once per live
    /// session, so it takes the same fence.
    pub(crate) responsible: fn(&[i32]) -> Vec<Result<i32, ResponsibleError>>,
    /// The claimant census: every bundle on disk claiming this process's
    /// `CFBundleIdentifier`, with each one's designated requirement. Runs
    /// `codesign` per candidate, so it takes the same fence as the others and
    /// its inert arm reads nothing at all.
    pub(crate) census: fn(&str) -> Claimants,
    /// The debugger rule (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §3.2): `DevToolsSecurity -status` (no dialog) plus the directory
    /// membership query. Spawns a process, so it is fenced; the inert arm
    /// spawns nothing.
    pub(crate) debugger: fn(ProbeGate) -> DebuggerProbe,
    /// This process's own code-signing flags (`csops`): whether a same-user
    /// debugger could attach to aterm itself. A syscall about our own image,
    /// but an OS call all the same, so it is injected like the rest.
    pub(crate) hardening: fn() -> SelfHardening,
    /// The Developer Tools list (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §9.1): one bounded `/usr/bin/log show` of tccd's own answers for the
    /// bundle id given. Spawns a process, so it is fenced; the inert arm
    /// spawns nothing.
    pub(crate) devtools: fn(ProbeGate, &str) -> DevToolProbe,
    /// The same list's SWITCH (debugger design §9.1, O18): the subject's row in
    /// the system TCC.db, one `open` and a bounded `/usr/bin/sqlite3
    /// -readonly` that takes no lock on it (`aterm_uds::foreign_db`). Spawns a
    /// process, so it is fenced; the inert arm opens and spawns nothing.
    pub(crate) devtools_list: fn(ProbeGate, &str) -> DevToolList,
    /// The bundle id the Developer Tools row asks about: the running bundle's
    /// `CFBundleIdentifier` once the identity is warm, never a literal. Not an
    /// OS question; injected so a test can name a subject without warming the
    /// process-wide identity. The reading thread calls only this one.
    pub(crate) subject: fn() -> Option<&'static str>,
    /// The same bundle id, reading the identity first when nothing has yet:
    /// that read may run `codesign` (50–150 ms), so only the
    /// `aterm-devtools-probe` worker calls it, when [`Self::subject`] found the
    /// identity cold. The inert arms never read it.
    pub(crate) warm_subject: fn() -> Option<&'static str>,
    /// Whether these are the live arms. Read only by `Debug` and tests; the
    /// `observer` row renders the inert arms' labels as `off`
    /// ([`crate::privacy_report::observer_fda_value`]).
    pub(crate) live: bool,
}

/// The inert census reads nothing and answers `Unavailable`: "I did not look",
/// never "there is nothing".
fn inert_census(_bundle_id: &str) -> Claimants {
    Claimants::default()
}

/// The inert debugger arm: `DevToolsSecurity` was deliberately not consulted,
/// so `mode=unknown member=unknown` with `probe=refused_disabled` — the same
/// reading, for the same reason, as [`inert_fda_probe`].
fn inert_debugger(_gate: ProbeGate) -> DebuggerProbe {
    DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled)
}

/// The inert hardening arm: the flags were not read, which is `unknown`.
fn inert_hardening() -> SelfHardening {
    SelfHardening::Unknown
}

/// The inert Developer Tools arm: tccd's log was deliberately not read, so
/// `state=unknown probe=refused_disabled`, for the same reason as
/// [`inert_fda_probe`].
fn inert_devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
    DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
}

/// The inert list arm: the system TCC.db was deliberately not read.
fn inert_devtools_list(_gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(DevToolListLabel::RefusedDisabled)
}

impl ConsentProbes {
    /// The windowed instance's arms.
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

    /// The headless / unit-test arms: no syscall, and `unknown` said out loud.
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

    /// Only a windowed macOS instance gets live arms. Other platforms report
    /// unsupported directly, without launching a worker for a nonexistent TCC.
    /// A headless instance remains deliberately disabled on every platform.
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

    /// Called only after the live gate was refused. Non-live function pointers
    /// are inert by construction; a disabled live instance never invokes one.
    fn inactive_fda(self, gate: ProbeGate) -> FdaProbe {
        if !gate.permits() {
            inert_fda_probe(gate)
        } else {
            debug_assert!(!self.live);
            (self.fda)(gate)
        }
    }
}

fn unsupported_fda_probe(gate: ProbeGate) -> FdaProbe {
    FdaProbe::refused(if gate.permits() {
        ProbeLabel::UnsupportedPlatform
    } else {
        ProbeLabel::RefusedDisabled
    })
}

/// There is no `DevToolsSecurity` off macOS. A closed gate still reads as
/// configuration, exactly as [`unsupported_fda_probe`] does.
fn unsupported_debugger_probe(gate: ProbeGate) -> DebuggerProbe {
    DebuggerProbe::refused(if gate.permits() {
        DebuggerProbeLabel::UnsupportedPlatform
    } else {
        DebuggerProbeLabel::RefusedDisabled
    })
}

/// There is no Developer Tools list off macOS. A closed gate still reads as
/// configuration, exactly as [`unsupported_fda_probe`] does.
fn unsupported_devtools_probe(gate: ProbeGate, _subject: &str) -> DevToolProbe {
    DevToolProbe::refused(if gate.permits() {
        DevToolProbeLabel::UnsupportedPlatform
    } else {
        DevToolProbeLabel::RefusedDisabled
    })
}

/// There is no Developer Tools list off macOS: the list arm's twin of
/// [`unsupported_devtools_probe`].
fn unsupported_devtools_list(gate: ProbeGate, _subject: &str) -> DevToolList {
    DevToolList::refused(if gate.permits() {
        DevToolListLabel::UnsupportedPlatform
    } else {
        DevToolListLabel::RefusedDisabled
    })
}

/// The inert Full Disk Access arm: the probe was deliberately not consulted.
/// `refused_disabled` rather than `refused_out_of_bundle` because the reason is
/// this instance's configuration, not where its executable happens to live.
fn inert_fda_probe(_gate: ProbeGate) -> FdaProbe {
    FdaProbe::refused(ProbeLabel::RefusedDisabled)
}

/// The inert responsibility arm: the SPI is not consulted about any pid.
fn inert_responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
    vec![Err(ResponsibleError::Unsupported); pids.len()]
}

// Instance-owned consent state

/// Minimum freshness and timer spacing. A zero configured interval must not
/// turn completion wakes into an endless probe/complete/probe loop. A blocked
/// request owns its worker slot until it returns; timers cannot replace it.
const PROBE_RETRY_FLOOR: Duration = Duration::from_millis(500);

/// Which probe slot a publication or a GUI wake is about.
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
    /// The change counter a publication of this slot bumps.
    const fn change(self) -> ChangeKind {
        match self {
            Self::Fda => ChangeKind::Fda,
            Self::Census => ChangeKind::Census,
            Self::Debugger => ChangeKind::Debugger,
            Self::Devtools => ChangeKind::Devtools,
        }
    }

    /// The event-loop wake a completion of this slot's worker posts. Each
    /// kind has its own variant: only a Full Disk Access answer ticks the
    /// access card, the debugger and Developer Tools handlers hand their
    /// answer to their own lanes, and `metrics::wakes::WakeCensus::note`
    /// attributes wakes per kind.
    pub(crate) const fn wake(self) -> Wake {
        match self {
            Self::Fda => Wake::ConsentProbeReady,
            Self::Census => Wake::ConsentCensusReady,
            Self::Debugger => Wake::DebuggerProbeReady,
            Self::Devtools => Wake::DevToolsProbeReady,
        }
    }
}

/// The GUI's completion wake, told which slot published.
type SlotWake = Arc<dyn Fn(SlotKind) + Send + Sync>;

/// What a read does with an answer at least a TTL old.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stale {
    /// Serve it with its age while one worker refreshes it: the census,
    /// debugger and Developer Tools rows, which report that age.
    Serve,
    /// Never serve it: an expired Full Disk Access grant reads pending until
    /// its refresh publishes, so a revoked grant is never reported held.
    Drop,
}

/// How a read takes a slot's lock.
#[derive(Clone, Copy, Debug)]
enum SlotLock {
    /// Never wait on it: a contended read answers pending and claims nothing.
    /// The event loop's Full Disk Access reads.
    Try,
    /// Wait out the holder, a read or a publication (no syscall runs under
    /// the lock). A control worker parked on the publications reads this way:
    /// a read that lost to another read is followed by no publication, so it
    /// would report pending for the rest of its wait. The background slots
    /// are read this way on every thread: their completion wakes hand the
    /// answer to a lane once (the debugger change lane, the Developer Tools
    /// watch), and a waiter woken by the same completion reads the slot at
    /// that moment, so a read that gave up would drop the verdict.
    Wait,
}

/// A worker's claim on its slot: the epoch the read that admitted it saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    epoch: u64,
}

/// What one read of a slot hands back: the answer with its age, and the
/// ticket of the worker the read claimed.
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
    /// No worker starts before it; every retirement keeps it.
    retry_after: Option<Instant>,
}

/// One answer a bounded worker computes off the event loop. The Full Disk
/// Access grant, the claimant census, the debugger rule and the Developer
/// Tools list are its four instances.
///
/// At most one worker is in flight, and a thread the OS refuses cannot turn
/// reads into a spawn loop. [`Self::retire`] moves `epoch` without waiting
/// for the lock: an answer published under an older epoch is never served,
/// and a worker admitted under one keeps no answer. `in_flight` and
/// `retry_after` survive a retirement, so focus churn can neither multiply
/// workers nor relaunch a quickly finished one inside [`PROBE_RETRY_FLOOR`].
struct SlotCell<E> {
    kind: SlotKind,
    stale: Stale,
    epoch: AtomicU64,
    /// The epoch + 1 of a ticket whose thread the OS refused (0: none). It is
    /// set without the lock, and the next admitting read frees the slot.
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

    /// The lock if nobody holds it.
    fn try_hold(&self) -> Option<MutexGuard<'_, Slot<E>>> {
        match self.slot.try_lock() {
            Ok(slot) => Some(slot),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Move the claim a read just made to the current epoch. Its worker is not
    /// spawned yet, so a retirement that met the read's lock costs it nothing:
    /// it keeps the answer it computes. A lock a never-wait read cannot take
    /// leaves the claim as it was, and its worker keeps nothing.
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

    /// A worker's end: it frees the slot, and keeps `value` only under the
    /// epoch it was admitted under. Whether `ticket` held the slot, which is
    /// the change waiters are told of, stale or not.
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

    /// Forget the answer without waiting: the epoch moves, then the entry is
    /// cleared if the lock is free. Whether an answer was forgotten, which is
    /// the only change a reader can see. When the lock is held, the holder
    /// sees the move: a read reads again or moves the claim it made to the new
    /// epoch ([`Self::read`]), and a completion keeps nothing and publishes
    /// anyway.
    fn retire(&self) -> bool {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.try_hold()
            .is_some_and(|mut slot| slot.entry.take().is_some())
    }

    /// Drop the admission floor, so the next read dispatches at once: the
    /// census after the bundles on disk changed under it.
    fn clear_floor(&self) {
        self.slot.lock_or_recover().retry_after = None;
    }

    /// Hold `value` as the current answer, published at `at`.
    #[cfg(test)]
    fn seed_for_test(&self, value: E, at: Instant) {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.slot.lock_or_recover().entry = Some(Entry { value, epoch, at });
    }

    /// The OS refused `ticket`'s thread, so no worker owns the slot. The next
    /// admitting read frees it and backs off by the TTL, so repeated reads
    /// cannot spawn in a loop.
    fn spawn_failed(&self, ticket: Ticket) {
        self.failed
            .store(ticket.epoch.wrapping_add(1), Ordering::Release);
    }

    /// When a read at `now` could next see something new, always after
    /// `now`: a current answer's expiry, else the admission floor, else one
    /// TTL on. A completion normally publishes sooner; a stalled worker gets
    /// at most this polling rate. It never waits on the lock.
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
    /// The answer with its age, and the ticket of the worker this read
    /// claimed: with `admit`, when the answer is missing or at least `ttl`
    /// old (never under [`PROBE_RETRY_FLOOR`]), no worker is out and the
    /// floor has passed. The caller spawns that worker with no lock held
    /// ([`spawn_slot_worker`]). Without `admit` the read changes nothing.
    fn read(&self, now: Instant, ttl: Duration, admit: bool, lock: SlotLock) -> SlotRead<E> {
        loop {
            let (read, epoch) = self.read_once(now, ttl, admit, lock);
            if self.epoch.load(Ordering::Acquire) == epoch {
                return read;
            }
            // A retirement that met this read's lock forgot nothing, so the
            // answer it retired may be what this read holds. A read that
            // claimed nothing reads again. One that claimed a worker serves
            // nothing and keeps the claim: a second pass would find the slot in
            // flight and claim nothing, and no thread would run that worker.
            match read {
                (_, Some(ticket)) => return (None, Some(self.restamp(ticket, lock))),
                (Some(_), None) => {}
                (None, None) => return read,
            }
        }
    }

    /// One pass of [`Self::read`] under the lock, and the epoch it read under.
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

    /// The entry a read under `epoch` may serve, with its age.
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
            // Retired, or expired where stale answers are dropped: gone.
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

/// Run `work` on a named worker for the ticket a read claimed. `work`
/// answers `None` when it ran nothing (its probe is off in the `[privacy]`
/// section published as it starts). Either way the worker frees the slot and
/// tells waiters and the GUI ([`SharedProbe::finish`]). A thread the OS
/// refuses is recorded on the slot ([`SlotCell::spawn_failed`]).
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

/// How long a census answer is served before a read refreshes it. The census
/// is about bundles on disk, which rarely change, and each refresh runs
/// `codesign` once per copy of the app. The census slot is not retired by
/// [`ConsentState::invalidate`] (focus and config do not change bundles on
/// disk).
const CENSUS_TTL: Duration = Duration::from_secs(60);

/// The debugger slot's answer: the probe and aterm's own hardening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DebuggerEntry {
    probe: DebuggerProbe,
    self_hardened: SelfHardening,
}

/// How long a debugger answer is served before a read refreshes it. The rule
/// changes only when an administrator changes it, and focus gain retires the
/// answer sooner ([`ConsentState::invalidate_debugger`]). What dispatches it:
/// the `privacy` verb's full report, the Security page, the change lane's
/// re-probe after a change exits (`App::tick_debugger_access`), and the
/// debugger-access launch ask (`App::look_for_debugger_moment`), which reads
/// it only while no answer is cached and once more, retiring the old one, at
/// the moment it would raise its alert.
const DEBUGGER_TTL: Duration = Duration::from_secs(60);

/// How long a Developer Tools answer is served before a read refreshes it.
/// tccd's answer changes only when the owner changes the list in System
/// Settings, and then only shows on the next new program, so a read every two
/// minutes while someone looks is enough; focus gain retires the answer sooner
/// ([`ConsentState::invalidate_devtools`]). Each refresh is one `log show`
/// (0.86 s wall for a 10-minute window, measured 2026-10-07).
const DEVTOOLS_TTL: Duration = Duration::from_secs(120);

/// The Developer Tools slot's answer: the probe, and the bundle id it was
/// about, which the worker resolved when the reading thread found the identity
/// cold (`None`: no usable one, and the probe is `refused_no_subject`).
#[derive(Clone, Copy, Debug)]
struct DevToolsEntry {
    probe: DevToolProbe,
    list: DevToolList,
    subject: Option<&'static str>,
}

/// What the `devtools` row reports: the probe, how long ago it completed, and
/// how old tccd's newest answer was when this row was read (`None` with no
/// sample: pending, refused, or no answer in the window).
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
    /// Nothing learned, for `label`'s reason.
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

    /// A published answer, aged against the two clocks it is read with: the
    /// slot's monotonic stamp for the probe, the wall clock for tccd's own
    /// timestamp (a stamp in the future is age 0).
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

/// The wall clock as unix milliseconds, for aging tccd's own timestamps. A
/// clock before 1970 reads 0, which ages every answer to 0 rather than
/// inventing an age.
fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// What the `debugger` row reports: the probe, aterm's own hardening, and the
/// answer's age (`None` while pending or refused — no sample, no age).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DebuggerRow {
    pub(crate) probe: DebuggerProbe,
    pub(crate) self_hardened: SelfHardening,
    pub(crate) probe_age_ms: Option<u128>,
}

impl DebuggerRow {
    /// Nothing learned, for `label`'s reason: every fact `unknown`.
    pub(crate) const fn refused(label: DebuggerProbeLabel) -> Self {
        Self {
            probe: DebuggerProbe::refused(label),
            self_hardened: SelfHardening::Unknown,
            probe_age_ms: None,
        }
    }

    /// The probe is out (or not yet dispatched): the rule is unread, but
    /// aterm's own hardening is fixed at exec, so a re-probe does not unlearn
    /// it. `self_hardened` is what a worker already read, `Unknown` before any.
    pub(crate) const fn awaiting(self_hardened: SelfHardening) -> Self {
        Self {
            probe: DebuggerProbe::pending(),
            self_hardened,
            probe_age_ms: None,
        }
    }

    /// A published answer, with its age only when a probe actually completed.
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

    /// How long ago the probe completed: `None` while pending or refused, and
    /// for an instance's syscall-free arm, which answers at every read.
    pub(crate) fn probe_age(&self) -> Option<Duration> {
        self.probe_age_ms
            .map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX)))
    }
}

/// The debugger probe's age only when a probe actually completed.
fn completed_debugger_age_ms(label: DebuggerProbeLabel, age: Duration) -> Option<u128> {
    (!label.refused() && label != DebuggerProbeLabel::Pending).then_some(age.as_millis())
}

/// What changed in the consent state. Each kind has its own counter, so a
/// waiter watches only the kinds its observation reads.
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

/// How a change reaches a waiter off the event loop. A publication is an
/// observable change only: an answer published or forgotten, a worker slot
/// released, a new policy. A retirement that forgot nothing publishes
/// nothing, so a read loop cannot wake itself.
///
/// A publisher bumps its counter, then takes and drops `lock`, then
/// notifies. A waiter compares the counters and parks under `lock`, so a bump
/// either precedes its comparison or lands after it parked: none is lost
/// between the two.
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
    /// The `[privacy]` mirror (see [`ConsentPolicy`]): published at
    /// construction and at every admitted config generation, never absent.
    policy: Mutex<Arc<ConsentPolicy>>,
    /// Every observable change of a slot or the mirror, for waiters off the
    /// event loop (`await consent`, `privacy`); [`Self::wake`] is the GUI's.
    publications: Publications,
    fda: SlotCell<FdaProbe>,
    census: SlotCell<Claimants>,
    debugger: SlotCell<DebuggerEntry>,
    devtools: SlotCell<DevToolsEntry>,
    /// The GUI's one completion wake, told which slot published
    /// ([`SlotKind::wake`]).
    wake: OnceLock<SlotWake>,
    /// This process's hardening, read by the first debugger worker and kept:
    /// the two bits it reads are fixed at exec. Never read on the event loop.
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

    /// The `[privacy]` section the last admitted config generation published.
    /// Every slot worker reads it as it starts: the read that dispatched it
    /// may have held a section a reload has since replaced (a control worker
    /// reads the mirror, or the `privacy` hop's copy of the config, and keeps
    /// rereading the slots under it), and no probe starts once a section that
    /// turns it off is published. The lock is held only for the `Arc` clone.
    fn published_policy(&self) -> Arc<ConsentPolicy> {
        Arc::clone(&self.policy.lock_or_recover())
    }

    /// The publication seam every worker ends through, and the conformance
    /// tests with it: `cell` frees the slot and keeps `value` when it is
    /// current ([`SlotCell::complete`]), then waiters and the GUI are told,
    /// with no lock held. A worker that ran nothing passes `None` and keeps
    /// no answer: a refusal kept would be served once its section reopens,
    /// which retires nothing in the background slots.
    fn finish<E>(&self, cell: &SlotCell<E>, ticket: Ticket, value: Option<E>, now: Instant) {
        if cell.complete(ticket, value, now) {
            self.publications.publish(cell.kind.change());
            if let Some(wake) = self.wake.get() {
                wake(cell.kind);
            }
        }
    }
}

/// Instance-owned, nonblocking consent observation: the four probe slots
/// ([`SlotCell`]) and the `[privacy]` mirror. Only a slot's worker invokes
/// its live probe; reads return Unknown/Pending until a current result
/// exists, and an expired grant is never served while refreshing. At most one
/// worker is in flight per slot, across every invalidation. A successor
/// starts with no cached observation, and a rebuild is a successor, so no
/// answer is keyed on the designated requirement. A clone shares the slots:
/// it is how a control worker observes them without a main-thread hop.
#[derive(Clone)]
pub(crate) struct ConsentState {
    probes: ConsentProbes,
    shared: Arc<SharedProbe>,
}

impl ConsentState {
    /// `policy` is the `[privacy]` section the instance starts under, so the
    /// mirror is published before any reader exists.
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

    /// The `[privacy]` mirror as the last admitted config generation
    /// published it. Readable from any thread; the lock is held only for the
    /// `Arc` clone.
    pub(crate) fn policy(&self) -> Arc<ConsentPolicy> {
        self.shared.published_policy()
    }

    /// Publish a config generation's `[privacy]` section. An equal value keeps
    /// the published `Arc`, so a reload that leaves the section alone changes
    /// nothing a reader holds. A changed probe gate also retires the Full Disk
    /// Access answer, as [`Self::invalidate`] does, even when an off/on pair of
    /// reloads lands before any read saw the first. The epoch moves before the
    /// swap, so no read under the new gate is served an answer from the old
    /// one; waiters are told after it, so one sent back to read finds the new
    /// gate.
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

    /// The change counters now. A waiter takes this BEFORE it reads the
    /// state, so a change between its read and its park is not lost.
    pub(crate) fn publication(&self) -> Publication {
        self.shared.publications.snapshot()
    }

    /// Park until a kind in `watch` publishes after `since` or `timeout`
    /// passes; whether one did. Control workers only: the GUI thread never
    /// waits on the consent state.
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

    /// Install once after the GUI event-loop proxy exists: the one completion
    /// wake for every slot, told which. A worker never retains App and calls
    /// it with no lock held.
    pub(crate) fn set_slot_wake(&self, wake: SlotWake) {
        let _ = self.shared.wake.set(wake);
    }

    /// Retire `cell`'s answer, and tell waiters when one was forgotten.
    fn retire<E>(&self, cell: &SlotCell<E>) {
        if cell.retire() {
            self.shared.publications.publish(cell.kind.change());
        }
    }

    /// Activation/policy changes retire every cached or in-flight answer.
    /// The in-flight slot and admission floor survive: focus churn cannot
    /// bypass the worker limit or repeatedly launch quickly completed probes.
    pub(crate) fn invalidate(&self) {
        self.retire(&self.shared.fda);
    }

    /// The census slot's epoch, for the host's wake test.
    #[cfg(test)]
    pub(crate) fn census_generation_for_test(&self) -> u64 {
        self.shared.census.epoch.load(Ordering::Acquire)
    }

    /// Forget the census answer after the bundles on disk changed under it (a
    /// *Move to Trash*). A worker already out keeps nothing, and the next
    /// read dispatches a fresh one without waiting out the retry floor.
    pub(crate) fn invalidate_census(&self) {
        self.retire(&self.shared.census);
        self.shared.census.clear_floor();
    }

    /// The claimant census, never computed on this thread: the last answer
    /// with its age (served while a refresh runs), and whether no answer exists
    /// yet. `(None, false)` (`census=off`) when the posture is disabled, the
    /// instance is inert, or there is no bundle id. The worker obeys the master
    /// switch published when it starts, not the `enabled` given here: with it
    /// off it runs nothing.
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

    /// Retire the debugger answer: window focus gain, and a closed gate. A
    /// worker already out keeps nothing. The in-flight bit and the retry
    /// floor survive, as they do for [`Self::invalidate`], so focus churn can
    /// neither multiply workers nor relaunch a quickly finished probe.
    pub(crate) fn invalidate_debugger(&self) {
        self.retire(&self.shared.debugger);
    }

    /// The debugger row, never computed on this thread. A live instance serves
    /// the last answer with its age and dispatches one worker when there is
    /// none or it is older than [`DEBUGGER_TTL`] (`probe=pending` until the
    /// first answer). A closed gate answers `refused_disabled`; an inert or
    /// unsupported instance answers from its syscall-free arm.
    pub(crate) fn debugger(&self, gate: ProbeGate) -> DebuggerRow {
        if !gate.permits() {
            // Policy wins before a warm answer, and retires it: a lane turned
            // back on asks again rather than serving what it saw before.
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
        // What a pending row says of aterm's own hardening: what a worker
        // already read, never read here, and taken before this call's own
        // worker can read it, so a cold row is `unknown` every time.
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
                    // Responsive: the worker publishes under the slot mutex the
                    // UI thread's reads wait on, and nearly all of its life is
                    // a wait on one short child.
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

    /// The debugger row's cached answer WITHOUT dispatching anything: `None`
    /// while a live instance holds no answer. For the change lane's completion
    /// wake (`Wake::DebuggerProbeReady`), which must never turn a probe that
    /// published nothing — a retired generation — into a new probe. A closed
    /// gate reads `refused_disabled`, and an inert or unsupported instance
    /// answers from its syscall-free arm, exactly as [`Self::debugger`] does.
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

    /// What an inert or unsupported instance's syscall-free arms answer.
    fn inert_debugger_row(&self, gate: ProbeGate) -> DebuggerRow {
        DebuggerRow {
            probe: (self.probes.debugger)(gate),
            self_hardened: (self.probes.hardening)(),
            probe_age_ms: None,
        }
    }

    /// Retire the Developer Tools answer: window focus gain (the owner coming
    /// back from System Settings), a closed gate, and `observer = false`. A
    /// worker already out keeps nothing; the in-flight bit and the retry
    /// floor survive, as for [`Self::invalidate_debugger`]. It starts nothing.
    pub(crate) fn invalidate_devtools(&self) {
        self.retire(&self.shared.devtools);
    }

    /// The Developer Tools row, never computed on this thread, about the
    /// running bundle's identifier. A closed gate answers `refused_disabled`
    /// and `observer_off` (`[privacy] observer = false`) answers
    /// `refused_observer_off`, each retiring a warm answer; an inert or
    /// unsupported instance answers from its spawn-free arm; a warm identity
    /// whose bundle id is unusable answers `refused_no_subject`. A live
    /// instance serves the last answer with its ages and dispatches one
    /// `aterm-devtools-probe` worker when there is none or it is older than
    /// [`DEVTOOLS_TTL`], under the retry floor (`probe=pending` until the
    /// first answer). The reading thread only looks at the identity (the
    /// `subject` arm); when it is cold the worker reads it (`warm_subject`)
    /// and answers `refused_no_subject` only if it holds no usable bundle id.
    /// The worker obeys the section published when it starts, not the `gate`
    /// and `observer_off` given here: under a closed gate or with the observer
    /// off it runs nothing.
    pub(crate) fn devtools(&self, gate: ProbeGate, observer_off: bool) -> DevToolsRow {
        self.devtools_read(gate, observer_off, true)
    }

    /// [`Self::devtools`] WITHOUT dispatching anything: the cached answer, or
    /// `probe=pending` while a live instance holds none. For a Security page
    /// that is not on screen (a background tab, an occluded or minimized
    /// window), which must not keep a `log show` running every
    /// [`DEVTOOLS_TTL`]. The refusals, and the retirement a closed gate or
    /// `observer = false` makes, are [`Self::devtools`]'s, so policy wins on
    /// either read.
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
        // A warm identity says what it is now (`known`); a cold one (`None`)
        // is read on the worker, never here.
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
                            // The switch first: ten milliseconds, against tccd's
                            // log's second.
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

    /// Give a headless test instance live-flagged Developer Tools arms: the
    /// injected probe and subject, and nothing that reaches the OS.
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

    /// Give a headless test instance a live-flagged Full Disk Access arm: the
    /// injected probe, and nothing that reaches the OS.
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

    /// A completed debugger answer, published as its worker would publish it,
    /// for host tests: the arms are flagged live so the slot serves it (until
    /// [`DEBUGGER_TTL`] or a retirement) and a read dispatches nothing. The arms
    /// stay as they were, so a later refresh cannot reach the OS from an inert
    /// state.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn publish_debugger_for_test(
        &mut self,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
    ) {
        self.publish_debugger_aged_for_test(probe, self_hardened, Duration::ZERO);
    }

    /// [`Self::publish_debugger_for_test`], as if the worker had published it
    /// `age` ago.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn publish_debugger_aged_for_test(
        &mut self,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
        age: Duration,
    ) {
        self.probes.live = true;
        // The worker reads the hardening once and keeps it, as here.
        let _ = self.shared.hardening.set(self_hardened);
        let at = Instant::now()
            .checked_sub(age)
            .expect("a monotonic clock that far back");
        self.shared.debugger.seed_for_test(
            DebuggerEntry {
                probe,
                self_hardened,
            },
            at,
        );
    }

    /// No debugger worker out and no answer held: nothing read the rule.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn debugger_slot_idle_for_test(&self) -> bool {
        let slot = self.shared.debugger.slot.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    }

    /// A debugger worker is out.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn debugger_in_flight_for_test(&self) -> bool {
        self.shared.debugger.slot.lock_or_recover().in_flight
    }

    /// The probe arms, for a test in another module that drives a headless
    /// instance through injected ones.
    #[cfg(test)]
    pub(crate) fn probes_for_test(&mut self) -> &mut ConsentProbes {
        &mut self.probes
    }

    /// End the Full Disk Access worker `ticket` names with `probe`, through
    /// the seam every real worker ends through ([`SharedProbe::finish`]).
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

    /// A future-only deadline for an active observer of the Full Disk Access
    /// answer ([`SlotCell::next_refresh_deadline`]); `None` on an inert
    /// instance.
    pub(crate) fn next_refresh_deadline(
        &self,
        now: Instant,
        interval: Duration,
    ) -> Option<Instant> {
        self.probes
            .live
            .then(|| self.shared.fda.next_refresh_deadline(now, interval))
    }

    /// The same deadline for an active observer of the debugger answer, under
    /// [`DEBUGGER_TTL`]; `None` on an inert instance.
    pub(crate) fn next_debugger_refresh_deadline(&self, now: Instant) -> Option<Instant> {
        self.probes.live.then(|| {
            self.shared
                .debugger
                .next_refresh_deadline(now, DEBUGGER_TTL)
        })
    }

    /// A Full Disk Access read at `now` that runs nothing: the policy, then
    /// the slot. With `admit` it may claim the worker, which the caller
    /// spawns. Policy wins BEFORE a warm grant is read, without an OS probe;
    /// a dispatching read also retires the grant, so a lane turned back on
    /// asks again.
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

    /// [`Self::fda_at`] as the event loop's dispatching read takes it: the
    /// seam the bounded-model conformance tests drive.
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

    /// GUI-thread reads never call the live probe or wait for its result,
    /// nor wait on the slot lock ([`SlotLock::Try`]).
    fn fda(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, true, SlotLock::Try)
    }

    /// [`Self::fda`] for a control worker that parks on the publications
    /// while the answer is pending (`privacy`, `await consent`): it waits out
    /// a holder of the slot lock ([`SlotLock::Wait`]).
    pub(crate) fn fda_off_loop(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, true, SlotLock::Wait)
    }

    /// A passive read: the cached answer with its age, or Pending. It never
    /// admits a request, bumps the epoch or retires an entry, so a reader that
    /// cannot act on the verdict (`status`) drives no probe. An entry is
    /// served only under the current epoch and while younger than the
    /// interval [`Self::fda`] would serve it for. A closed gate answers its
    /// refusal and leaves the retirement to the reload hook.
    fn fda_cached(&self, gate: ProbeGate, interval: Duration) -> (FdaProbe, Duration) {
        self.fda_read(gate, interval, false, SlotLock::Try)
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

    /// The four slots the full report reads, under `policy`, about the
    /// running `identity`. Each read may dispatch its slot's bounded worker,
    /// under its TTL and retry floor. Control workers only: the Full Disk
    /// Access read waits out a holder of its lock ([`Self::fda_off_loop`]).
    pub(crate) fn report_slots(
        &self,
        policy: &ConsentPolicy,
        identity: &SigningIdentity,
    ) -> SlotFacts {
        SlotFacts {
            fda: self.fda_off_loop(policy.gate, policy.interval),
            // The census asks about the RUNNING bundle's identifier — never a
            // literal; a build with no readable one gets no census.
            census: self.claimants(identity.bundle_id.as_deref(), policy.enabled),
            // The debugger rule, from its own slot: a full report may dispatch
            // its bounded worker, and `status` never does.
            debugger: self.debugger(policy.gate),
            // The Developer Tools list, from its own slot, about the RUNNING
            // bundle's identifier: only this full report (and the visible
            // Security page) may dispatch its `log show`, never `status`,
            // `await consent` or a poll.
            devtools: self.devtools(policy.gate, policy.observer_off),
        }
    }

    /// Whether a Full Disk Access reading in this window is this aterm's own: `false`
    /// only when macOS names ANOTHER live process responsible for it — the terminal or
    /// launcher that started it in-process (`aterm --window` typed in another
    /// terminal; design §3.8), whose grant the probe then reads, and whose answers a
    /// folder dialog then records. Self, exited (the survivor is its own root, §2.2)
    /// and unknown (no answer from the SPI) keep today's reading. One unprivileged
    /// responsibility query through the injected arm (inert in tests: unknown).
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

    /// A test instance whose responsibility arm answers with `answer`.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn set_responsible_for_test(
        &mut self,
        answer: fn(&[i32]) -> Vec<Result<i32, ResponsibleError>>,
    ) {
        self.probes.responsible = answer;
    }

    /// Synthetic cached verdict for host tests. Even a later refresh uses the
    /// inert function pointer, so this helper cannot contact the OS.
    #[cfg(all(test, unix))]
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

/// What the full report reads from the four probe slots
/// ([`ConsentState::report_slots`]).
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
    /// The slots with no answer yet. A first census, debugger or Developer
    /// Tools read takes the same bounded wait as a cold probe, so a one-shot
    /// report gets the row rather than `pending`.
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

// The Security panel's slice

impl App {
    /// The Security panel's slice of the posture (design §3.4): the same
    /// state `privacy` reports, assembled for a renderer. Never spawns
    /// `codesign`; a cold identity degrades to `DrClass::Unknown`, which
    /// suppresses no repair and promises no durability. The Full Disk Access
    /// read dispatches the probe when the answer is due: the access card, a
    /// focus gain and a Security page on screen keep the cache fresh.
    pub(crate) fn consent_panel_facts(&self) -> ConsentPanelFacts {
        self.panel_facts(true)
    }

    /// [`Self::consent_panel_facts`] WITHOUT dispatching the probe: the cached
    /// verdict, or pending. For a Security page that is not on screen, which
    /// must not keep `aterm-consent-probe` running every
    /// `probe_interval_ms`.
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
        // This hot Settings read needs no protected-root resolution. Preserve
        // the report's master-switch rule for the adoption count directly.
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

/// What the Security panel needs from the consent model, as pure data: a
/// compact projection of [`crate::privacy_report::PrivacySnapshot`] (posture and
/// session counts), with service coverage from
/// [`crate::privacy_report::covers_split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConsentPanelFacts {
    /// `[privacy] enabled`. `false` means aterm asked nothing.
    pub(crate) enabled: bool,
    /// The cached Full Disk Access verdict.
    pub(crate) fda: FdaState,
    /// Why — including every case where no syscall was performed.
    pub(crate) probe: ProbeLabel,
    /// The designated requirement class a grant would be keyed to.
    pub(crate) dr: DrClass,
    /// The RUNNING bundle's id, never a literal and never the release
    /// channel's when this build is not it.
    pub(crate) bundle_id: Option<String>,
    /// The same id as the reset's subject, for the panel's repair plan.
    pub(crate) reset_subject: Option<consent::ResetSubject>,
    /// The install posture token the doctor would classify (`installed`,
    /// `mounted-image`, `translocated`, `not-a-bundle`, `unknown`).
    pub(crate) install: &'static str,
    /// The canonical path this process runs from, when it can be read.
    pub(crate) running: Option<String>,
    /// Live sessions, and how many of them this process took over from a
    /// predecessor across an in-place apply.
    pub(crate) sessions_total: usize,
    pub(crate) sessions_adopted: usize,
}

// The two additive `status` fields (design §5.2)

/// What the `status` record adds for one session: two fields and one
/// `reasons=` token. Additive — `schema=1` does not move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionConsent {
    /// aterm's OWN adoption record for this session.
    pub(crate) attribution: Attribution,
    /// `covered` requires the grant, `attribution=live` and measured coverage;
    /// `denied` requires an EPERM aterm observed for this session; otherwise
    /// `unknown`. With §7 S4 unmeasured, `covered` is unreachable.
    pub(crate) fs_consent: FsConsent,
    /// Whether the `consent_at_risk` reason token is emitted.
    pub(crate) at_risk: bool,
}

impl SessionConsent {
    /// What a session that is not in the pool reports.
    const fn unknown() -> Self {
        Self {
            attribution: Attribution::Unknown,
            fs_consent: FsConsent::Unknown,
            at_risk: false,
        }
    }
}

/// The `[privacy]` section as the consent readers use it. One is resolved per
/// config generation and published on [`ConsentState`] ([`ConsentState::policy`]),
/// so a reader off the event loop needs no hop for it, and the protected roots
/// (`$HOME` plus a join per root) are resolved there and nowhere per read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConsentPolicy {
    /// `enabled` + `check`, as the consent module's own gate.
    pub(crate) gate: ProbeGate,
    /// `probe_interval_ms`.
    pub(crate) interval: Duration,
    /// `report_attribution`, under the master switch.
    pub(crate) report_attribution: bool,
    /// The master switch.
    pub(crate) enabled: bool,
    /// `observer = false`, which also stops the Developer Tools row's
    /// `log show` ([`crate::app_config::Config::privacy_observer_off`]).
    pub(crate) observer_off: bool,
    /// `protected_roots`, resolved ([`crate::app_config::Config::privacy_protected_roots`]).
    protected_roots: Arc<[PathBuf]>,
}

impl ConsentPolicy {
    /// Resolve `config`'s `[privacy]` section. The one place the protected
    /// roots are resolved.
    pub(crate) fn from_config(config: &crate::app_config::Config) -> Self {
        Self::over_roots(config, config.privacy_protected_roots().into())
    }

    /// `config`'s gates over roots already resolved.
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

    /// The resolved protected roots. Empty means `$HOME` was unset with no
    /// absolute override: "cannot answer", never "nothing is protected".
    pub(crate) fn protected_roots(&self) -> &[PathBuf] {
        &self.protected_roots
    }

    /// With the lane off every consent field reads `unknown` ("aterm stopped
    /// looking", not `denied`), including the adoption record.
    pub(crate) const fn adoption(&self, observed: Attribution) -> Attribution {
        if self.enabled {
            observed
        } else {
            Attribution::Unknown
        }
    }
}

/// One session's posture under `policy`: the single join behind `status`'s
/// consent fields and the `privacy` rows. The responsibility SPI is not an
/// input, because it changes neither `attribution` nor `fs_consent`; the report
/// carries it in its own column. No production site observes a per-session
/// `EPERM` yet (the warm-up §3.5 and aterm's own file work §3.6 will), and the
/// join is table-tested on both values.
pub(crate) fn session_posture(
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
    /// The `[privacy]` gates as `self.config` holds them now, over the
    /// protected roots the last admitted config generation published: the
    /// event loop never resolves the roots.
    pub(crate) fn consent_policy(&self) -> ConsentPolicy {
        ConsentPolicy::over_roots(
            &self.config,
            Arc::clone(&self.consent.policy().protected_roots),
        )
    }

    /// The consent half of a config generation, run after the swap: the
    /// mirror is republished, which retires the Full Disk Access answer when
    /// the probe gate changed ([`ConsentState::publish_policy`]).
    pub(crate) fn reconcile_consent_config(&self) {
        self.consent
            .publish_policy(ConsentPolicy::from_config(&self.config));
    }

    /// Set `[privacy]` as an admitted generation does: the section and its
    /// published mirror together. The off-loop readers (`await consent`, the
    /// report's slot reads, every slot worker's gate re-check) read only the
    /// mirror, so a test that assigned `config.privacy` alone would run them
    /// against the policy it replaced.
    #[cfg(test)]
    pub(crate) fn set_privacy_for_test(&mut self, privacy: crate::app_config::PrivacyConfig) {
        self.config.privacy = Some(privacy);
        self.reconcile_consent_config();
    }

    /// Only a watching card/panel should ask for this scheduling hint.
    pub(crate) fn next_consent_probe_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.config.privacy_probe_gate().permits() {
            return None;
        }
        self.consent.next_refresh_deadline(
            now,
            Duration::from_millis(self.config.privacy_probe_interval_ms()),
        )
    }

    /// The consent fields for one session's `status` record. Runs on the event
    /// loop under a poll, so it never blocks: the identity only if warm, and a
    /// `cwd` from the caller, which already holds (or failed to take) the
    /// session's terminal lock. `status` is a passive reader of the Full Disk
    /// Access cache ([`ConsentState::fda_cached`]) and never dispatches a
    /// probe; the card watch, the Security page, `privacy` and `await consent`
    /// keep the cache fresh.
    pub(crate) fn session_consent(&self, session: u64, cwd: Option<&str>) -> SessionConsent {
        let Some(pooled) = self.pool.get(session) else {
            return SessionConsent::unknown();
        };
        let policy = self.consent_policy();
        let (probe, _age) = self.consent.fda_cached(policy.gate, policy.interval);
        // The responsibility SPI is not consulted on this poll path: it
        // changes neither field. The `privacy` verb reports it.
        let posture = session_posture(&policy, session_attribution(pooled), probe.state);
        SessionConsent {
            attribution: posture.attribution,
            fs_consent: posture.fs_consent,
            at_risk: consent_at_risk(posture.fs_consent, cwd, policy.protected_roots()),
        }
    }
}

/// `consent_at_risk` (design §3.6): this session's `fs_consent` is not
/// `covered` and its shell-integration cwd is under a protected root. Not a
/// claim that a dialog is showing; not emitted when the cwd is unknown. Pure
/// and lexical.
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

/// One session's attribution, from aterm's own adoption record (the handoff id
/// stamped on each inherited shell, `spawn::Adopted`), never from the
/// responsibility SPI (design §3.9).
pub(crate) fn session_attribution(session: &crate::Session) -> Attribution {
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
    /// A slot wake that tells `tx` of `kind`'s completions and no other's.
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

    /// The census never runs on the reading thread, an answer is served until
    /// [`CENSUS_TTL`], and `invalidate()` leaves it alone. The fixture counts
    /// its runs, so a served answer shows as no new run.
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

        // A move to the Trash changes the disk: the answer is dropped and the
        // next read dispatches at once, retry floor or not.
        state.invalidate_census();
        let (answer, pending) = state.claimants(Some("x"), true);
        assert!(answer.is_none() && pending);
        wait();
        assert!(state.claimants(Some("x"), true).0.is_some());
        assert_eq!(RUNS.load(Ordering::Acquire), 2);
    }

    /// A census already out when the disk changes publishes nothing: it
    /// describes bundles that may just have moved.
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

    /// No census without a bundle id, with the privacy posture disabled, or on
    /// an inert instance — and never a guessed id.
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

    /// The `consent_at_risk` conjunction (§3.6), and silence on an unknown cwd
    /// (every session without shell integration).
    #[test]
    fn consent_at_risk_is_a_conjunction_and_is_silent_without_a_cwd() {
        let roots = vec![PathBuf::from("/u/me/Protected")];
        let inside = Some("/u/me/Protected/work");
        assert!(consent_at_risk(FsConsent::Unknown, inside, &roots));
        assert!(consent_at_risk(FsConsent::Denied, inside, &roots));
        // `covered` is the one verdict that clears it…
        assert!(!consent_at_risk(FsConsent::Covered, inside, &roots));
        // …and so does a cwd that is not under a protected root.
        assert!(!consent_at_risk(
            FsConsent::Unknown,
            Some("/u/me/src"),
            &roots
        ));
        // NEVER on an unknown cwd, for any verdict.
        for verdict in [FsConsent::Unknown, FsConsent::Denied, FsConsent::Covered] {
            assert!(!consent_at_risk(verdict, None, &roots));
        }
    }

    /// A counting debugger arm for the slot tests: it answers like a Mac with
    /// developer mode off and spawns nothing. Each test owns its own counter.
    fn fake_debugger(runs: &std::sync::atomic::AtomicUsize) -> DebuggerProbe {
        runs.fetch_add(1, Ordering::AcqRel);
        DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

    /// The debugger probe never runs on the reading thread: a cold read is
    /// `probe=pending` and dispatches one worker; an answer is served with its
    /// age within [`DEBUGGER_TTL`]; [`ConsentState::invalidate_debugger`]
    /// retires it and the FDA `invalidate()` does not.
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
        // The retry floor survives focus churn; step past it rather than race.
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

    /// A retired debugger answer forgets the rule, not aterm's own hardening:
    /// once a worker has read it, the pending row a re-probe serves carries it,
    /// so the change lane's log line, the Security row's hedge and
    /// `self_hardened=` do not fall back to `unknown` on a hardened aterm. A
    /// cold row, read before any worker looked, and a closed gate's row (aterm
    /// did not look) still say `unknown`.
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
        // Step past the retry floor so this read dispatches the re-probe.
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

    /// `[privacy] enabled = false`: the debugger row reads `unknown` with
    /// `probe=refused_disabled pending=off` (design §4.1) and consults nothing;
    /// a warm answer is retired, so re-enabling asks again.
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

        // A warm answer does not survive the switch.
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

    /// Only the full report dispatches the debugger worker (design §8 P0 item
    /// 2): the FDA `await consent` read ([`observe_fda`]) and `status`'s
    /// `session_consent` are polled, and neither may start a
    /// `DevToolsSecurity -status` run or read the hardening bits, even with the
    /// gate open and live-flagged counting arms. The positive control is the
    /// same App's `Lines` read, which does dispatch — so the silence before it
    /// is the path, not a closed gate. (`await consent kind=debugger` asks the
    /// slot, under its TTL: see
    /// [`await_consent_kind_debugger_latches_on_a_mode_change_without_a_probe_per_poll`].)
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

        // The positive control: the full report dispatches exactly one.
        let _ = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the full report's worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert!(!slot_idle(&app), "the full report's answer is cached");
    }

    /// `status` is a passive reader of the Full Disk Access cache: no status
    /// field depends on the verdict, so `session_consent` never spawns
    /// `aterm-consent-probe`, even with the gate open, a pooled session and a
    /// live-flagged counting arm, and it serves what a dispatching reader
    /// published. The positive control is one `fda()` read on the same App,
    /// which does dispatch.
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
        // An interval no stalled thread outlives, so the published verdict is
        // still fresh at the last read; expiry at the interval is the slot
        // tests', at explicit instants.
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

        // The positive control: a dispatching read runs the probe once.
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

    /// What the fake Developer Tools arm reports: tccd's newest answer was
    /// `unlisted`, three new programs in the window, the newest at `at_ms`.
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

    /// The subject the fakes ask about.
    fn fake_subject() -> Option<&'static str> {
        Some("com.example.subject")
    }

    /// The Developer Tools probe never runs on the reading thread: a cold read
    /// is `probe=pending` and dispatches one `aterm-devtools-probe` worker for
    /// the subject; an answer is served with both ages within
    /// [`DEVTOOLS_TTL`]; [`ConsentState::invalidate_devtools`] (focus gain)
    /// retires it, and neither the FDA `invalidate()` nor the debugger's does.
    #[test]
    fn the_devtools_probe_runs_on_a_worker_and_focus_gain_retires_it() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static SUBJECT_SEEN: Mutex<Option<String>> = Mutex::new(None);
        fn counting(_gate: ProbeGate, subject: &str) -> DevToolProbe {
            *SUBJECT_SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(subject.to_owned());
            // Two minutes before the clock the row is read with.
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

        state.invalidate_devtools();
        // The retry floor survives focus churn; step past it rather than race.
        state.shared.devtools.clear_floor();
        assert!(state.devtools(ProbeGate::on(), false).pending());
        wait();
        assert!(!state.devtools(ProbeGate::on(), false).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 2);

        // Past the TTL one worker refreshes it, the stale answer served
        // meanwhile.
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

    /// `[privacy] enabled = false` (or `check = false`) makes the row
    /// `refused_disabled`, `[privacy] observer = false` makes it
    /// `refused_observer_off`, and no usable bundle id makes it
    /// `refused_no_subject` (a cold identity only after the worker read it):
    /// each reads `state=unknown`, runs nothing, and a warm answer does not
    /// survive the first two. The same live-flagged counting arm DOES run
    /// once all three allow it, so the silence is the policy.
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
                    && row.ends_with(&format!(" list_probe={label}")),
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
        // A warm identity with an unusable id refuses on the reading thread.
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
        // A cold one is read on the worker, which finds no bundle id.
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

        // The positive control: the same App, everything allowing it, runs it.
        app.consent.probes.subject = fake_subject;
        app.consent.invalidate_devtools();
        app.consent.shared.devtools.clear_floor();
        assert!(row(&app).contains(" probe=pending "), "{}", row(&app));
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the worker publishes");
        assert_eq!(RUNS.load(Ordering::Acquire), 1);
        assert!(row(&app).contains(" probe=observed "), "{}", row(&app));
    }

    /// The identity is read on the Developer Tools worker, never on the
    /// reading thread: with the `subject` arm cold (`None`), as it is on a
    /// window whose access card never ran, the row is `pending`, and the worker
    /// reads the warming arm and runs the probe for that bundle id; the row
    /// then names it. (A warming arm with no bundle id answers
    /// `refused_no_subject`: the master-switch test drives it.)
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

    /// Only the full report (and the visible Security page) dispatches the
    /// Developer Tools worker: `status`'s `session_consent`, the two `await
    /// consent` reads ([`observe_fda`], [`observe_debugger`]),
    /// `debugger_cached` and `devtools_cached` (a hidden Security page's read)
    /// are polled,
    /// and none may start a `log show`, even with the gate open, the observer
    /// on, a subject and a live-flagged counting arm. The positive control is
    /// the same App's `Lines` read, which does dispatch.
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

    /// `await consent kind=debugger` on its read, with a fake probe arm
    /// standing in for `DevToolsSecurity -status`: a cold slot is pending and
    /// arms nothing; the first completed reading is the baseline and cannot
    /// latch; twenty more reads inside `DEBUGGER_TTL` run the probe no further
    /// time and name the answer's expiry as the next refresh; a retired answer
    /// (the owner's change re-probing, or focus gain) is pending again, not a
    /// change; and the re-read that reports another mode latches
    /// `debugger_mode=enabled`. An answer past the TTL is refreshed by one
    /// worker, however many reads ask. None of it publishes a Full Disk Access
    /// change, so the FDA form's waiters are never woken by debugger churn.
    #[test]
    fn await_consent_kind_debugger_latches_on_a_mode_change_without_a_probe_per_poll() {
        use std::sync::atomic::{AtomicU8, AtomicUsize};
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        /// 0 = disabled, 1 = enabled: what the fake `-status` reports.
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
        // Step past the retry floor rather than race it.
        let clear_floor = |app: &crate::App| {
            app.consent.shared.debugger.clear_floor();
        };
        let observe = |app: &crate::App| observe_debugger(&app.consent, Instant::now()).0;
        let mode = |mode| ConsentObservation::Complete(ConsentTuple::Debugger { mode });
        let fda_seq = |app: &crate::App| app.consent.publication().0[ChangeKind::Fda as usize];
        let fda_before = fda_seq(&app);
        let now = Instant::now();
        let mut wait = ConsentWait::new(now, Duration::from_secs(300));

        // Cold: pending, and pending never arms the baseline.
        assert_eq!(observe(&app), ConsentObservation::Pending);
        assert_eq!(
            wait.observe_at(ConsentObservation::Pending, now),
            ConsentWaitDecision::Wait
        );
        published();
        // The first completed reading arms the baseline; it cannot latch.
        let baseline = observe(&app);
        assert_eq!(baseline, mode(DebuggerMode::Disabled));
        assert_eq!(wait.observe_at(baseline, now), ConsentWaitDecision::Wait);
        // Reads inside the TTL are served from the slot: no probe per read,
        // even with the rule changed underneath (only a re-read can see it),
        // and the next refresh is the answer's expiry.
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

        // The owner's change re-probes by retiring the answer: pending again,
        // which is not a change, then the fresh reading latches.
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

        // Past the TTL, one worker refreshes the answer (served, with its age,
        // meanwhile), however many reads ask while it is out.
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

    /// A slot's next refresh is always after the instant asked about: a
    /// current answer's expiry, else the retry floor, else one TTL on, which is
    /// also what a worker out gets (its completion publishes sooner). The
    /// naive rule (expiry, else the floor) would answer a stale answer's PAST
    /// expiry while its refresh runs, and a waiter parked on it would spin.
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

    /// A closed gate and an inert (headless) instance never read the rule, so
    /// `await consent kind=debugger` can only time out there: no reading ever
    /// arms a baseline, nothing is spawned, and no refresh is ever due.
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

    /// `[privacy] enabled = false` makes every consent field read `unknown` —
    /// the honest word for "aterm stopped looking". It must NOT read like a
    /// denial, and it must not leave the adoption record leaking through: the
    /// master switch turns the whole lane off, not just the probe. With the
    /// lane on, `attribution` comes from aterm's handoff record (design §3.9).
    #[test]
    fn the_master_switch_makes_every_consent_field_unknown() {
        let mut app = crate::App::headless_for_test();
        assert_eq!(app.session_consent(0, None).attribution, Attribution::Live);
        // A session that is not in the pool answers `unknown`, not a guess.
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

    /// An admitted config generation republishes the `[privacy]` mirror, roots
    /// resolved, and the event-loop readers judge against the new roots. A
    /// generation that leaves the section alone keeps the published `Arc` (the
    /// negative control: nothing a reader holds changes).
    #[test]
    fn a_config_reload_republishes_the_privacy_mirror() {
        use crate::app_colorscheme::install_resolved_colors::apply_config_text;
        // An absolute root keeps every expectation here independent of the
        // process-global HOME; `/tmp/…` is absolute only on Unix.
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

    /// End the Full Disk Access worker `ticket` names with `probe`, through
    /// the seam every real worker ends through ([`SharedProbe::finish`]).
    fn complete(state: &ConsentState, ticket: Ticket, probe: FdaProbe, at: Instant) {
        state
            .shared
            .finish(&state.shared.fda, ticket, Some(probe), at);
    }

    /// A debugger slot's answer, for the tests that drive a serving slot.
    fn debugger_entry() -> DebuggerEntry {
        DebuggerEntry {
            probe: DebuggerProbe::refused(DebuggerProbeLabel::TimedOut),
            self_hardened: SelfHardening::Unknown,
        }
    }

    /// A Developer Tools slot's answer.
    fn devtools_entry() -> DevToolsEntry {
        DevToolsEntry {
            probe: DevToolProbe::refused(DevToolProbeLabel::TimedOut),
            list: DevToolList::refused(DevToolListLabel::TimedOut),
            subject: None,
        }
    }

    /// The publication model, for a slot that drops a stale answer (`Serve =
    /// 0`, Full Disk Access) or serves it while one worker refreshes it
    /// (`Serve = 1`, the background slots). A cached answer and a worker in
    /// flight exclude each other only where stale answers are dropped.
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

    /// The model under each policy (see [`probe_model`]).
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

    /// The admission floor. `Forget` is the census's retirement after a *Move
    /// to Trash* ([`ConsentState::invalidate_census`]): the bundles on disk
    /// changed, so it ends the floor on purpose and is not an `Invalidate`.
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

    /// Focus churn on any of the four slots, through its real
    /// [`SlotCell::retire`]: however many retirements, none serves a retired
    /// answer, bypasses the retry floor or admits a second worker, and a read
    /// at the floor admits again. The census's *Move to Trash* retirement
    /// also clears the floor: the model's `Forget`, after which a read inside
    /// the floor admits. Read as an `Invalidate`, that same state breaks
    /// `FloorSurvivesInvalidation`, which is the negative control.
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
                // An invalidation that clears the deadline fails the model
                // before another worker could be admitted.
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

    /// The waiter's protocol: the counters are taken BEFORE the read, so a
    /// publication between the read and the park is seen at once, by a park
    /// on this thread or one already parked on another. The negative control
    /// is a snapshot taken after the publication, what a waiter that took it
    /// after its read would hold: its park sees no change.
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

        // The negative control: a snapshot after the publication.
        let late = state.publication();
        assert!(!state.wait_for_change(late, &[ChangeKind::Fda], Duration::ZERO));

        // A waiter parked on `late` on another thread returns on the next one.
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

    /// A publication is an observable change only: a retirement that forgot
    /// nothing publishes nothing, so a read loop under a closed gate cannot
    /// wake its own waiter. The control is in the same test: forgetting a held
    /// answer, a completion and a new policy each publish exactly once.
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
        // A retirement that meets a held lock forgets nothing it can see, so
        // it publishes nothing: two waiters that retire at once cannot wake
        // each other in a loop.
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

    /// `[privacy]` as `privacy` sets it, resolved.
    fn section(privacy: crate::app_config::PrivacyConfig) -> ConsentPolicy {
        ConsentPolicy::from_config(&crate::app_config::Config {
            privacy: Some(privacy),
            ..Default::default()
        })
    }

    /// `[privacy] check = false`, published.
    fn checks_off() -> ConsentPolicy {
        section(crate::app_config::PrivacyConfig {
            check: Some(false),
            ..Default::default()
        })
    }

    /// Every slot worker reads the published section as it starts. A read
    /// that still holds a section a reload replaced (a control worker reads
    /// the mirror, or the `privacy` hop's copy of the config, then
    /// dispatches) admits a worker, which runs nothing the published section
    /// turns off, frees its slot and keeps no answer: Full Disk Access and the
    /// debugger obey the gate, the census the master switch, Developer Tools
    /// the gate and `observer` (its cold identity read included). Every
    /// dispatch holds the open default; the control is that default
    /// published, under which each worker runs its arms once and keeps its
    /// answer.
    #[test]
    fn a_worker_probes_only_under_the_published_gate() {
        use crate::app_config::PrivacyConfig;
        use std::sync::atomic::AtomicUsize;
        static FDA: AtomicUsize = AtomicUsize::new(0);
        static DEBUGGER: AtomicUsize = AtomicUsize::new(0);
        static CENSUS: AtomicUsize = AtomicUsize::new(0);
        // Each of the Developer Tools worker's three arms: the cold identity's
        // read, the switch and the log.
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
        /// Whether a worker is out in `cell`, and whether it holds an answer.
        fn held<E>(cell: &SlotCell<E>) -> (bool, bool) {
            let slot = cell.slot.lock_or_recover();
            (slot.in_flight, slot.entry.is_some())
        }
        let runs = || [&FDA, &DEBUGGER, &CENSUS, &DEVTOOLS].map(|arm| arm.load(Ordering::Acquire));
        // The published section, and the arm runs under it of the FDA,
        // debugger, census and Developer Tools workers.
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
            // The open default a stale snapshot of the section still holds.
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

    /// What a waiter woken by `publish`'s first bump would read: whether the
    /// FDA counter had moved, and the published gate. The publisher runs on
    /// its own thread and is held at the publications lock after its first
    /// bump; both kinds must then be told once the lock is let go.
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

    /// A section with a new probe gate is published before its retirement of
    /// the Full Disk Access answer tells anyone, so a waiter that retirement
    /// wakes reads the new gate and dispatches nothing under the old one. The
    /// negative control is the order a reload used to take (retire, then
    /// publish), whose retirement shows while the replaced gate is still
    /// published. A section that keeps the gate keeps the answer.
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

        // The negative control: retire, then publish.
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

    /// Tier-1: admission and publication are the exact shipping seams, for a
    /// slot that drops a stale answer (Full Disk Access) and one that serves
    /// it (the debugger rule), each against the model under its own policy.
    /// Replay every placement of two invalidations around two requests,
    /// including an invalidation while the first worker is held. A stale
    /// worker retains its slot and cannot publish an answer when it
    /// eventually finishes.
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
                    // Past the TTL, the read that admits the refresh drops a
                    // dropping slot's answer (the model's two decisions,
                    // Expire then Start) and serves a serving slot's.
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

    /// Neither a never-wait read nor a retirement waits on a held slot lock,
    /// for a slot that drops a stale answer and one that serves it: the read
    /// answers pending and claims nothing, and the retirement forgets nothing
    /// it can see. The old answer stays in the slot until the lock is let go,
    /// and is never served after.
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

    /// A retirement that meets a read's held lock forgets nothing and
    /// publishes nothing, so the read itself sees the epoch move: it never
    /// hands out the answer that retirement retired, for a slot that drops a
    /// stale answer and one that serves it. A passive read, or an admitting
    /// one that claimed nothing, reads again. An admitting read that claimed
    /// an expired answer's refresh serves nothing and keeps the claim, moved
    /// to the new epoch, so its worker's answer is kept. The negative control
    /// is one pass of the read under the same interleaving
    /// ([`SlotCell::read_once`]): it serves the retired answer, and its claim
    /// keeps nothing.
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

    /// The full report's read on a control worker
    /// ([`ConsentState::fda_off_loop`]) waits out a holder of the slot lock,
    /// then serves the cached answer: a read that lost to another read is
    /// followed by no publication, so a report parked on the publications
    /// would answer Pending for its whole wait. The event loop's read under
    /// the same held lock answers Pending at once (the negative control).
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
        // A waiting read cannot answer while the lock is held, however long
        // this looks; a read that gave up would answer Pending here.
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

    /// The passive read (`status`) never admits: a cold cache is Pending with
    /// no request; a completed entry is served while younger than the
    /// interval; at the interval, under a closed gate or after a contended
    /// retirement it is not, and none of those reads claims the worker, arms
    /// the floor, moves the epoch or drops the entry. The negative control is
    /// [`ConsentState::read_at`], the dispatching read, at the same instant:
    /// it admits.
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

        // The negative control: the dispatching read at the same instant.
        let (answer, refresh) = state.read_at(ProbeGate::on(), ttl, now + ttl);
        assert_eq!(answer.0.label, ProbeLabel::Pending);
        assert!(
            refresh.is_some(),
            "the dispatching read admits at the interval"
        );

        // A retirement that met a held lock left the entry in place under an
        // old epoch; the passive read refuses it all the same.
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

    /// A thread the OS refuses ([`SlotCell::spawn_failed`]) frees its slot at
    /// the next admitting read and backs it off by the TTL, never under the
    /// floor, so repeated reads cannot spawn in a loop: the Full Disk Access
    /// slot at a zero interval waits out the floor, a serving slot its TTL.
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

    /// Each slot's completion reaches the GUI under its own kind, so a census
    /// answer never ticks the Full Disk Access card: the census worker's wake
    /// carries `Census`, which posts the census nudge. The negative control is
    /// a Full Disk Access completion, which carries `Fda` and posts the card's
    /// wake.
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
        // There is no elapsed-time assertion: reaching the release below while
        // the worker waits is the nonblocking witness, even on a loaded box.
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
