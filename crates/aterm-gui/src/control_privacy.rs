// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl privacy`: the macOS consent posture in one hop, plus the instance
//! half of the two `status` consent fields and the `await consent` predicate
//! (design §5.1–§5.3).
//!
//! Every OS consent question goes through [`ConsentProbes`], injected function
//! pointers with an inert arm: a windowed instance gets the live arms; a
//! headless instance and every unit test get arms that answer `unknown` without
//! a syscall, so no test binary reaches `tccd` or `WindowServer` (`AGENTS.md`,
//! the aterm-census guard `input-probe`). A `--headless` instance still answers the verb,
//! with `full_disk_access=unknown probe=refused_disabled`: not consulted, which
//! is distinct from a denial.
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
//!
//! Grant scope and service coverage (spikes S1, S4) are not claimed
//! (`SpikeEvidence::UNMEASURED`): a completed grant establishes
//! `fda_scope=this_process` and Apple's documented `app-data` coverage for that
//! host, but not inherited-session or per-folder access, so `prompt_possible`
//! stays `yes` and no folder is reported covered. S4 is measured; why its flag
//! stays off is on `SpikeEvidence::fda_coverage_measured`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aterm_types::MutexExt as _;
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    self, Attribution, Claimants, ConsentKey, ConsentPosture, DebuggerMode, DebuggerProbe,
    DebuggerProbeLabel, DevToolList, DevToolListLabel, DevToolProbe, DevToolProbeLabel, DrClass,
    FdaProbe, FdaState, Folder, FsConsent, PostureInputs, ProbeGate, ProbeLabel, Responsible,
    ResponsibleError, SelfHardening, SpikeEvidence, TrashOffer, plist_string, read_plist_text,
};
use aterm_control::wire::{json_ok, json_str_field, pct_encode};
use winit::event_loop::EventLoopProxy;

use crate::control::control_media::MainHopError;
use crate::control::{Scope, cross_session_authorized};
use crate::{App, Wake};

/// The TCC service classes a Full Disk Access grant is claimed (by Apple) to
/// subsume. App-data follows the host's completed grant; others stay
/// `unmeasured` while §7 S4's flag is off, except [`NEVER_COVERED`] (see
/// [`covers_split`]).
const SERVICES: &[&str] = &[
    "documents",
    "desktop",
    "downloads",
    "network-volumes",
    "removable-volumes",
    "app-data",
    "file-provider-domains",
    // The promptable classes Full Disk Access does not subsume, each with its
    // own System Settings pane. They are not folder rows.
    "media-library",
    "photos",
    "app-bundles",
    "contacts",
];

/// The service classes Full Disk Access does not reliably cover
/// (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4). Always `uncovered`.
const NEVER_COVERED: &[&str] = &[
    "file-provider-domains",
    // Separate consent services that the FDA rule does not subsume: the grant
    // does not reach them, so `uncovered`, not `unmeasured`. Contacts is
    // measured: on 2026-10-16 (macOS 27.0.1) a program reading Contacts'
    // container drew a kTCCServiceAddressBook check while the same sweep's
    // Full Disk Access checks were granted.
    "media-library",
    "photos",
    "app-bundles",
    "contacts",
];

/// The two volume classes that appear on the `folder` row beside the three
/// [`Folder`] variants. They have no `$HOME`-relative path, so they carry no
/// resolved `PathBuf` and can only ever be `unknown` here.
const VOLUME_ROWS: &[&str] = &["network-volumes", "removable-volumes"];

/// `await consent`'s default deadline. Finite on purpose: the system dialog it
/// waits behind never expires (§1.4), so an autonomous agent must not park on
/// an absent human forever.
const AWAIT_CONSENT_DEFAULT_MS: u64 = 300_000;

/// The ceiling every wait verb in this file shares with `await`/`ready`.
const AWAIT_MAX_MS: u64 = 600_000;

/// How often `await consent` re-reads the tuple. The observable change is
/// gated by `[privacy] probe_interval_ms`, so this only bounds the tick on
/// top of it.
const AWAIT_CONSENT_TICK: Duration = Duration::from_millis(500);

/// A control caller can wait briefly for the asynchronous check it just
/// requested. GUI reads remain immediate, and a stuck OS call still reports
/// Pending rather than holding a control worker indefinitely.
const PRIVACY_READ_WAIT: Duration = Duration::from_millis(500);
const PRIVACY_READ_TICK: Duration = Duration::from_millis(25);

/// The refusal for a selector on an instance-wide verb.
const NO_SELECTOR: &str = "ERR privacy is instance-wide and takes no selector\n";

/// Internal main-thread reply only; never emitted by the public privacy verb.
/// An absent row still means the target exited, distinct from an unfinished
/// observation. A complete tuple always begins with `fs_consent=`.
const PENDING_CONSENT_TUPLE: &str = "consent-probe-pending";

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
    fda: fn(ProbeGate) -> FdaProbe,
    /// `responsibility_get_pid_responsible_for_pid`, through `dlsym`. Not a
    /// TCC or `WindowServer` contact, but it is still an OS call made once per
    /// live session, so it takes the same fence.
    responsible: fn(i32) -> Result<i32, ResponsibleError>,
    /// The claimant census: every bundle on disk claiming this process's
    /// `CFBundleIdentifier`, with each one's designated requirement. Runs
    /// `codesign` per candidate, so it takes the same fence as the others and
    /// its inert arm reads nothing at all.
    census: fn(&str) -> Claimants,
    /// The debugger rule (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §3.2): `DevToolsSecurity -status` (no dialog) plus the directory
    /// membership query. Spawns a process, so it is fenced; the inert arm
    /// spawns nothing.
    debugger: fn(ProbeGate) -> DebuggerProbe,
    /// This process's own code-signing flags (`csops`): whether a same-user
    /// debugger could attach to aterm itself. A syscall about our own image,
    /// but an OS call all the same, so it is injected like the rest.
    hardening: fn() -> SelfHardening,
    /// The Developer Tools list (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §9.1): one bounded `/usr/bin/log show` of tccd's own answers for the
    /// bundle id given. Spawns a process, so it is fenced; the inert arm
    /// spawns nothing.
    devtools: fn(ProbeGate, &str) -> DevToolProbe,
    /// The same list's SWITCH (debugger design §9.1, O18): the subject's row in
    /// the system TCC.db, one `open` and one bounded `/usr/bin/sqlite3
    /// -readonly`. Spawns a process, so it is fenced; the inert arm opens and
    /// spawns nothing.
    devtools_list: fn(ProbeGate, &str) -> DevToolList,
    /// The bundle id the Developer Tools row asks about: the running bundle's
    /// `CFBundleIdentifier` once the identity is warm, never a literal. Not an
    /// OS question; injected so a test can name a subject without warming the
    /// process-wide identity. The reading thread calls only this one.
    subject: fn() -> Option<&'static str>,
    /// The same bundle id, reading the identity first when nothing has yet:
    /// that read may run `codesign` (50–150 ms), so only the
    /// `aterm-devtools-probe` worker calls it, when [`Self::subject`] found the
    /// identity cold. The inert arms never read it.
    warm_subject: fn() -> Option<&'static str>,
    /// Whether these are the live arms. Read only by `Debug` and tests; the
    /// `observer` row renders the inert arms' labels as `off`
    /// ([`observer_fda_value`]).
    live: bool,
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
            responsible: consent::responsible_pid_detailed,
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

/// The inert responsibility arm.
fn inert_responsible(_pid: i32) -> Result<i32, ResponsibleError> {
    Err(ResponsibleError::Unsupported)
}

// Instance-owned consent state

/// Minimum freshness and timer spacing. A zero configured interval must not
/// turn completion wakes into an endless probe/complete/probe loop. A blocked
/// request owns its worker slot until it returns; timers cannot replace it.
const PROBE_RETRY_FLOOR: Duration = Duration::from_millis(500);

type ProbeWake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
struct ProbeRequest {
    key: ConsentKey,
    epoch: u64,
}

#[derive(Debug)]
struct ProbeEntry {
    request: ProbeRequest,
    probe: FdaProbe,
    completed_at: Instant,
}

#[derive(Debug, Default)]
struct ProbeSlot {
    key: Option<ConsentKey>,
    request_epoch: u64,
    in_flight: bool,
    entry: Option<ProbeEntry>,
    /// Admission floor survives every invalidation, even a changed identity.
    retry_after: Option<Instant>,
}

/// One answer a bounded worker computes off the event loop: at most one worker
/// in flight, a failed spawn cannot loop, and a completed answer is served
/// with its age while a refresh runs. The claimant census and the debugger
/// rule are instances of it.
///
/// `generation` is bumped by [`Self::retire`]; a worker dispatched under an
/// older one publishes nothing. `in_flight` and `retry_after` survive a
/// retirement, so focus churn can neither multiply workers nor relaunch a
/// quickly finished one inside [`PROBE_RETRY_FLOOR`].
#[derive(Debug)]
struct TtlSlot<E> {
    in_flight: bool,
    generation: u64,
    /// The last published answer and the instant it was published.
    entry: Option<(E, Instant)>,
    retry_after: Option<Instant>,
}

impl<E> Default for TtlSlot<E> {
    fn default() -> Self {
        Self {
            in_flight: false,
            generation: 0,
            entry: None,
            retry_after: None,
        }
    }
}

impl<E: Clone> TtlSlot<E> {
    /// The cached answer with its age, and, when that answer is missing or at
    /// least `ttl` old, no worker is out and the retry floor has passed, the
    /// generation of the worker this call claims ([`Self::begin`]). The caller
    /// spawns that worker with no lock held.
    fn serve(&mut self, now: Instant, ttl: Duration) -> (Option<(E, Duration)>, Option<u64>) {
        let answer = self.cached(now);
        let stale = answer.as_ref().is_none_or(|(_, age)| *age >= ttl);
        let admitted = stale && !self.in_flight && !self.retry_after.is_some_and(|at| now < at);
        (answer, admitted.then(|| self.begin(now)))
    }

    /// The cached answer with its age, claiming nothing.
    fn cached(&self, now: Instant) -> Option<(E, Duration)> {
        self.entry
            .as_ref()
            .map(|(value, at)| (value.clone(), now.saturating_duration_since(*at)))
    }
}

impl<E> TtlSlot<E> {
    /// Claim the one worker slot and arm the retry floor; the worker publishes
    /// under the generation returned.
    fn begin(&mut self, now: Instant) -> u64 {
        self.in_flight = true;
        self.retry_after = Some(now + PROBE_RETRY_FLOOR);
        self.generation
    }

    /// The worker's publication: it always releases the slot, and publishes
    /// only under the generation it was dispatched with.
    fn complete(&mut self, generation: u64, value: E, now: Instant) {
        self.in_flight = false;
        if self.generation == generation {
            self.entry = Some((value, now));
        }
    }

    /// Forget the answer; a worker already out publishes nothing.
    fn retire(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entry = None;
    }

    /// The OS refused the worker thread: no worker owns the slot, so release it
    /// and let a later read retry once the floor [`Self::begin`] armed passes.
    fn spawn_failed(&mut self) {
        self.in_flight = false;
    }
}

/// Run `work` on a named worker for a dispatch [`TtlSlot::serve`] claimed. A
/// thread the OS refuses releases the slot ([`TtlSlot::spawn_failed`]).
fn spawn_slot_worker<E>(
    slot: &Mutex<TtlSlot<E>>,
    name: &str,
    work: impl FnOnce() + Send + 'static,
) {
    if std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .is_err()
    {
        slot.lock_or_recover().spawn_failed();
    }
}

/// How long a census answer is served before a read refreshes it. The census
/// is about bundles on disk, which rarely change, and each refresh runs
/// `codesign` once per copy of the app. The census slot is not retired by
/// [`ConsentState::invalidate`] (focus and config do not change bundles on
/// disk).
const CENSUS_TTL: Duration = Duration::from_secs(60);

/// The debugger slot's answer: the probe and aterm's own hardening.
#[derive(Clone, Copy, Debug)]
struct DebuggerEntry {
    probe: DebuggerProbe,
    self_hardened: SelfHardening,
}

/// How long a debugger answer is served before a read refreshes it. The rule
/// changes only when an administrator changes it, and focus gain retires the
/// answer sooner ([`ConsentState::invalidate_debugger`]). Only the `privacy`
/// verb's full report and the Security page dispatch it.
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
    probe: DevToolProbe,
    /// The switch's row in the system TCC.db (`list=`, `list_probe=`).
    list: DevToolList,
    /// The bundle id the row is about (`subject=`), when there is one.
    subject: Option<&'static str>,
    probe_age_ms: Option<u128>,
    answer_age_ms: Option<u64>,
}

impl DevToolsRow {
    /// Nothing learned, for `label`'s reason.
    const fn refused(label: DevToolProbeLabel, subject: Option<&'static str>) -> Self {
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
    probe: DebuggerProbe,
    self_hardened: SelfHardening,
    probe_age_ms: Option<u128>,
}

impl DebuggerRow {
    /// Nothing learned, for `label`'s reason: every fact `unknown`.
    const fn refused(label: DebuggerProbeLabel) -> Self {
        Self {
            probe: DebuggerProbe::refused(label),
            self_hardened: SelfHardening::Unknown,
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
}

/// The debugger probe's age only when a probe actually completed.
fn completed_debugger_age_ms(label: DebuggerProbeLabel, age: Duration) -> Option<u128> {
    (!label.refused() && label != DebuggerProbeLabel::Pending).then_some(age.as_millis())
}

#[derive(Default)]
struct SharedProbe {
    /// Invalidation never waits for the worker or its publication mutex.
    epoch: AtomicU64,
    /// A spawn failure can be published without waiting for the slot mutex.
    failed_request: AtomicU64,
    slot: Mutex<ProbeSlot>,
    census: Mutex<TtlSlot<Claimants>>,
    wake: OnceLock<ProbeWake>,
    debugger: Mutex<TtlSlot<DebuggerEntry>>,
    /// The debugger worker's own completion wake (`Wake::DebuggerProbeReady`),
    /// kept apart from [`Self::wake`] so a debugger answer never ticks the
    /// Full Disk Access card.
    debugger_wake: OnceLock<ProbeWake>,
    /// This process's hardening, read by the first debugger worker and kept:
    /// the two bits it reads are fixed at exec. Never read on the event loop.
    hardening: OnceLock<SelfHardening>,
    devtools: Mutex<TtlSlot<DevToolsEntry>>,
    /// The Developer Tools worker's own completion wake
    /// (`Wake::DevToolsProbeReady`), apart from the other two for the same
    /// reason the debugger's is.
    devtools_wake: OnceLock<ProbeWake>,
}

impl SharedProbe {
    /// The census's publication seam. The worker runs `codesign` with no lock
    /// held and only then takes the mutex.
    fn complete_census(&self, generation: u64, claimants: Claimants, now: Instant) {
        self.census
            .lock_or_recover()
            .complete(generation, claimants, now);
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    /// The debugger worker's publication seam. `DevToolsSecurity` has already
    /// exited when this runs; no syscall happens under the lock.
    fn complete_debugger(
        &self,
        generation: u64,
        probe: DebuggerProbe,
        self_hardened: SelfHardening,
        now: Instant,
    ) {
        self.debugger.lock_or_recover().complete(
            generation,
            DebuggerEntry {
                probe,
                self_hardened,
            },
            now,
        );
        if let Some(wake) = self.debugger_wake.get() {
            wake();
        }
    }

    /// The Developer Tools worker's publication seam. `log` has already exited
    /// when this runs; nothing is spawned under the lock.
    fn complete_devtools(&self, generation: u64, entry: DevToolsEntry, now: Instant) {
        self.devtools
            .lock_or_recover()
            .complete(generation, entry, now);
        if let Some(wake) = self.devtools_wake.get() {
            wake();
        }
    }

    /// Both real workers and the conformance tests publish through this seam.
    /// No syscall or callback executes under the publication lock.
    fn complete(&self, request: &ProbeRequest, probe: FdaProbe, now: Instant) {
        {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            if !slot.in_flight || slot.request_epoch != request.epoch {
                return;
            }
            slot.in_flight = false;
            if request.epoch == self.epoch.load(Ordering::Acquire)
                && slot.key.as_ref() == Some(&request.key)
            {
                slot.entry = Some(ProbeEntry {
                    request: request.clone(),
                    probe,
                    completed_at: now,
                });
            }
        }
        // Even a stale completion releases the one worker slot. Wake the
        // observer so it can ask for the new epoch, rather than staying pending.
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }
}

/// Instance-owned, nonblocking Full Disk Access observation. Only the worker
/// invokes the live FDA probe; reads return Unknown/Pending until a current
/// result exists, and an expired grant is never served while refreshing. At
/// most one worker is in flight per instance, across every invalidation. A
/// successor starts with no cached observation.
pub(crate) struct ConsentState {
    probes: ConsentProbes,
    shared: Arc<SharedProbe>,
}

impl ConsentState {
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            probes: ConsentProbes::for_instance(headless),
            shared: Arc::new(SharedProbe::default()),
        }
    }

    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// Install once after the GUI event-loop proxy exists. Completion never
    /// retains App or invokes the callback while holding its publication lock.
    pub(crate) fn set_completion_wake(&self, wake: ProbeWake) {
        let _ = self.shared.wake.set(wake);
    }

    /// Activation/policy changes retire every cached or in-flight answer.
    /// The in-flight slot and admission floor survive: focus churn cannot
    /// bypass the worker limit or repeatedly launch quickly completed probes.
    pub(crate) fn invalidate(&self) {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut slot) = self.shared.slot.try_lock() {
            slot.entry = None;
        }
    }

    /// The census slot's generation, for the host's wake test.
    #[cfg(test)]
    pub(crate) fn census_generation_for_test(&self) -> u64 {
        self.shared.census.lock_or_recover().generation
    }

    /// Forget the census answer after the bundles on disk changed under it (a
    /// *Move to Trash*). A worker already out publishes nothing, and the next
    /// read dispatches a fresh one without waiting out the retry floor.
    pub(crate) fn invalidate_census(&self) {
        let mut slot = self.shared.census.lock_or_recover();
        slot.retire();
        slot.retry_after = None;
    }

    /// The claimant census, never computed on this thread: the last answer
    /// with its age (served while a refresh runs), and whether no answer exists
    /// yet. `(None, false)` (`census=off`) when the posture is disabled, the
    /// instance is inert, or there is no bundle id.
    pub(crate) fn claimants(
        &self,
        bundle_id: Option<&str>,
        enabled: bool,
    ) -> (Option<(Claimants, Duration)>, bool) {
        let Some(bundle_id) = bundle_id.filter(|_| enabled && self.probes.live) else {
            return (None, false);
        };
        let (answer, dispatch) = self
            .shared
            .census
            .lock_or_recover()
            .serve(Instant::now(), CENSUS_TTL);
        if let Some(generation) = dispatch {
            let shared = Arc::clone(&self.shared);
            let census = self.probes.census;
            let id = bundle_id.to_owned();
            spawn_slot_worker(&self.shared.census, "aterm-claimant-census", move || {
                let answer = census(&id);
                shared.complete_census(generation, answer, Instant::now());
            });
        }
        let pending = answer.is_none();
        (answer, pending)
    }

    /// Install once after the GUI event-loop proxy exists: the debugger
    /// worker's completion wake (`Wake::DebuggerProbeReady`).
    pub(crate) fn set_debugger_wake(&self, wake: ProbeWake) {
        let _ = self.shared.debugger_wake.set(wake);
    }

    /// Retire the debugger answer: window focus gain, and a closed gate. A
    /// worker already out publishes nothing. The in-flight bit and the retry
    /// floor survive, as they do for [`Self::invalidate`], so focus churn can
    /// neither multiply workers nor relaunch a quickly finished probe.
    pub(crate) fn invalidate_debugger(&self) {
        self.shared.debugger.lock_or_recover().retire();
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
            return DebuggerRow {
                probe: (self.probes.debugger)(gate),
                self_hardened: (self.probes.hardening)(),
                probe_age_ms: None,
            };
        }
        let (answer, dispatch) = self
            .shared
            .debugger
            .lock_or_recover()
            .serve(Instant::now(), DEBUGGER_TTL);
        if let Some(generation) = dispatch {
            let shared = Arc::clone(&self.shared);
            let probe = self.probes.debugger;
            let hardening = self.probes.hardening;
            spawn_slot_worker(&self.shared.debugger, "aterm-debugger-probe", move || {
                // Responsive: the worker publishes under the slot mutex the
                // UI thread's `privacy` read contends, and nearly all of its
                // life is a wait on one short child.
                crate::qos::set_self(crate::qos::Role::Responsive);
                let answer = probe(gate);
                let self_hardened = *shared.hardening.get_or_init(hardening);
                shared.complete_debugger(generation, answer, self_hardened, Instant::now());
            });
        }
        answer.map_or(
            DebuggerRow::refused(DebuggerProbeLabel::Pending),
            |(entry, age)| DebuggerRow::from_entry(entry, age),
        )
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
            return Some(DebuggerRow {
                probe: (self.probes.debugger)(gate),
                self_hardened: (self.probes.hardening)(),
                probe_age_ms: None,
            });
        }
        self.shared
            .debugger
            .lock_or_recover()
            .cached(Instant::now())
            .map(|(entry, age)| DebuggerRow::from_entry(entry, age))
    }

    /// Install once after the GUI event-loop proxy exists: the Developer Tools
    /// worker's completion wake (`Wake::DevToolsProbeReady`).
    pub(crate) fn set_devtools_wake(&self, wake: ProbeWake) {
        let _ = self.shared.devtools_wake.set(wake);
    }

    /// Retire the Developer Tools answer: window focus gain (the owner coming
    /// back from System Settings), a closed gate, and `observer = false`. A
    /// worker already out publishes nothing; the in-flight bit and the retry
    /// floor survive, as for [`Self::invalidate_debugger`]. It starts nothing.
    pub(crate) fn invalidate_devtools(&self) {
        self.shared.devtools.lock_or_recover().retire();
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
        let now = Instant::now();
        let answer = if dispatch {
            let (answer, generation) = self
                .shared
                .devtools
                .lock_or_recover()
                .serve(now, DEVTOOLS_TTL);
            if let Some(generation) = generation {
                let shared = Arc::clone(&self.shared);
                let probe = self.probes.devtools;
                let read_list = self.probes.devtools_list;
                let warm_subject = self.probes.warm_subject;
                spawn_slot_worker(&self.shared.devtools, "aterm-devtools-probe", move || {
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
                    shared.complete_devtools(
                        generation,
                        DevToolsEntry {
                            probe: read,
                            list,
                            subject,
                        },
                        Instant::now(),
                    );
                });
            }
            answer
        } else {
            self.shared.devtools.lock_or_recover().cached(now)
        };
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

    /// No Developer Tools worker out and no answer held.
    #[cfg(test)]
    pub(crate) fn devtools_slot_idle_for_test(&self) -> bool {
        let slot = self.shared.devtools.lock_or_recover();
        !slot.in_flight && slot.entry.is_none()
    }

    /// A future-only deadline for an active observer. Completion normally
    /// wakes it sooner; a stalled worker gets at most this bounded polling rate.
    pub(crate) fn next_refresh_deadline(
        &self,
        now: Instant,
        interval: Duration,
    ) -> Option<Instant> {
        if !self.probes.live {
            return None;
        }
        let interval = interval.max(PROBE_RETRY_FLOOR);
        let retry = now + interval;
        let Ok(slot) = self.shared.slot.try_lock() else {
            return Some(retry);
        };
        if slot.in_flight {
            return Some(retry);
        }
        if let Some(entry) = &slot.entry
            && entry.request.epoch == self.shared.epoch.load(Ordering::Acquire)
            && let Some(expires) = entry.completed_at.checked_add(interval)
            && expires > now
        {
            return Some(expires);
        }
        if let Some(at) = slot.retry_after
            && at > now
        {
            return Some(at);
        }
        Some(retry)
    }

    /// Prepare a request without running it. This is the production admission
    /// seam, exercised directly by the bounded-model conformance tests.
    fn read_at(
        &self,
        gate: ProbeGate,
        interval: Duration,
        key: ConsentKey,
        now: Instant,
    ) -> ((FdaProbe, Duration), Option<ProbeRequest>) {
        let interval = interval.max(PROBE_RETRY_FLOOR);
        let pending = (FdaProbe::pending(), Duration::ZERO);
        // Policy wins BEFORE reading a warm grant, without consulting an OS
        // probe. Invalidation is atomic even if a publisher owns the mutex.
        if !gate.permits() || !self.probes.live {
            self.invalidate();
            return ((self.probes.inactive_fda(gate), Duration::ZERO), None);
        }
        let Ok(mut slot) = self.shared.slot.try_lock() else {
            return (pending, None);
        };
        let failed = self.shared.failed_request.swap(0, Ordering::AcqRel);
        if failed != 0 && slot.in_flight && slot.request_epoch == failed {
            slot.in_flight = false;
            slot.retry_after = Some(now + interval);
        }
        if slot.key.as_ref() != Some(&key) {
            self.shared.epoch.fetch_add(1, Ordering::AcqRel);
            slot.key = Some(key.clone());
            slot.entry = None;
        }
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        if let Some(entry) = &slot.entry {
            let age = now.saturating_duration_since(entry.completed_at);
            if entry.request.epoch == epoch && age < interval {
                return ((entry.probe, age), None);
            }
        }
        slot.entry = None;
        if slot.in_flight || slot.retry_after.is_some_and(|at| now < at) {
            return (pending, None);
        }
        slot.in_flight = true;
        slot.request_epoch = epoch;
        slot.retry_after = Some(now + PROBE_RETRY_FLOOR);
        (pending, Some(ProbeRequest { key, epoch }))
    }

    /// GUI-thread reads never call the live probe or wait for its result.
    fn fda(&self, gate: ProbeGate, interval: Duration, dr: &str) -> (FdaProbe, Duration) {
        if !gate.permits() || !self.probes.live {
            self.invalidate();
            return (self.probes.inactive_fda(gate), Duration::ZERO);
        }
        let key = ConsentKey::new(cache_bundle().clone(), dr.to_owned());
        let (answer, request) = self.read_at(gate, interval, key, Instant::now());
        if let Some(request) = request {
            let shared = Arc::clone(&self.shared);
            let probe = self.probes.fda;
            let request_epoch = request.epoch;
            let spawned = std::thread::Builder::new()
                .name("aterm-consent-probe".to_owned())
                .spawn(move || {
                    let answer = probe(gate);
                    shared.complete(&request, answer, Instant::now());
                });
            if spawned.is_err() {
                // Thread creation failed, so no worker owns the slot. Retire
                // it with a backoff; repeated GUI reads cannot spawn in a loop.
                self.shared
                    .failed_request
                    .store(request_epoch, Ordering::Release);
            }
        }
        answer
    }

    fn responsible_answer(&self, pid: i32) -> Result<i32, ResponsibleError> {
        (self.probes.responsible)(pid)
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
        !matches!(
            consent::classify_responsible(me, self.responsible_answer(me)),
            Responsible::Other(_)
        )
    }

    /// A test instance whose responsibility arm answers with `answer`.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn set_responsible_for_test(
        &mut self,
        answer: fn(i32) -> Result<i32, ResponsibleError>,
    ) {
        self.probes.responsible = answer;
    }

    /// Synthetic cached verdict for host tests. Even a later refresh uses the
    /// inert function pointer, so this helper cannot contact the OS.
    #[cfg(all(test, unix))]
    pub(crate) fn with_cached_probe_for_test(probe: FdaProbe) -> Self {
        let mut state = Self::inert();
        state.probes.live = true;
        let key = ConsentKey::new(
            cache_bundle().clone(),
            signing_identity_if_warm().dr_text.clone(),
        );
        let (_, request) =
            state.read_at(ProbeGate::on(), Duration::from_secs(5), key, Instant::now());
        state
            .shared
            .complete(&request.expect("fresh request"), probe, Instant::now());
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
            .slot
            .try_lock()
            .ok()
            .is_none_or(|s| s.entry.is_none());
        f.debug_struct("ConsentState")
            .field("probes_live", &self.probes.live)
            .field("cache_empty", &empty)
            .finish()
    }
}

// The signing identity a grant is keyed to

/// The code identity macOS keys a TCC grant to. Read ONCE per process: the
/// running binary's identity cannot change under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SigningIdentity {
    /// `CFBundleIdentifier` of the running bundle, when there is one.
    bundle_id: Option<String>,
    /// `CFBundleDisplayName` / `CFBundleName`, else the executable basename.
    display_name: Option<String>,
    /// `developer-id` | `adhoc` | `unsigned` | `unknown`.
    signing: &'static str,
    /// The Team ID, when the signature carries one.
    team: Option<String>,
    /// The designated requirement's class.
    dr: DrClass,
    /// The designated requirement verbatim — part of the probe cache key,
    /// because a grant is bound to it and a rebuild that changes it must
    /// invalidate a cached `granted`.
    dr_text: String,
    /// Whether the bundle carries the `ATermDevBuild` mark.
    dev_build: Option<bool>,
}

impl SigningIdentity {
    /// Everything unknown — what a caller outside a `.app` reports, and the
    /// value the event loop reads while the identity is not yet warm.
    fn unknown() -> &'static Self {
        static UNKNOWN: OnceLock<SigningIdentity> = OnceLock::new();
        UNKNOWN.get_or_init(|| Self {
            bundle_id: None,
            display_name: None,
            signing: "unknown",
            team: None,
            dr: DrClass::Unknown,
            dr_text: String::new(),
            dev_build: None,
        })
    }
}

/// The running executable, resolved once. Pure path resolution — no consent
/// surface, no `WindowServer`.
fn running_exe() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().unwrap_or_default())
}

/// What the probe cache is keyed on: the `.app` root, else the executable.
fn cache_bundle() -> &'static PathBuf {
    static BUNDLE: OnceLock<PathBuf> = OnceLock::new();
    BUNDLE.get_or_init(|| consent::cache_bundle_for(running_exe()))
}

/// The running process's signing identity, read at most once, on the control
/// thread (`cmd_privacy` warms it before the main-thread hop): it may spawn
/// `codesign`, a 50–150 ms stall. It touches neither `tccd` nor `WindowServer`,
/// and does nothing unless the executable is inside a `.app`.
pub(crate) fn signing_identity() -> &'static SigningIdentity {
    identity_cell().get_or_init(read_signing_identity)
}

/// The identity only if already read. The event loop uses this: until a
/// control-thread verb warmed it, main-thread paths see
/// [`SigningIdentity::unknown`].
fn signing_identity_if_warm() -> &'static SigningIdentity {
    identity_cell()
        .get()
        .unwrap_or_else(SigningIdentity::unknown)
}

/// Whether the identity has been read (off the event loop) by anything yet.
pub(crate) fn signing_identity_is_warm() -> bool {
    identity_cell().get().is_some()
}

/// [`running_bundle_id`] for the event loop: `None` until the identity has
/// been read off it.
pub(crate) fn running_bundle_id_if_warm() -> Option<&'static str> {
    signing_identity_if_warm().bundle_id.as_deref()
}

/// The designated requirement's class for the event loop: `DrClass::Unknown` until
/// the identity has been read off it (which `grant_stable` treats as unstable).
pub(crate) fn running_dr_class_if_warm() -> DrClass {
    signing_identity_if_warm().dr
}

/// Whether the running executable sits inside a `.app` bundle: a pure path
/// test (no file is read, nothing spawns), so the event loop may ask it before
/// the identity is warm. An unbundled run has no bundle id to be a dialog's
/// subject.
pub(crate) fn running_from_bundle() -> bool {
    consent::app_bundle_root(running_exe()).is_some()
}

fn identity_cell() -> &'static OnceLock<SigningIdentity> {
    static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
    &IDENTITY
}

/// The running bundle's `CFBundleIdentifier`, or `None` when the executable is
/// not inside a `.app`, its `Info.plist` names none or cannot be read, or the
/// bundle sits under a protected root, whose `Info.plist` aterm declines to
/// read (`read_signing_identity`). Callers omit the claim on `None`; never
/// substitute the release channel's id (design §3.1). It reads the identity
/// when nothing has yet, so never on the event loop.
pub(crate) fn running_bundle_id() -> Option<&'static str> {
    signing_identity().bundle_id.as_deref()
}

fn read_signing_identity() -> SigningIdentity {
    let exe = running_exe();
    let Some(root) = consent::app_bundle_root(exe) else {
        return SigningIdentity::unknown().clone();
    };
    // An `Info.plist` under a protected root is exactly the read this design
    // exists to avoid, so both reads refuse it and the identity is `unknown`.
    let protected = consent::protected_roots(&[]);
    let Some(identity) = consent::bundle_identity(&root, &protected) else {
        return SigningIdentity::unknown().clone();
    };
    let text = consent::readable_info_plist(exe, &protected)
        .as_deref()
        .and_then(read_plist_text);
    let display_name = text
        .as_deref()
        .and_then(|t| plist_string(t, "CFBundleDisplayName").or(plist_string(t, "CFBundleName")))
        .map(str::to_owned);
    SigningIdentity {
        bundle_id: identity.bundle_id,
        display_name,
        signing: identity.signing,
        team: identity.team,
        dr: identity.dr,
        dr_text: identity.dr_text,
        // The updater's own reading of the `ATermDevBuild` mark (fails open).
        #[cfg(target_os = "macos")]
        dev_build: text.as_deref().map(aterm_update::plist_marks_dev_build),
        // No `.app` bundle exists off macOS, so no build is marked there.
        #[cfg(not(target_os = "macos"))]
        dev_build: text.as_deref().map(|_| false),
    }
}

/// The host OS version (`ProductVersion`), read once from the public system
/// plist. Not a consent surface and not a `WindowServer` contact: an ordinary
/// read of a world-readable file under `/System`.
fn os_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            if !cfg!(target_os = "macos") {
                return None;
            }
            let text = read_plist_text(Path::new(
                "/System/Library/CoreServices/SystemVersion.plist",
            ))?;
            plist_string(&text, "ProductVersion").map(str::to_owned)
        })
        .as_deref()
}

// The snapshot

/// One live session's consent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRow {
    sid: String,
    attribution: Attribution,
    /// The raw pid the responsibility SPI named, when it named one.
    responsible_pid: Option<i32>,
    responsible: Responsible,
    fs_consent: FsConsent,
    /// The shell-integration cwd, unencoded. `None` when unreported.
    cwd: Option<String>,
}

/// Where the Full Disk Access question stands, for the `access_card` line of
/// the report (`consent_card::report_state` and the record). Pure data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AccessCardReport {
    /// `quiet`, `asked`, `opened`, `granted` or `declined`.
    pub(crate) state: &'static str,
    /// The four unix-second facts the record holds.
    pub(crate) record: crate::consent_card::AccessRecord,
    /// Need events counted in this process (dialogs macOS showed for a program
    /// of this aterm, or refused reads).
    pub(crate) needs: u32,
}

impl AccessCardReport {
    /// When the question may be asked again: a week after it was asked.
    fn renew_at(&self) -> Option<u64> {
        self.record
            .asked_at
            .map(|at| at.saturating_add(crate::consent_card::RENEW_AFTER.as_secs()))
    }

    fn opt(value: Option<u64>) -> String {
        value.map_or_else(|| "-".to_string(), |v| v.to_string())
    }

    /// The `access_card` row.
    fn line(&self) -> String {
        format!(
            "access_card state={} asked_at={} opened_at={} verified_at={} renew_at={} needs={}",
            self.state,
            Self::opt(self.record.asked_at),
            Self::opt(self.record.opened_at),
            Self::opt(self.record.verified_at),
            Self::opt(self.renew_at()),
            self.needs,
        )
    }

    /// The `access_card` object of the JSON form (flat keys).
    fn json(&self) -> String {
        let num = |value: Option<u64>| value.map_or_else(|| "null".to_string(), |v| v.to_string());
        format!(
            "{{{},\"asked_at\":{},\"opened_at\":{},\"verified_at\":{},\"renew_at\":{},\"needs\":{}}}",
            json_str_field("state", self.state),
            num(self.record.asked_at),
            num(self.record.opened_at),
            num(self.record.verified_at),
            num(self.renew_at()),
            self.needs,
        )
    }
}

/// The whole posture, assembled once and rendered two ways. Pure data: the
/// renderers below are total functions of it, so the wire format is testable
/// without an event loop, a window, or an OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrivacySnapshot {
    platform: &'static str,
    os: Option<String>,
    identity: SigningIdentity,
    /// The macOS install posture the first-open doctor classifies
    /// (`aterm_update::which_copy::posture_from`), as a token. Other platforms
    /// run a bare executable and report `not-a-bundle`.
    install: &'static str,
    /// The canonical path this process runs from, when it can be read.
    running: Option<String>,
    /// Whether macOS can still find the code this process's grants are keyed
    /// to. `running=` is the exec-time path, which reads healthy even after the
    /// bundle was renamed or deleted under the process (an in-place apply).
    anchor: aterm_containment::ImageAnchor,
    fda: FdaState,
    probe: ProbeLabel,
    probe_age_ms: Option<u128>,
    evidence: SpikeEvidence,
    attribution_root: Attribution,
    sessions: Vec<SessionRow>,
    containment_mode: String,
    seatbelt: &'static str,
    protected: Vec<String>,
    warmup: &'static str,
    warmup_last_ms: Option<u128>,
    /// The debugger rule (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
    /// §4.1): an Authorization Services right, not a TCC grant, so it joins no
    /// coverage bucket and never feeds `prompt_possible`.
    debugger: DebuggerRow,
    /// The Developer Tools list (debugger design §9.1): tccd's newest answer
    /// for this bundle id, read from tccd's own log. TCC, but never a file
    /// grant, so it joins no coverage bucket and never feeds `prompt_possible`.
    devtools: DevToolsRow,
    /// Per-service folder state, in `folder_names` order, and its source.
    folders: Vec<(&'static str, &'static str)>,
    folder_source: String,
    observer_fda: &'static str,
    observer_responsible: &'static str,
    /// The `observer log=` token: `off` unless `[privacy] observer` asked.
    observer_log: &'static str,
    /// Where the Full Disk Access question stands (`access_card` row).
    access_card: AccessCardReport,
    /// Every bundle on disk claiming this process's `CFBundleIdentifier`
    /// (`aterm_containment::consent`, "The claimant census") and the answer's
    /// age. The only row about the TCC record rather than this process.
    claimants: Option<(Claimants, Duration)>,
    /// No census answer exists yet — `census=pending`, as opposed to `off`.
    census_pending: bool,
    /// Whether `/usr/bin/trash` is on this Mac: without it the Security panel
    /// offers no move, and no row reads `trash=offered`.
    trash_tool: bool,
}

/// The `covers=` / `uncovered=` / `unmeasured=` split; "not measured" is not
/// "measured as not covered". App-data uses Apple's documented FDA rule and the
/// host's confirmed grant; other services stay unmeasured while §7 S4's flag is off.
/// [`NEVER_COVERED`] stays separate regardless.
pub(crate) fn covers_split(fda: FdaState, evidence: SpikeEvidence) -> CoverageSplit {
    let never = |s: &&&str| NEVER_COVERED.contains(s);
    let uncovered: Vec<&'static str> = SERVICES.iter().filter(never).copied().collect();
    let rest: Vec<&'static str> = SERVICES.iter().filter(|s| !never(s)).copied().collect();
    let mut split = if evidence.fda_coverage_measured {
        CoverageSplit {
            covers: rest,
            uncovered,
            unmeasured: Vec::new(),
        }
    } else {
        CoverageSplit {
            covers: Vec::new(),
            uncovered,
            unmeasured: rest,
        }
    };
    // Apple documents app-container access as an FDA-covered class (WWDC23,
    // "What's new in privacy"). A completed grant is positive evidence for
    // the observing host; it is not proof about adopted sessions or folders.
    split.covers.retain(|service| *service != "app-data");
    split.unmeasured.retain(|service| *service != "app-data");
    if app_data_covered(fda, observed_fda_scope(fda, evidence.fda_scope)) {
        split.covers.push("app-data");
    } else {
        split.unmeasured.push("app-data");
    }
    split
}

/// Successful access establishes the observing host's scope directly. It does
/// not establish when a changed Settings grant reaches another process.
pub(crate) const fn observed_fda_scope(
    fda: FdaState,
    measured: consent::FdaScope,
) -> consent::FdaScope {
    if matches!(fda, FdaState::Granted) && matches!(measured, consent::FdaScope::Unknown) {
        consent::FdaScope::ThisProcess
    } else {
        measured
    }
}

const fn app_data_covered(fda: FdaState, scope: consent::FdaScope) -> bool {
    matches!(fda, FdaState::Granted)
        && matches!(
            scope,
            consent::FdaScope::ThisProcess | consent::FdaScope::NewProcesses
        )
}

/// The three coverage buckets of [`covers_split`]. Shared with the Security
/// panel, so the verb and the panel can never disagree about a service.
pub(crate) struct CoverageSplit {
    pub(crate) covers: Vec<&'static str>,
    pub(crate) uncovered: Vec<&'static str>,
    pub(crate) unmeasured: Vec<&'static str>,
}

/// [`install_posture_rows`], computed once: the executable path of a running
/// process does not change, and the Security panel reads this on every park
/// while it is open.
fn install_posture_once() -> &'static (&'static str, Option<String>) {
    static ONCE: std::sync::OnceLock<(&'static str, Option<String>)> = std::sync::OnceLock::new();
    ONCE.get_or_init(install_posture_rows)
}

/// The `install=` token and `running=` path: the first-open doctor's
/// classification (`aterm_update::which_copy::posture_from`) of this process's
/// canonical executable path on macOS; `not-a-bundle` elsewhere. No TCC.
/// Canonicalized on unix only: Windows answers a verbatim `\\?\C:\…` path.
fn install_posture_rows() -> (&'static str, Option<String>) {
    let Ok(exe) = std::env::current_exe() else {
        return ("unknown", None);
    };
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    // `which_copy::posture_from` is macOS-only (translocation and mounted
    // images are Gatekeeper/DMG phenomena). Elsewhere the running copy is a
    // plain binary: `InstallPosture::NotABundle`.
    #[cfg(target_os = "macos")]
    let token = match aterm_update::which_copy::posture_from(&exe) {
        aterm_update::which_copy::InstallPosture::Installed => "installed",
        aterm_update::which_copy::InstallPosture::MountedImage => "mounted-image",
        aterm_update::which_copy::InstallPosture::Translocated => "translocated",
        aterm_update::which_copy::InstallPosture::NotABundle => "not-a-bundle",
    };
    #[cfg(not(target_os = "macos"))]
    let token = "not-a-bundle";
    (token, Some(exe.to_string_lossy().into_owned()))
}

/// The `observer fda=` value: `off` when deliberately not consulted,
/// `unavailable` when permitted but unable to run, `ok` when a syscall
/// answered.
fn observer_fda_value(probe: ProbeLabel) -> &'static str {
    match probe {
        ProbeLabel::RefusedDisabled => "off",
        ProbeLabel::Pending => "pending",
        label if label.refused() => "unavailable",
        _ => "ok",
    }
}

fn completed_probe_age_ms(label: ProbeLabel, age: Duration) -> Option<u128> {
    (!label.refused() && label != ProbeLabel::Pending).then_some(age.as_millis())
}

/// The `observer responsible=` value, on the same three-valued vocabulary.
fn observer_responsible_value(answers: &[Result<i32, ResponsibleError>]) -> &'static str {
    if answers
        .iter()
        .any(|a| matches!(a, Err(ResponsibleError::Unsupported)))
    {
        return "off";
    }
    if answers
        .iter()
        .any(|a| matches!(a, Err(ResponsibleError::SymbolUnavailable)))
    {
        return "unavailable";
    }
    if answers.is_empty() { "off" } else { "ok" }
}

/// `Responsible` on the wire: its token, or the decimal pid for `Other`.
fn responsible_token(responsible: Responsible) -> String {
    responsible.token().map_or_else(
        || {
            responsible
                .pid()
                .map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
        },
        str::to_owned,
    )
}

/// `-` for unset, pct-encoded otherwise. Free text on this wire is always
/// encoded, so a cwd with a space can never split a field.
fn opt(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_string(), pct_encode)
}

/// The closing prose row: what a `folder` value of `unknown` means, which a
/// reader otherwise takes for a denial.
const NOTE: &str =
    "unknown means not observed, not denied: reading a folder is what raises the prompt";

/// The closing `note` off macOS, in [`NOTE`]'s place: there is no consent layer to
/// prompt, so an EPERM is the file's own permission.
const NOTE_NO_OS_CONSENT: &str = "this platform has no OS consent layer: nothing prompts, and an \
     Operation not permitted is an ordinary file permission";

/// The `remediate` row's places, in its order (fda, files, debugger, devtools).
const REMEDIATE: [&str; 4] = [
    "settings:Privacy_AllFiles",
    "settings:Privacy_FilesAndFolders",
    DEBUGGER_REMEDIATE,
    DEVTOOLS_REMEDIATE,
];

/// `pending=` on the `debugger` row and `taskport=` on the `observer` row: the
/// authd lane that would fill them (design §3.7, P2) does not exist, so `off`
/// (not watching), never `no`. The fields it would fill read unset.
const TASKPORT_WATCH: &str = "off";

/// `remediate debugger=`: a PLACE where a human acts, never a command (commit
/// d034733c6 removed `reset=` for handing in-session agents a command). It is
/// the place the design names (§4.1): aterm's own Settings ▸ Security, whose
/// debugger row carries the owner's "Allow debugger access…" press — confirmed
/// in aterm's alert, after which macOS should ask for an administrator in its
/// own dialog (spike D2 has not measured it); an agent can answer neither. An enable token here would be the hazard `reset=` was.
const DEBUGGER_REMEDIATE: &str = "aterm-settings:security";

/// `remediate devtools=`: a PLACE, never a command. The Developer Tools list
/// changes only in System Settings, by a human (`+`, then the switch); aterm
/// adds nothing to it, and no command a program could run does either.
const DEVTOOLS_REMEDIATE: &str = "settings:Privacy_DevTools";

impl PrivacySnapshot {
    /// The census rows: a summary, then one line per conflicting copy (at most
    /// `MAX_CLAIMANT_ROWS`; the summary keeps true totals). A copy sharing the
    /// running requirement cannot reset a grant, so it is counted, not listed.
    fn claimant_lines(&self) -> Vec<String> {
        let Some((census, age)) = &self.claimants else {
            let state = if self.census_pending {
                "pending"
            } else {
                "off"
            };
            return vec![format!("claimants=- conflicting=- census={state}")];
        };
        let conflicting = census.conflicting();
        let mut out = vec![format!(
            "claimants={} conflicting={} census={} sole={} age_ms={}",
            census.found.len(),
            conflicting.len(),
            census.enumeration.as_str(),
            yes_no(census.sole_claimant()),
            age.as_millis(),
        )];
        // Each row says whether the panel's button takes the copy, and if not
        // why, by the rule the button reads (`TrashOffer`).
        let listed = census.listed(self.trash_tool);
        for (claimant, offer) in &listed {
            out.push(format!(
                "claimant path={} signing={} dr={} team={} conflicts=yes trash={}",
                pct_encode(&claimant.path.to_string_lossy()),
                claimant.signing,
                claimant.dr.as_str(),
                opt(claimant.team.as_deref()),
                offer.as_str(),
            ));
        }
        if conflicting.len() > consent::MAX_CLAIMANT_ROWS {
            out.push(format!(
                "claimant truncated={} of={}",
                conflicting.len() - consent::MAX_CLAIMANT_ROWS,
                conflicting.len()
            ));
        }
        if !conflicting.is_empty() {
            // The note names the Trash only beside a row that reads offered, and
            // points at those rows rather than restating the rule.
            let n = conflicting.len();
            let (copies, them, marked) = if n == 1 {
                ("1 other copy of aterm is".to_string(), "it", "it")
            } else {
                (
                    format!("{n} other copies of aterm are"),
                    "them",
                    "the ones marked trash=offered",
                )
            };
            let remedy = if listed
                .iter()
                .any(|(_, offer)| *offer == TrashOffer::Offered)
            {
                format!(
                    "Settings \u{25b8} Security lists {them} and offers to move {marked} to the \
                     Trash"
                )
            } else {
                format!("Settings \u{25b8} Security lists {them}")
            };
            out.push(format!(
                "note {}",
                pct_encode(&format!(
                    "{copies} signed differently; whichever asks macOS last resets access for \
                     every copy; {remedy}"
                ))
            ));
        }
        out
    }

    /// Whether the snapshot's platform has an OS consent layer (TCC): macOS
    /// alone. Keyed on the snapshot's own `platform`, so a macOS fixture reads
    /// the same on any host. Off it, the report keeps its schema and row order
    /// and says what is true: no prompt is possible, no folder is protected by
    /// the OS, there is no Settings pane to send anyone to.
    fn os_consent(&self) -> bool {
        self.platform == "macos"
    }

    /// The `remediate` places, `None` where the platform has no consent layer
    /// ([`Self::os_consent`]): a System Settings pane on Linux is no remedy.
    fn remediate(&self) -> [Option<&'static str>; 4] {
        REMEDIATE.map(|place| self.os_consent().then_some(place))
    }

    /// The `protected=` folders: none where the platform has no consent layer.
    fn protected(&self) -> &[String] {
        if self.os_consent() {
            &self.protected
        } else {
            &[]
        }
    }

    /// The closing note ([`NOTE`], or [`NOTE_NO_OS_CONSENT`] off macOS).
    fn note(&self) -> &'static str {
        if self.os_consent() {
            NOTE
        } else {
            NOTE_NO_OS_CONSENT
        }
    }

    /// `prompt_possible` — the module's own rule, not a second copy of it, and
    /// never where no consent layer exists to prompt (Linux read `yes` because
    /// its Full Disk Access is always `unknown`).
    fn prompt_possible(&self) -> bool {
        self.os_consent()
            && ConsentPosture::join(PostureInputs {
                adoption: Attribution::Live,
                fda: self.fda,
                responsible: Responsible::Unknown,
                observed_eperm: false,
                evidence: self.evidence,
            })
            .prompt_possible
    }

    fn sessions_adopted(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| matches!(s.attribution, Attribution::Adopted))
            .count()
    }

    /// The line body. `OK <n>` counts these rows; every live session adds
    /// exactly one, with no truncation, so `sessions_total=` equals the number
    /// of `session` rows.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.sessions.len() + 14);
        out.push("schema=1".to_string());
        out.push(format!(
            "platform={} os={}",
            self.platform,
            opt(self.os.as_deref())
        ));
        out.push(format!(
            "bundle_id={} display_name={} signing={} team={} dr={} grant_stable={} dev_build={}",
            opt(self.identity.bundle_id.as_deref()),
            opt(self.identity.display_name.as_deref()),
            self.identity.signing,
            opt(self.identity.team.as_deref()),
            self.identity.dr.as_str(),
            yes_no(self.identity.dr.grant_stable()),
            self.identity
                .dev_build
                .map_or_else(|| "-".to_string(), |d| d.to_string()),
        ));
        // Where this process runs from: a grant is keyed to the bundle at a
        // path, so a translocated or mounted-image posture cannot use one.
        out.push(format!(
            "install={} running={} anchor={}",
            self.install,
            opt(self.running.as_deref()),
            self.anchor.as_str()
        ));
        out.push(format!(
            "full_disk_access={} probe={} probe_age_ms={} fda_scope={}",
            self.fda.as_str(),
            self.probe.as_str(),
            self.probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            observed_fda_scope(self.fda, self.evidence.fda_scope).as_str(),
        ));
        let split = covers_split(self.fda, self.evidence);
        out.push(format!("covers={}", list_or_dash(&split.covers)));
        out.push(format!("uncovered={}", list_or_dash(&split.uncovered)));
        out.push(format!("unmeasured={}", list_or_dash(&split.unmeasured)));
        let mut folder = String::from("folder");
        for (name, state) in &self.folders {
            folder.push(' ');
            folder.push_str(name);
            folder.push('=');
            folder.push_str(state);
        }
        folder.push_str(" source=");
        folder.push_str(&self.folder_source);
        out.push(folder);
        out.extend(self.claimant_lines());
        out.push(format!(
            "prompt_possible={}",
            yes_no(self.prompt_possible())
        ));
        out.push(format!(
            "attribution_root={} sessions_total={} sessions_adopted={}",
            self.attribution_root.as_str(),
            self.sessions.len(),
            self.sessions_adopted(),
        ));
        for s in &self.sessions {
            out.push(format!(
                "session sid={} attribution={} responsible_pid={} responsible={} fs_consent={} \
                 cwd={}",
                pct_encode(&s.sid),
                s.attribution.as_str(),
                s.responsible_pid
                    .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                responsible_token(s.responsible),
                s.fs_consent.as_str(),
                opt(s.cwd.as_deref()),
            ));
        }
        out.push(format!(
            "containment mode={} seatbelt={} protected={}",
            self.containment_mode,
            self.seatbelt,
            if self.protected().is_empty() {
                "-".to_string()
            } else {
                self.protected()
                    .iter()
                    .map(|p| pct_encode(p))
                    .collect::<Vec<_>>()
                    .join(",")
            },
        ));
        out.push(format!(
            "warmup={} warmup_last_ms={}",
            self.warmup,
            self.warmup_last_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        ));
        out.push(format!(
            "debugger mode={} member={} self_hardened={} right={} probe={} probe_age_ms={} \
             pending={TASKPORT_WATCH} pending_right=- pending_age_ms=- last=- watch_ms=-",
            self.debugger.probe.mode.as_str(),
            self.debugger.probe.member.as_str(),
            self.debugger.self_hardened.as_str(),
            consent::TASKPORT_DEBUG_RIGHT,
            self.debugger.probe.label.as_str(),
            self.debugger
                .probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        ));
        let num = |value: Option<u64>| value.map_or_else(|| "-".to_string(), |n| n.to_string());
        let devtools = self.devtools.probe;
        out.push(format!(
            "devtools state={} subject={} requests={} not_allowed={} answer_age_ms={} \
             window_s={} probe={} probe_age_ms={} grant={} list={} list_probe={}",
            devtools.state.as_str(),
            opt(self.devtools.subject),
            num(devtools.requests.map(u64::from)),
            num(devtools.not_allowed.map(u64::from)),
            num(self.devtools.answer_age_ms),
            consent::DEVTOOL_WINDOW_S,
            devtools.label.as_str(),
            self.devtools
                .probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            self.devtools.grant().as_str(),
            self.devtools.list.listing.as_str(),
            self.devtools.list.label.as_str(),
        ));
        out.push(format!(
            "observer fda={} responsible={} log={} taskport={TASKPORT_WATCH}",
            self.observer_fda, self.observer_responsible, self.observer_log,
        ));
        out.push(self.access_card.line());
        let [fda, files, debugger, devtools] = self
            .remediate()
            .map(|place| place.map_or_else(|| "-".to_string(), pct_encode));
        out.push(format!(
            "remediate fda={fda} files={files} debugger={debugger} devtools={devtools}"
        ));
        // A grant that cannot be validated: `full_disk_access=` gates on the
        // exec-time path, which a swap leaves pointing at a healthy bundle, so
        // say it on its own row.
        if self.anchor.grant_unverifiable() {
            let gone = if self.anchor == aterm_containment::ImageAnchor::Deleted {
                "deleted"
            } else {
                "renamed"
            };
            out.push(format!(
                "note {}",
                pct_encode(&format!(
                    "the bundle this process started from was {gone}, so no grant can match \
                     it: macOS asks again for everything, whatever full_disk_access= says, and \
                     its requests clear access for every copy of aterm"
                ))
            ));
        }
        out.push(format!("note {}", pct_encode(self.note())));
        out
    }

    /// This session's consent tuple — the `await consent` baseline and the
    /// value it latches a change against.
    fn tuple_line(row: &SessionRow, fda: FdaState) -> String {
        format!(
            "fs_consent={} fda={} attribution={}",
            row.fs_consent.as_str(),
            fda.as_str(),
            row.attribution.as_str(),
        )
    }

    /// Internal control bridge: unfinished probes have no consent tuple yet.
    fn observed_tuple_line(row: &SessionRow, probe: FdaProbe) -> String {
        if probe.label == ProbeLabel::Pending {
            PENDING_CONSENT_TUPLE.to_string()
        } else {
            Self::tuple_line(row, probe.state)
        }
    }
}

/// The debugger tuple `await consent kind=debugger` arms on and latches a
/// change against: `debugger_mode=` alone in this build (`debugger_pending=`
/// joins when aterm watches for a waiting password request, design §4.4). A
/// reading that observed no mode — a probe still out, a closed gate, a refused
/// or failed run, an inert instance — is not an observation: it reads as the
/// internal pending marker, so it can neither arm the baseline nor latch.
fn debugger_tuple_line(probe: DebuggerProbe) -> String {
    if probe.mode == DebuggerMode::Unknown {
        PENDING_CONSENT_TUPLE.to_string()
    } else {
        format!("debugger_mode={}", probe.mode.as_str())
    }
}

/// Every name on the `folder` row: the consent module's own roster (the three
/// `$HOME` folders it resolves, then `app-data` — every other app's own data,
/// promptable and resettable exactly as a folder is), then the two volume
/// classes that have no path.
fn folder_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Folder::ALL.iter().map(|f| f.as_str()).collect();
    names.extend_from_slice(VOLUME_ROWS);
    names
}

/// The per-service folder state, and the provenance token that qualifies it.
///
/// Pure; the only place the join exists. Two sources, in strict precedence:
///
/// 1. **What aterm observed.** A warm-up row the owner asked for outranks
///    everything, including a held grant: an `EPERM` on Documents reads
///    `denied` whatever the coverage rule claims.
/// 2. **Measured coverage.** A service §7 S4 proved a held grant suppresses
///    reads `covered-by-fda`, only while it is in `covers` for the current
///    posture.
///
/// Everything else is `unknown`: reading a folder to learn whether it is
/// readable is what raises the prompt (§3.4). The volume classes have no path
/// or warm-up row, so they are always `unknown`. Unmeasured folders
/// (Documents, Desktop, Downloads) stay `unknown` under a held grant.
fn folder_state(
    name: &str,
    warmup: &[(Folder, crate::consent_warmup::WarmupRow)],
    covers: &[&'static str],
) -> &'static str {
    if let Some((_, row)) = warmup
        .iter()
        .find(|(folder, _)| folder.as_str() == name)
        .filter(|(_, row)| *row != crate::consent_warmup::WarmupRow::Unknown)
    {
        return row.as_str();
    }
    if covers.contains(&name) {
        return "covered-by-fda";
    }
    "unknown"
}

/// Which sources contributed a non-`unknown` folder state, in a fixed order;
/// `none` when nothing did.
fn folder_source(states: &[(&'static str, &'static str)]) -> String {
    let mut sources: Vec<&'static str> = Vec::new();
    if states
        .iter()
        .any(|(_, state)| matches!(*state, "allowed" | "denied" | "asking" | "error"))
    {
        sources.push("warmup");
    }
    if states.iter().any(|(_, state)| *state == "covered-by-fda") {
        sources.push("fda");
    }
    if sources.is_empty() {
        "none".to_string()
    } else {
        sources.join(",")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn list_or_dash(items: &[&str]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(",")
    }
}

/// The `--json` body: the same facts as one object, with `folders`,
/// `sessions`, `observers` and `remediate` as sub-objects.
///
/// The `privacy` row of `JSON_CAPABLE_VERBS` is this handler.
fn cmd_privacy_json(snapshot: &PrivacySnapshot) -> String {
    use std::fmt::Write as _;

    let split = covers_split(snapshot.fda, snapshot.evidence);
    let mut body = String::from("{\"schema\":1,");
    let _ = write!(body, "{},", json_str_field("platform", snapshot.platform));
    let _ = write!(body, "\"os\":{},", json_opt(snapshot.os.as_deref()));
    let _ = write!(
        body,
        "\"identity\":{{\"bundle_id\":{},\"display_name\":{},{},\"team\":{},{},\
         \"grant_stable\":{},\"dev_build\":{}}},",
        json_opt(snapshot.identity.bundle_id.as_deref()),
        json_opt(snapshot.identity.display_name.as_deref()),
        json_str_field("signing", snapshot.identity.signing),
        json_opt(snapshot.identity.team.as_deref()),
        json_str_field("dr", snapshot.identity.dr.as_str()),
        snapshot.identity.dr.grant_stable(),
        snapshot
            .identity
            .dev_build
            .map_or_else(|| "null".to_string(), |d| d.to_string()),
    );
    let _ = write!(
        body,
        "{},\"running\":{},{},\"grant_unverifiable\":{},",
        json_str_field("install", snapshot.install),
        json_opt(snapshot.running.as_deref()),
        json_str_field("anchor", snapshot.anchor.as_str()),
        snapshot.anchor.grant_unverifiable(),
    );
    let _ = write!(
        body,
        "{},{},\"probe_age_ms\":{},{},",
        json_str_field("full_disk_access", snapshot.fda.as_str()),
        json_str_field("probe", snapshot.probe.as_str()),
        snapshot
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field(
            "fda_scope",
            observed_fda_scope(snapshot.fda, snapshot.evidence.fda_scope).as_str()
        ),
    );
    let _ = write!(
        body,
        "\"covers\":{},\"uncovered\":{},\"unmeasured\":{},",
        json_str_array(&split.covers),
        json_str_array(&split.uncovered),
        json_str_array(&split.unmeasured),
    );
    let _ = write!(
        body,
        "\"folders\":{{{}",
        json_str_field("source", &snapshot.folder_source)
    );
    for (name, state) in &snapshot.folders {
        let _ = write!(body, ",{}", json_str_field(name, state));
    }
    let _ = write!(body, "}},");
    // THE CENSUS, with the same keys in every state: counts are null until it
    // answers, as the text form prints `-`.
    let _ = match &snapshot.claimants {
        None => write!(
            body,
            "\"census\":{{{},\"claimants\":null,\"conflicting\":null,\"sole\":null,\
             \"age_ms\":null,\"conflicts\":null,\"truncated\":0}},",
            json_str_field(
                "state",
                if snapshot.census_pending {
                    "pending"
                } else {
                    "off"
                }
            )
        ),
        Some((census, age)) => {
            let conflicting = census.conflicting();
            let conflicts: Vec<String> = census
                .listed(snapshot.trash_tool)
                .into_iter()
                .map(|(claimant, offer)| {
                    format!(
                        "{{{},{},{},\"team\":{},{}}}",
                        json_str_field("path", &claimant.path.to_string_lossy()),
                        json_str_field("signing", claimant.signing),
                        json_str_field("dr", claimant.dr.as_str()),
                        json_opt(claimant.team.as_deref()),
                        json_str_field("trash", offer.as_str()),
                    )
                })
                .collect();
            write!(
                body,
                "\"census\":{{{},\"claimants\":{},\"conflicting\":{},\"sole\":{},\
                 \"age_ms\":{},\"conflicts\":[{}],\"truncated\":{}}},",
                json_str_field("state", census.enumeration.as_str()),
                census.found.len(),
                conflicting.len(),
                census.sole_claimant(),
                age.as_millis(),
                conflicts.join(","),
                conflicting.len().saturating_sub(consent::MAX_CLAIMANT_ROWS),
            )
        }
    };
    let _ = write!(body, "\"prompt_possible\":{},", snapshot.prompt_possible());
    let _ = write!(
        body,
        "{},\"sessions_total\":{},\"sessions_adopted\":{},",
        json_str_field("attribution_root", snapshot.attribution_root.as_str()),
        snapshot.sessions.len(),
        snapshot.sessions_adopted(),
    );
    body.push_str("\"sessions\":[");
    for (i, s) in snapshot.sessions.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        let _ = write!(
            body,
            "{{{},{},\"responsible_pid\":{},{},{},\"cwd\":{}}}",
            json_str_field("sid", &s.sid),
            json_str_field("attribution", s.attribution.as_str()),
            s.responsible_pid
                .map_or_else(|| "null".to_string(), |pid| pid.to_string()),
            json_str_field("responsible", &responsible_token(s.responsible)),
            json_str_field("fs_consent", s.fs_consent.as_str()),
            json_opt(s.cwd.as_deref()),
        );
    }
    body.push_str("],");
    let _ = write!(
        body,
        "\"containment\":{{{},{},\"protected\":{}}},",
        json_str_field("mode", &snapshot.containment_mode),
        json_str_field("seatbelt", snapshot.seatbelt),
        json_str_array(
            &snapshot
                .protected()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        ),
    );
    let _ = write!(
        body,
        "\"warmup\":{{{},\"last_ms\":{}}},",
        json_str_field("mode", snapshot.warmup),
        snapshot
            .warmup_last_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
    );
    // The debugger rule, with the same keys in every state: what was not
    // observed is null, as the text form prints `-`.
    let _ = write!(
        body,
        "\"debugger\":{{{},{},{},{},{},\"probe_age_ms\":{},{},\"pending_right\":null,\
         \"pending_age_ms\":null,\"last\":null,\"watch_ms\":null}},",
        json_str_field("mode", snapshot.debugger.probe.mode.as_str()),
        json_str_field("member", snapshot.debugger.probe.member.as_str()),
        json_str_field("self_hardened", snapshot.debugger.self_hardened.as_str()),
        json_str_field("right", consent::TASKPORT_DEBUG_RIGHT),
        json_str_field("probe", snapshot.debugger.probe.label.as_str()),
        snapshot
            .debugger
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field("pending", TASKPORT_WATCH),
    );
    // The Developer Tools list, with the same keys in every state.
    let num = |value: Option<u64>| value.map_or_else(|| "null".to_string(), |n| n.to_string());
    let devtools = snapshot.devtools.probe;
    let _ = write!(
        body,
        "\"devtools\":{{{},\"subject\":{},\"requests\":{},\"not_allowed\":{},\
         \"answer_age_ms\":{},\"window_s\":{},{},\"probe_age_ms\":{},{},{},{}}},",
        json_str_field("state", devtools.state.as_str()),
        json_opt(snapshot.devtools.subject),
        num(devtools.requests.map(u64::from)),
        num(devtools.not_allowed.map(u64::from)),
        num(snapshot.devtools.answer_age_ms),
        consent::DEVTOOL_WINDOW_S,
        json_str_field("probe", devtools.label.as_str()),
        snapshot
            .devtools
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field("grant", snapshot.devtools.grant().as_str()),
        json_str_field("list", snapshot.devtools.list.listing.as_str()),
        json_str_field("list_probe", snapshot.devtools.list.label.as_str()),
    );
    let _ = write!(body, "\"access_card\":{},", snapshot.access_card.json());
    let _ = write!(
        body,
        "\"observers\":{{{},{},{},{}}},",
        json_str_field("fda", snapshot.observer_fda),
        json_str_field("responsible", snapshot.observer_responsible),
        json_str_field("log", snapshot.observer_log),
        json_str_field("taskport", TASKPORT_WATCH),
    );
    let [fda, files, debugger, devtools] = snapshot.remediate().map(json_opt);
    let _ = write!(
        body,
        "\"remediate\":{{\"fda\":{fda},\"files\":{files},\"debugger\":{debugger},\
         \"devtools\":{devtools}}},",
    );
    let _ = write!(body, "{}}}", json_str_field("note", snapshot.note()));
    json_ok(&body)
}

fn json_opt(value: Option<&str>) -> String {
    value.map_or_else(
        || "null".to_string(),
        |v| format!("\"{}\"", aterm_control::wire::json_escape(v)),
    )
}

fn json_str_array(items: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&aterm_control::wire::json_escape(item));
        out.push('"');
    }
    out.push(']');
    out
}

// The main-thread read

/// Which shape of the posture the main thread should assemble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivacyForm {
    /// The full posture, as lines.
    Lines,
    /// The full posture, as one JSON object.
    Json,
    /// One session's consent tuple, for `await consent`.
    Tuple(u64),
    /// The debugger rule's tuple, for `await consent kind=debugger`, read
    /// through one session so a wait on an exited session ends `ERR exited`.
    /// The fact is instance-level; the session only addresses the waiter.
    DebuggerTuple(u64),
}

/// Internal reply metadata: the worker must not infer readiness by parsing
/// human text or JSON. Nothing here changes the public report's schema.
pub(crate) struct PrivacyRead {
    lines: Vec<String>,
    pending: bool,
}

impl App {
    /// Assemble the consent posture on the main thread (`Wake::ReadPrivacy`).
    ///
    /// A pure read of `App` state plus the injected probes. Reading it raises
    /// NO dialog: the Full Disk Access probe reads state that already exists,
    /// and nothing here is inferred from anything else — in particular no
    /// folder's state is ever derived from the grant.
    ///
    /// `scope` decides WHICH sessions it reports: those `scope` may read with
    /// the verb's own op. Owner-class scopes see every session; an edge sees
    /// only the sessions it holds a read edge into, because each row (sid,
    /// cwd, responsible pid) is a fact `status` shows one session at a time to
    /// whoever may read it — and the whole set is the roster `sessions`,
    /// `flows` and `subscribe sessions` withhold from an edge. The
    /// instance-wide posture is the same for every scope.
    pub(crate) fn read_privacy(&self, form: PrivacyForm, scope: Scope) -> PrivacyRead {
        let policy = self.consent_policy();
        if let PrivacyForm::DebuggerTuple(target) = form {
            // Nothing of the Full Disk Access posture is read: this waiter's
            // evidence is the debugger slot alone, and an exited session ends
            // the wait whatever the slot holds.
            if self.pool.get(target).is_none() {
                return PrivacyRead {
                    lines: Vec::new(),
                    pending: false,
                };
            }
            // The slot's own discipline, never a probe per poll: the cached
            // answer is served, and the ONE bounded worker is dispatched only
            // when that answer is older than `DEBUGGER_TTL` or something
            // retired it (focus gain, the owner's change re-probing), under
            // the retry floor. A 500 ms poll therefore spawns at most one
            // `-status` run per TTL of its own accord.
            let row = self.consent.debugger(policy.gate);
            return PrivacyRead {
                lines: vec![debugger_tuple_line(row.probe())],
                pending: row.pending(),
            };
        }
        let identity = signing_identity_if_warm();
        let (probe, age) = self
            .consent
            .fda(policy.gate, policy.interval, &identity.dr_text);
        let self_pid = i32::try_from(std::process::id()).unwrap_or(-1);

        let mut ids: Vec<u64> = self.pool.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        let mut answers = Vec::with_capacity(ids.len());
        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(session) = self.pool.get(id) else {
                continue;
            };
            if !cross_session_authorized(scope, "privacy", &session.ctx) {
                continue;
            }
            // `[privacy] report_attribution = false` removes a COLUMN, not a
            // verdict: the SPI is simply not consulted, and the observer row
            // says `off` rather than pretending it answered.
            let answer = if policy.report_attribution {
                let answer = self.consent.responsible_answer(session.pid);
                answers.push(answer);
                answer
            } else {
                Err(ResponsibleError::Unsupported)
            };
            let responsible = consent::classify_responsible(self_pid, answer);
            let posture = ConsentPosture::join(PostureInputs {
                adoption: policy.adoption(session_attribution(session)),
                fda: probe.state,
                responsible,
                // No production site observes a per-session EPERM yet (the
                // warm-up §3.5 and aterm's own file work §3.6 will).
                observed_eperm: false,
                evidence: SpikeEvidence::UNMEASURED,
            });
            sessions.push(SessionRow {
                sid: session.ctx.self_id.as_str().to_string(),
                attribution: posture.attribution,
                responsible_pid: answer.ok(),
                responsible,
                fs_consent: posture.fs_consent,
                cwd: self.session_reported_cwd(id),
            });
        }

        if let PrivacyForm::Tuple(target) = form {
            let lines = sessions
                .iter()
                .find(|row| self.session_sid_matches(target, &row.sid))
                .map(|row| vec![PrivacySnapshot::observed_tuple_line(row, probe)])
                .unwrap_or_default();
            return PrivacyRead {
                lines,
                pending: probe.label == ProbeLabel::Pending,
            };
        }

        let mode = aterm_containment::mode_or_containment();
        let (install, running) = install_posture_once().clone();
        // The census asks about the RUNNING bundle's identifier — never a
        // literal; a build with no readable one gets no census.
        let (claimants, census_pending) = self
            .consent
            .claimants(identity.bundle_id.as_deref(), policy.enabled);
        // The debugger rule, from its own slot: a full report may dispatch its
        // bounded worker, and nothing on the `status`/tuple path above does.
        let debugger = self.consent.debugger(policy.gate);
        // The Developer Tools list, from its own slot, about the RUNNING
        // bundle's identifier: only this full report (and the visible Security
        // page) may dispatch its `log show`, never `status`, a tuple or a poll.
        let devtools = self
            .consent
            .devtools(policy.gate, self.config.privacy_observer_off());
        // The folder row's state. `rows()` does not drain (a pass the Security
        // panel has not folded yet must not be consumed). `covers` comes from
        // the same split `lines()` renders.
        let evidence = SpikeEvidence::UNMEASURED;
        let split = covers_split(probe.state, evidence);
        let warmup_rows = self.consent_warmup.rows();
        let folders: Vec<(&'static str, &'static str)> = folder_names()
            .into_iter()
            .map(|name| (name, folder_state(name, warmup_rows, &split.covers)))
            .collect();
        let folder_source_token = folder_source(&folders);
        let snapshot = PrivacySnapshot {
            platform: std::env::consts::OS,
            os: os_version().map(str::to_owned),
            identity: identity.clone(),
            install,
            running,
            // Read fresh on every report: it changes under a running process.
            // One `proc_pidpath`; no dialog.
            anchor: aterm_containment::image_anchor(),
            fda: probe.state,
            probe: probe.label,
            probe_age_ms: completed_probe_age_ms(probe.label, age),
            evidence,
            attribution_root: policy.adoption(self.instance_attribution()),
            sessions,
            containment_mode: mode.to_string().to_ascii_lowercase(),
            seatbelt: if aterm_containment::actuator::network_sandbox_actuated(mode) {
                "applied"
            } else {
                "none"
            },
            protected: self
                .config
                .privacy_protected_roots()
                .iter()
                .map(|p| crate::app_tabs::home_abbreviated(&p.to_string_lossy()))
                .collect(),
            warmup: self.config.privacy_warmup().as_str(),
            // The warm-up's completion stamp (design §3.5), read without
            // draining: an unfolded pass is reported on the next read.
            warmup_last_ms: self.consent_warmup.last_pass_ms(),
            debugger,
            devtools,
            folders,
            folder_source: folder_source_token,
            claimants,
            census_pending,
            trash_tool: crate::consent_retire::trash_tool_present(),
            observer_fda: observer_fda_value(probe.label),
            observer_responsible: observer_responsible_value(&answers),
            observer_log: self.privacy_observer_log(),
            access_card: self.access_card_report(probe.state),
        };
        let lines = match form {
            PrivacyForm::Json => vec![cmd_privacy_json(&snapshot)],
            PrivacyForm::Lines | PrivacyForm::Tuple(_) | PrivacyForm::DebuggerTuple(_) => {
                snapshot.lines()
            }
        };
        PrivacyRead {
            lines,
            // A first census (no answer yet) takes the same bounded wait as a
            // cold probe, so a one-shot read gets the row rather than `pending`;
            // so do a first debugger read and a first Developer Tools read.
            pending: probe.label == ProbeLabel::Pending
                || census_pending
                || debugger.pending()
                || devtools.pending(),
        }
    }

    /// The Security panel's slice of the posture (design §3.4): the same
    /// state `privacy` reports, assembled for a renderer. Never spawns
    /// `codesign`; a cold identity degrades to `DrClass::Unknown`, which
    /// suppresses no repair and promises no durability.
    pub(crate) fn consent_panel_facts(&self) -> ConsentPanelFacts {
        let identity = signing_identity_if_warm();
        let enabled = self.config.privacy_enabled();
        let (probe, _age) = self.consent.fda(
            self.config.privacy_probe_gate(),
            Duration::from_millis(self.config.privacy_probe_interval_ms()),
            &identity.dr_text,
        );
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
            install,
            running: running.clone(),
            sessions_total,
            sessions_adopted,
        }
    }

    /// This instance's adoption record: `adopted` when it took over its
    /// predecessor's shells across an in-place apply (even when its session 0
    /// is a fresh bootstrap), `live` otherwise.
    fn instance_attribution(&self) -> Attribution {
        if self.handoff_successor {
            Attribution::Adopted
        } else {
            Attribution::Live
        }
    }

    /// The shell-integration cwd for one session, without ever waiting on the
    /// terminal lock: a contended terminal reports nothing rather than
    /// blocking the event loop under a poll.
    pub(crate) fn session_reported_cwd(&self, session: u64) -> Option<String> {
        use crate::cwd_native::ReportedCwd as _;
        let pooled = self.pool.get(session)?;
        let term = pooled.term.try_lock().ok()?;
        term.native_working_directory()
            .map(std::borrow::Cow::into_owned)
    }

    /// Whether the local id `target` is the session whose stable sid is `sid`.
    fn session_sid_matches(&self, target: u64, sid: &str) -> bool {
        self.pool
            .get(target)
            .is_some_and(|s| s.ctx.self_id.as_str() == sid)
    }
}

/// What the Security panel needs from the consent model, as pure data: a
/// compact projection of [`PrivacySnapshot`] (posture and session counts), with
/// service coverage from [`covers_split`].
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

/// The `[privacy]` section, resolved once per report.
struct ConsentPolicy {
    /// `enabled` + `check`, as the consent module's own gate.
    gate: ProbeGate,
    /// `probe_interval_ms`.
    interval: Duration,
    /// `report_attribution`.
    report_attribution: bool,
    /// The master switch.
    enabled: bool,
}

impl ConsentPolicy {
    /// With the lane off every consent field reads `unknown` ("aterm stopped
    /// looking", not `denied`), including the adoption record.
    const fn adoption(&self, observed: Attribution) -> Attribution {
        if self.enabled {
            observed
        } else {
            Attribution::Unknown
        }
    }
}

impl App {
    /// Resolve the `[privacy]` section for one report.
    fn consent_policy(&self) -> ConsentPolicy {
        ConsentPolicy {
            gate: self.config.privacy_probe_gate(),
            interval: Duration::from_millis(self.config.privacy_probe_interval_ms()),
            report_attribution: self.config.privacy_enabled()
                && self.config.privacy_report_attribution(),
            enabled: self.config.privacy_enabled(),
        }
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
    /// loop under a poll, so it never blocks: the cached probe, the identity
    /// only if warm, and a `cwd` from the caller, which already holds (or
    /// failed to take) the session's terminal lock.
    pub(crate) fn session_consent(&self, session: u64, cwd: Option<&str>) -> SessionConsent {
        let Some(pooled) = self.pool.get(session) else {
            return SessionConsent::unknown();
        };
        let policy = self.consent_policy();
        let (probe, _age) = self.consent.fda(
            policy.gate,
            policy.interval,
            &signing_identity_if_warm().dr_text,
        );
        let posture = ConsentPosture::join(PostureInputs {
            adoption: policy.adoption(session_attribution(pooled)),
            fda: probe.state,
            // The responsibility SPI is not consulted on this poll path: it
            // changes neither field. The `privacy` verb reports it.
            responsible: Responsible::Unknown,
            // See `read_privacy`: no production site observes a per-session
            // EPERM yet, and the join is table-tested on both values.
            observed_eperm: false,
            evidence: SpikeEvidence::UNMEASURED,
        });
        SessionConsent {
            attribution: posture.attribution,
            fs_consent: posture.fs_consent,
            // Resolve the protected roots only when they can decide the
            // answer (a cwd is known and not covered).
            at_risk: cwd.is_some()
                && !matches!(posture.fs_consent, FsConsent::Covered)
                && consent_at_risk(
                    posture.fs_consent,
                    cwd,
                    &self.config.privacy_protected_roots(),
                ),
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
fn session_attribution(session: &crate::Session) -> Attribution {
    if session.handoff_local_id.is_some() {
        Attribution::Adopted
    } else {
        Attribution::Live
    }
}

// The verb

/// `privacy [--json]`: the macOS consent posture, instance-wide.
///
/// Runs on a control worker: it warms the signing identity HERE (it may spawn
/// `codesign`, which must never land on the event loop) and then takes the one
/// main-thread hop that reads `App` state and the injected probes. An unfinished
/// observation gets a short, bounded wait HERE, never on the GUI thread. Thus
/// occasional calls do not report only Pending by always arriving after the
/// cache's freshness interval.
pub(crate) fn cmd_privacy(rest: &str, scope: Scope, proxy: &EventLoopProxy<Wake>) -> String {
    let form = match parse_privacy_form(rest) {
        Ok(form) => form,
        Err(usage) => return usage,
    };
    let json = form == PrivacyForm::Json;
    // Warm the process-wide identity off the event loop.
    let _ = signing_identity();
    let first = match crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
        form,
        scope,
        reply: tx,
    }) {
        Ok(read) => read,
        Err(e) => return format!("ERR {e}\n"),
    };
    let lines = finish_privacy_read(
        first,
        |remaining| {
            crate::control::control_media::call_main_within(proxy, remaining, |tx| {
                Wake::ReadPrivacy {
                    form,
                    scope,
                    reply: tx,
                }
            })
        },
        Instant::now,
        std::thread::sleep,
    )
    .lines;
    if json {
        // The JSON body already carries its own `OK 1` framing.
        return lines.into_iter().next().unwrap_or_else(|| json_ok("{}"));
    }
    let mut out = format!("OK {}\n", lines.len());
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The deadline is fixed before the first retry. Pending refreshes cannot
/// extend it, and failures preserve the latest honest Pending snapshot.
fn finish_privacy_read(
    mut latest: PrivacyRead,
    mut read: impl FnMut(Duration) -> Result<PrivacyRead, MainHopError>,
    now: impl Fn() -> Instant,
    mut pause: impl FnMut(Duration),
) -> PrivacyRead {
    let deadline = now() + PRIVACY_READ_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if !privacy_read_should_retry(latest.pending, remaining) {
            return latest;
        }
        pause(PRIVACY_READ_TICK.min(remaining));
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return latest;
        }
        match read(remaining) {
            Ok(next) => latest = next,
            Err(_) => return latest,
        }
    }
}

const fn privacy_read_should_retry(pending: bool, remaining: Duration) -> bool {
    pending && !remaining.is_zero()
}

#[cfg(test)]
mod privacy_read_tests {
    use super::*;
    use std::cell::Cell;

    fn reply(probe: FdaProbe) -> PrivacyRead {
        PrivacyRead {
            lines: vec![probe.state.as_str().to_string()],
            pending: probe.label == ProbeLabel::Pending,
        }
    }

    #[test]
    fn privacy_read_observes_the_real_async_cache_completion() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let mut state = ConsentState::inert();
        state.probes.live = true;
        let key = ConsentKey::new("fixture.app", "fixture identity");
        let interval = Duration::from_secs(5);
        let ((first, _), request) = state.read_at(ProbeGate::on(), interval, key.clone(), start);
        assert_eq!(first.label, ProbeLabel::Pending);
        let request = request.unwrap();
        let denied = consent::classify_probe(consent::ProbeOutcome::Errno(consent::ERRNO_EPERM));
        let result = finish_privacy_read(
            reply(first),
            |_| {
                state.shared.complete(&request, denied, clock.get());
                let ((current, _), next) =
                    state.read_at(ProbeGate::on(), interval, key.clone(), clock.get());
                assert!(
                    next.is_none(),
                    "reading a fresh completion does not launch a probe"
                );
                Ok(reply(current))
            },
            || clock.get(),
            |delay| clock.set(clock.get() + delay),
        );
        assert!(!result.pending);
        assert_eq!(result.lines, ["denied"]);
        assert_eq!(clock.get() - start, PRIVACY_READ_TICK);
    }

    #[test]
    fn privacy_read_has_one_deadline_and_keeps_stuck_work_pending() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let result = finish_privacy_read(
            reply(FdaProbe::pending()),
            |remaining| {
                assert!(remaining < PRIVACY_READ_WAIT);
                Ok(reply(FdaProbe::pending()))
            },
            || clock.get(),
            |delay| clock.set(clock.get() + delay),
        );
        assert!(result.pending);
        assert_eq!(result.lines, ["unknown"]);
        assert_eq!(
            clock.get() - start,
            PRIVACY_READ_WAIT,
            "the deadline is never extended by Pending"
        );
    }

    #[test]
    fn privacy_read_completed_and_failed_reads_do_not_retry_in_a_loop() {
        let now = Instant::now();
        let completed = finish_privacy_read(
            reply(consent::classify_probe(consent::ProbeOutcome::Ok)),
            |_| panic!("a completed observation needs no retry"),
            || now,
            |_| panic!("a completed observation needs no wait"),
        );
        assert_eq!(completed.lines, ["granted"]);
        let clock = Cell::new(now);
        let reads = Cell::new(0);
        let failed = finish_privacy_read(
            reply(FdaProbe::pending()),
            |_| {
                reads.set(reads.get() + 1);
                Err(MainHopError::Reason("event loop gone"))
            },
            || clock.get(),
            |delay| clock.set(clock.get() + delay),
        );
        assert!(failed.pending);
        assert_eq!(reads.get(), 1);
    }

    fn app_data_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AppDataFdaAuthority {
                const Buggy = 0;
                var grant = 0;
                var scope = 0;
                var adopted = 0;
                var decided = 0;
                var host_covered = 0;
                var session_covered = 0;
                action Grant when (decided == 0) { grant = 1; }
                action CurrentScope when (decided == 0) { scope = 1; }
                action FutureScope when (decided == 0) { scope = 2; }
                action Adopt when (decided == 0) { adopted = 1; }
                action Classify when (decided == 0) {
                    host_covered = if (grant == 1 && scope > 0) || Buggy == 1 { 1 } else { 0 };
                    session_covered = if Buggy == 1 && grant == 1 { 1 } else { 0 };
                    decided = 1;
                }
                invariant HostNeedsAuthority: host_covered == 0 || (grant == 1 && scope > 0);
                invariant NoInheritedGrant: adopted == 0 || session_covered == 0;
                invariant Bounds: grant <= 1 && scope <= 2 && adopted <= 1 && decided <= 1 && host_covered <= 1 && session_covered <= 1;
            }
        }
    }

    #[test]
    fn app_data_model_and_real_classifier_require_current_authority() {
        let model = app_data_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            for scope in [
                consent::FdaScope::Unknown,
                consent::FdaScope::ThisProcess,
                consent::FdaScope::NewProcesses,
            ] {
                for adoption in [Attribution::Live, Attribution::Adopted] {
                    let mut state = model.init_state();
                    if fda == FdaState::Granted {
                        assert!(model.fire("Grant", &mut state));
                    }
                    match scope {
                        consent::FdaScope::Unknown => {}
                        consent::FdaScope::ThisProcess => {
                            assert!(model.fire("CurrentScope", &mut state))
                        }
                        consent::FdaScope::NewProcesses => {
                            assert!(model.fire("FutureScope", &mut state))
                        }
                    }
                    if adoption == Attribution::Adopted {
                        assert!(model.fire("Adopt", &mut state));
                    }
                    let expected = model.successors("Classify", &state);
                    let mut projected = expected[0].clone();
                    projected.insert("host_covered", i64::from(app_data_covered(fda, scope)));
                    let posture = ConsentPosture::join(PostureInputs {
                        adoption,
                        fda,
                        responsible: Responsible::Unknown,
                        observed_eperm: false,
                        evidence: SpikeEvidence::UNMEASURED,
                    });
                    projected.insert(
                        "session_covered",
                        i64::from(posture.fs_consent == FsConsent::Covered),
                    );
                    assert!(expected.contains(&projected));
                    if adoption == Attribution::Adopted {
                        projected.insert("session_covered", 1);
                        assert!(!model.check_invariant("NoInheritedGrant", &projected));
                    }
                }
            }
            let split = covers_split(fda, SpikeEvidence::UNMEASURED);
            assert_eq!(split.covers.contains(&"app-data"), fda == FdaState::Granted);
            assert_eq!(
                split.unmeasured.contains(&"app-data"),
                fda != FdaState::Granted
            );
            assert_eq!(
                observed_fda_scope(fda, consent::FdaScope::Unknown),
                if fda == FdaState::Granted {
                    consent::FdaScope::ThisProcess
                } else {
                    consent::FdaScope::Unknown
                }
            );
        }
    }
}

/// The verb's grammar: an optional `--json` (or bare `json`) flag and nothing
/// else; anything else is `ERR usage`.
fn parse_privacy_form(rest: &str) -> Result<PrivacyForm, String> {
    let mut json = false;
    let mut unknown: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if tok == "--json" || tok == "json" {
            json = true;
        } else {
            unknown.push(tok);
        }
    }
    if !unknown.is_empty() {
        return Err(format!(
            "ERR usage: privacy [--json] (got {:?})\n",
            unknown.join(" ")
        ));
    }
    Ok(if json {
        PrivacyForm::Json
    } else {
        PrivacyForm::Lines
    })
}

/// The refusal a selector earns on this verb.
pub(crate) const fn no_selector_error() -> &'static str {
    NO_SELECTOR
}

#[derive(Debug, PartialEq, Eq)]
enum ConsentObservation {
    Pending,
    Complete(String),
    Exited,
}

fn consent_observation(lines: Vec<String>) -> ConsentObservation {
    match lines.into_iter().next() {
        Some(line) if line == PENDING_CONSENT_TUPLE => ConsentObservation::Pending,
        Some(line) => ConsentObservation::Complete(line),
        None => ConsentObservation::Exited,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ConsentWaitDecision {
    Wait,
    Changed(String),
    Exited,
    TimedOut,
}

struct ConsentWait {
    baseline: Option<String>,
    deadline: Instant,
}

impl ConsentWait {
    fn new(armed: Instant, timeout: Duration) -> Self {
        Self {
            baseline: None,
            deadline: armed + timeout,
        }
    }

    fn expired_at(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Pending is not an observed change or an exit. A cold wait arms at its
    /// first completed observation; a refresh retains that same baseline.
    /// Neither operation extends the original request's deadline.
    fn observe_at(&mut self, observation: ConsentObservation, now: Instant) -> ConsentWaitDecision {
        if self.expired_at(now) {
            return ConsentWaitDecision::TimedOut;
        }
        match observation {
            ConsentObservation::Pending => ConsentWaitDecision::Wait,
            ConsentObservation::Exited => ConsentWaitDecision::Exited,
            ConsentObservation::Complete(line) => match &self.baseline {
                Some(baseline) if baseline != &line => ConsentWaitDecision::Changed(line),
                Some(_) => ConsentWaitDecision::Wait,
                None => {
                    self.baseline = Some(line);
                    ConsentWaitDecision::Wait
                }
            },
        }
    }
}

/// `await consent [kind=debugger] [timeout=<ms>]` — wait for an observed
/// tuple to change. Bare, it is THIS session's Full Disk Access tuple
/// (`fs_consent= fda= attribution=`), untouched by the debugger form so its
/// waiters are never woken by debugger churn. `kind=debugger` is the debugger
/// rule's tuple (`debugger_mode=`, design §4.4), addressed through the session
/// only so the wait ends `ERR exited` with it.
///
/// A pending refresh is not a change. On a cold cache, the first completed
/// observation establishes the baseline; it cannot itself satisfy the wait.
/// The timeout remains anchored to the original request.
///
/// A latch says aterm's own posture CHANGED. It does NOT say a human answered
/// a dialog: aterm cannot observe the answer.
pub(crate) fn cmd_await_consent(
    proxy: &EventLoopProxy<Wake>,
    scope: Scope,
    session: u64,
    rest: &str,
) -> String {
    let args = match parse_consent_args(rest) {
        Ok(args) => args,
        Err(usage) => return usage,
    };
    let armed = Instant::now();
    let mut wait = ConsentWait::new(armed, Duration::from_millis(args.timeout_ms));
    let form = match args.kind {
        ConsentKind::Fda => {
            // The FDA tuple's probe keys on the signing identity; warm it off
            // the event loop. The debugger form reads no identity.
            let _ = signing_identity();
            PrivacyForm::Tuple(session)
        }
        ConsentKind::Debugger => PrivacyForm::DebuggerTuple(session),
    };
    let sample = |proxy: &EventLoopProxy<Wake>| -> Result<ConsentObservation, String> {
        match crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
            form,
            scope,
            reply: tx,
        }) {
            Ok(read) => Ok(consent_observation(read.lines)),
            Err(e) => Err(format!("ERR {e}\n")),
        }
    };
    loop {
        if wait.expired_at(Instant::now()) {
            return "OK timeout\n".to_string();
        }
        let observation = match sample(proxy) {
            Ok(observation) => observation,
            Err(error) => return error,
        };
        match wait.observe_at(observation, Instant::now()) {
            ConsentWaitDecision::Changed(line) => {
                return format!(
                    "OK consent {line} elapsed_ms={}\n",
                    armed.elapsed().as_millis()
                );
            }
            ConsentWaitDecision::Exited => return "ERR exited\n".to_string(),
            ConsentWaitDecision::TimedOut => return "OK timeout\n".to_string(),
            ConsentWaitDecision::Wait => {}
        }
        if crate::control::caller_hung_up() {
            return crate::control::HUNG_UP_REPLY.to_string();
        }
        std::thread::sleep(crate::control::hangup_park(
            AWAIT_CONSENT_TICK.min(wait.deadline.saturating_duration_since(Instant::now())),
        ));
    }
}

/// Which tuple `await consent` waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsentKind {
    /// This session's Full Disk Access tuple — the bare form.
    Fda,
    /// The debugger rule's tuple — `kind=debugger`.
    Debugger,
}

/// `await consent`'s parsed arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsentArgs {
    kind: ConsentKind,
    timeout_ms: u64,
}

/// `[kind=debugger] [timeout=<ms> | timeout <ms>]` in any order. The timeout
/// defaults to [`AWAIT_CONSENT_DEFAULT_MS`] for both kinds and is capped at
/// [`AWAIT_MAX_MS`], the ceiling the rest of the wait family shares. Any other
/// token — including a `kind=` this build does not know — is the usage line,
/// so an agent learns the form is unsupported instead of waiting on the wrong
/// tuple.
fn parse_consent_args(rest: &str) -> Result<ConsentArgs, String> {
    const USAGE: &str = "ERR usage: await consent [kind=debugger] [timeout=<ms>]\n";
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut args = ConsentArgs {
        kind: ConsentKind::Fda,
        timeout_ms: AWAIT_CONSENT_DEFAULT_MS,
    };
    let mut i = 0;
    // `consent` itself is the leading token the caller already matched on.
    if toks.first() == Some(&"consent") {
        i = 1;
    }
    while i < toks.len() {
        if toks[i] == "kind=debugger" {
            args.kind = ConsentKind::Debugger;
            i += 1;
        } else if let Some(v) = toks[i].strip_prefix("timeout=") {
            args.timeout_ms = v.parse().map_err(|_| USAGE.to_string())?;
            i += 1;
        } else if toks[i] == "timeout" && i + 1 < toks.len() {
            args.timeout_ms = toks[i + 1].parse().map_err(|_| USAGE.to_string())?;
            i += 2;
        } else {
            return Err(USAGE.to_string());
        }
    }
    args.timeout_ms = args.timeout_ms.min(AWAIT_MAX_MS);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use aterm_containment::consent::DebuggerMember;

    use super::*;

    /// The debugger row the fixture carries: design §4.1's example, the
    /// installed release build on a Mac with developer mode off.
    const FIXTURE_DEBUGGER: DebuggerRow = DebuggerRow {
        probe: DebuggerProbe {
            mode: DebuggerMode::Disabled,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        },
        self_hardened: SelfHardening::Hardened,
        probe_age_ms: Some(1204),
    };

    /// The Developer Tools row the fixture carries: the design's example, the
    /// installed release build, not on the list, during a build that started
    /// 233 new programs in the window.
    const FIXTURE_DEVTOOLS: DevToolsRow = DevToolsRow {
        probe: DevToolProbe {
            state: aterm_containment::DevToolState::Unlisted,
            label: DevToolProbeLabel::Observed,
            requests: Some(233),
            not_allowed: Some(233),
            answered_at_ms: Some(1_791_381_501_820),
        },
        list: DevToolList {
            listing: aterm_containment::DevToolListing::Absent,
            label: DevToolListLabel::Read,
            modified_at_s: None,
            rows: Some(1),
        },
        subject: Some("com.aterm.aterm"),
        probe_age_ms: Some(812),
        answer_age_ms: Some(4100),
    };

    /// A snapshot with no OS in it, so the wire format is a total function of
    /// data a test can name.
    fn snapshot(sessions: usize, fda: FdaState, evidence: SpikeEvidence) -> PrivacySnapshot {
        snapshot_with_warmup(sessions, fda, evidence, &[])
    }

    /// The same snapshot, with warm-up rows, so a test can drive the folder row
    /// through the states production reaches instead of asserting a constant.
    fn snapshot_with_warmup(
        sessions: usize,
        fda: FdaState,
        evidence: SpikeEvidence,
        warmup: &[(Folder, crate::consent_warmup::WarmupRow)],
    ) -> PrivacySnapshot {
        let split = covers_split(fda, evidence);
        let folders: Vec<(&'static str, &'static str)> = folder_names()
            .into_iter()
            .map(|name| (name, folder_state(name, warmup, &split.covers)))
            .collect();
        let folder_source = folder_source(&folders);
        PrivacySnapshot {
            folders,
            folder_source,
            // No census answer unless a test sets `claimants` itself.
            claimants: None,
            census_pending: false,
            trash_tool: true,
            platform: "macos",
            os: Some("26.6.2".to_string()),
            identity: SigningIdentity {
                bundle_id: Some("com.aterm.aterm".to_string()),
                display_name: Some("aterm".to_string()),
                signing: "developer-id",
                team: Some("A66A9P66Z7".to_string()),
                dr: DrClass::Identity,
                dr_text: "designated => identifier \"com.aterm.aterm\"".to_string(),
                dev_build: Some(false),
            },
            install: "installed",
            running: Some("/Applications/aterm.app/Contents/MacOS/aterm".to_string()),
            anchor: aterm_containment::ImageAnchor::Live,
            fda,
            probe: ProbeLabel::OpenEperm,
            probe_age_ms: Some(1840),
            evidence,
            attribution_root: Attribution::Live,
            sessions: (0..sessions)
                .map(|i| SessionRow {
                    sid: format!("s-{i}"),
                    attribution: if i == 1 {
                        Attribution::Adopted
                    } else {
                        Attribution::Live
                    },
                    responsible_pid: Some(4711),
                    responsible: Responsible::SelfProcess,
                    fs_consent: FsConsent::Unknown,
                    cwd: Some("/Users//a b/src".to_string()),
                })
                .collect(),
            containment_mode: "user".to_string(),
            seatbelt: "none",
            protected: vec!["~/one".to_string(), "~/two".to_string()],
            warmup: "on-request",
            warmup_last_ms: None,
            debugger: FIXTURE_DEBUGGER,
            devtools: FIXTURE_DEVTOOLS,
            observer_fda: "ok",
            observer_responsible: "ok",
            observer_log: "off",
            access_card: AccessCardReport {
                state: "quiet",
                record: crate::consent_card::AccessRecord::default(),
                needs: 0,
            },
        }
    }

    /// A headless-constructed `App` reaches `tccd` through nothing (design
    /// §3.3 guardrail 2): every probe row names the INERT arm's label. The
    /// live FDA arm from a test binary would answer `refused_out_of_bundle`,
    /// the inert one `refused_disabled`; the responsibility SPI is `off`; and
    /// no debugger or Developer Tools worker is dispatched.
    #[test]
    fn a_headless_app_wires_the_inert_consent_probe_arm() {
        let app = crate::App::headless_for_test();
        let read = app.read_privacy(PrivacyForm::Lines, Scope::Owner);
        let row = |prefix: &str| {
            read.lines
                .iter()
                .find(|l| l.starts_with(prefix))
                .unwrap_or_else(|| panic!("no {prefix} row: {:?}", read.lines))
                .clone()
        };
        let fda = row("full_disk_access=");
        assert!(
            fda.starts_with("full_disk_access=unknown probe=refused_disabled "),
            "{fda}"
        );
        assert!(row("observer ").contains(" responsible=off "));
        for prefix in ["debugger ", "devtools "] {
            let line = row(prefix);
            assert!(line.contains(" probe=refused_disabled "), "{line}");
        }
        assert!(!read.pending);
    }

    /// An edge is shown only the sessions it may read — never the roster
    /// `sessions` withholds from it. Owner sees every session; an edge holding
    /// a read edge into one sibling sees that row alone, in both forms; a
    /// write-only edge, or a token no table knows, sees none. The
    /// instance-wide posture is answered to all of them.
    #[test]
    fn an_edge_sees_only_the_sessions_it_may_read() {
        use aterm_session::{EdgeToken, Op, SessionId};
        let mut app = crate::App::headless_for_test();
        let sibling = app.pool.insert(crate::stub_session(1));
        let grant = |app: &crate::App, op: Op| {
            let ctx = &app.pool.get(sibling).expect("the sibling").ctx;
            let mut table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            table.grant(SessionId::new("s-src"), ctx.self_id.clone(), op, ctx.nonce)
        };
        let read = Scope::Edge(grant(&app, Op::ReadScreen));
        let write = Scope::Edge(grant(&app, Op::WriteInput));
        let sibling_sid = app
            .pool
            .get(sibling)
            .expect("sid")
            .ctx
            .self_id
            .as_str()
            .to_string();
        let rows = |scope: Scope| -> Vec<String> {
            let lines = app.read_privacy(PrivacyForm::Lines, scope).lines;
            assert!(
                lines.iter().any(|l| l.starts_with("full_disk_access=")),
                "the instance-wide posture answers every scope: {lines:?}"
            );
            lines
                .into_iter()
                .filter(|l| l.starts_with("session "))
                .collect()
        };
        assert_eq!(rows(Scope::Owner).len(), 2, "Owner sees every session");
        let seen = rows(read);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(
            seen[0].starts_with(&format!("session sid={sibling_sid} ")),
            "{seen:?}"
        );
        assert!(rows(write).is_empty(), "a write edge reads no session");
        assert!(rows(Scope::Edge(EdgeToken::generate())).is_empty());
        let json = app.read_privacy(PrivacyForm::Json, read).lines.concat();
        assert_eq!(json.matches("\"sid\":").count(), 1, "{json}");
        assert!(json.contains(&sibling_sid), "{json}");
    }

    /// Every live session contributes exactly one `session` row, with no cap,
    /// and `sessions_total=` counts them; its free-text `cwd` takes the wire's
    /// own pct encoder, which escapes the space that would split the field.
    /// Off macOS there is no consent layer: the report keeps its schema and row
    /// order (agents read rows by position) but says no prompt is possible,
    /// names no OS-protected folder and no Settings pane, and closes with the
    /// note that an EPERM is an ordinary permission. On Linux it read
    /// `prompt_possible=yes`, `protected=~/Documents,…,~/Library/Mail,…` and
    /// `remediate fda=settings:Privacy_AllFiles`. Negative control: the same
    /// snapshot on macOS keeps every one of those.
    #[test]
    fn off_macos_the_report_claims_no_consent_layer_and_keeps_its_shape() {
        let mac = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
        let mut linux = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
        linux.platform = "linux";
        let (mac_lines, linux_lines) = (mac.lines(), linux.lines());
        assert_eq!(mac_lines.len(), linux_lines.len(), "the same rows");
        assert!(
            mac_lines.contains(&"prompt_possible=yes".to_string()),
            "control"
        );
        assert!(
            linux_lines.contains(&"prompt_possible=no".to_string()),
            "{linux_lines:?}"
        );
        assert!(
            linux_lines.contains(&"remediate fda=- files=- debugger=- devtools=-".to_string()),
            "{linux_lines:?}"
        );
        let containment = |lines: &[String]| {
            lines
                .iter()
                .find(|l| l.starts_with("containment "))
                .cloned()
                .unwrap()
        };
        assert!(
            containment(&linux_lines).ends_with(" protected=-"),
            "{linux_lines:?}"
        );
        assert!(
            !containment(&mac_lines).ends_with(" protected=-"),
            "control"
        );
        assert_eq!(
            linux_lines.last().unwrap(),
            &format!("note {}", pct_encode(NOTE_NO_OS_CONSENT))
        );
        let json = cmd_privacy_json(&linux);
        assert!(json.contains("\"prompt_possible\":false"), "{json}");
        assert!(json.contains("\"remediate\":{\"fda\":null"), "{json}");
        assert!(json.contains("\"protected\":[]"), "{json}");
    }

    #[test]
    fn every_live_session_gets_one_row_with_its_cwd_pct_encoded() {
        for count in [0, 1, 40] {
            let lines = snapshot(count, FdaState::Unknown, SpikeEvidence::UNMEASURED).lines();
            let rows: Vec<&String> = lines.iter().filter(|l| l.starts_with("session ")).collect();
            assert_eq!(rows.len(), count);
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&format!(" sessions_total={count} "))),
                "{lines:?}"
            );
            if let Some(row) = rows.first() {
                assert!(row.ends_with(" cwd=/Users//a%20b/src"), "{row}");
            }
        }
    }

    /// The text body is the documented `k=v` contract: each row's keys, each
    /// row once (one `session` row per live session), in any order. The
    /// census summary names its unanswered states — `pending` and `off` are
    /// different answers and neither is a count — and a lone install is
    /// `sole=yes` with no copy row and no census note.
    #[test]
    fn the_line_body_is_the_documented_shape() {
        let keys = |line: &String| -> String {
            line.split(' ')
                .enumerate()
                .filter_map(|(i, field)| match field.split_once('=') {
                    Some((key, _)) => Some(key),
                    None => (i == 0).then_some(field),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut snap = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
        let mut got: Vec<String> = snap.lines().iter().map(keys).collect();
        let folder = std::iter::once("folder")
            .chain(folder_names())
            .chain(["source"])
            .collect::<Vec<_>>()
            .join(" ");
        let session = "session sid attribution responsible_pid responsible fs_consent cwd";
        let mut want: Vec<String> = [
            "schema",
            "platform os",
            "bundle_id display_name signing team dr grant_stable dev_build",
            "install running anchor",
            "full_disk_access probe probe_age_ms fda_scope",
            "covers",
            "uncovered",
            "unmeasured",
            &folder,
            "claimants conflicting census",
            "prompt_possible",
            "attribution_root sessions_total sessions_adopted",
            session,
            session,
            "containment mode seatbelt protected",
            "warmup warmup_last_ms",
            "debugger mode member self_hardened right probe probe_age_ms pending pending_right \
             pending_age_ms last watch_ms",
            "devtools state subject requests not_allowed answer_age_ms window_s probe \
             probe_age_ms grant list list_probe",
            "observer fda responsible log taskport",
            "access_card state asked_at opened_at verified_at renew_at needs",
            "remediate fda files debugger devtools",
            "note",
        ]
        .map(String::from)
        .to_vec();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);

        let summary = |snap: &PrivacySnapshot| {
            snap.lines()
                .into_iter()
                .find(|l| l.starts_with("claimants="))
                .expect("a census row")
        };
        assert_eq!(summary(&snap), "claimants=- conflicting=- census=off");
        snap.census_pending = true;
        assert_eq!(summary(&snap), "claimants=- conflicting=- census=pending");
        snap.claimants = Some((
            census_answer(vec![claimant(
                "/Applications/aterm.app",
                DrClass::Identity,
                "developer-id",
                true,
            )]),
            Duration::ZERO,
        ));
        assert_eq!(
            summary(&snap),
            "claimants=1 conflicting=0 census=complete sole=yes age_ms=0"
        );
        let lines = snap.lines();
        assert!(
            !lines.iter().any(|l| l.starts_with("claimant ")),
            "{lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("note ")).count(),
            1,
            "only the closing note: {lines:?}"
        );
    }

    fn census_answer(found: Vec<aterm_containment::consent::Claimant>) -> Claimants {
        Claimants {
            found,
            enumeration: aterm_containment::consent::Enumeration::Complete,
        }
    }

    fn claimant(
        path: &str,
        dr: DrClass,
        signing: &'static str,
        running: bool,
    ) -> aterm_containment::consent::Claimant {
        aterm_containment::consent::Claimant {
            path: std::path::PathBuf::from(path),
            dr,
            dr_text: format!("designated => {} {path}", dr.as_str()),
            signing,
            team: None,
            running,
            links: Vec::new(),
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
        state.set_completion_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
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
        state.set_completion_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
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

    /// The census rows when a grant is at risk: a Developer-ID install with
    /// three ad-hoc copies. Every row says whether Settings ▸ Security's *Move
    /// to Trash* takes it, and in both forms of the verb only the copy the move
    /// itself takes (`Claimants::retirable`, whose rule `aterm-containment`
    /// tests) reads as offered — and none without the tool. The running copy
    /// is the reference, never a row.
    #[test]
    fn the_census_rows_offer_the_trash_only_for_what_the_move_takes() {
        let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let mut linked = claimant("/Users//a/dev/aterm.app", DrClass::Cdhash, "adhoc", false);
        linked.links = vec![PathBuf::from("/Applications/aterm-dev.app")];
        let census = census_answer(vec![
            claimant(
                "/Applications/aterm.app",
                DrClass::Identity,
                "developer-id",
                true,
            ),
            claimant(
                "/Applications/aterm.app.rollback",
                DrClass::Cdhash,
                "adhoc",
                false,
            ),
            linked,
            claimant(
                "/Users//a/pinned/aterm.app",
                DrClass::Unknown,
                "adhoc",
                false,
            ),
        ]);
        let moved: Vec<PathBuf> = census.retirable().iter().map(|c| c.path.clone()).collect();
        assert_eq!(moved, [PathBuf::from("/Applications/aterm.app.rollback")]);
        snap.claimants = Some((census, Duration::from_millis(1200)));
        // The copies the verb reads as offered, in the text rows and in the
        // JSON's conflicts, each of which must say one way or the other.
        let offered = |snap: &PrivacySnapshot| -> Vec<PathBuf> {
            let lines = snap.lines();
            assert!(
                !lines
                    .iter()
                    .any(|l| l.starts_with("claimant path=/Applications/aterm.app ")),
                "the running copy is the reference: {lines:?}"
            );
            let text: Vec<PathBuf> = lines
                .iter()
                .filter_map(|line| line.strip_prefix("claimant path="))
                .filter_map(|row| {
                    let (path, fields) = row.split_once(' ').expect("fields");
                    let trash = fields
                        .split(' ')
                        .find_map(|field| field.strip_prefix("trash="))
                        .expect("every row says whether the Trash takes it");
                    (trash == "offered").then(|| PathBuf::from(path))
                })
                .collect();
            let json: aterm_json::Value =
                aterm_json::from_str(cmd_privacy_json(snap).lines().nth(1).expect("JSON body"))
                    .expect("the JSON parses");
            let json: Vec<PathBuf> = json["census"]["conflicts"]
                .as_array()
                .expect("an answered census lists its conflicts")
                .iter()
                .filter_map(|row| {
                    let trash = row["trash"]
                        .as_str()
                        .expect("every conflict says whether the Trash takes it");
                    (trash == "offered").then(|| PathBuf::from(row["path"].as_str().expect("path")))
                })
                .collect();
            assert_eq!(text, json, "both forms read one rule");
            text
        };
        assert_eq!(offered(&snap), moved, "only what the move takes is offered");
        snap.trash_tool = false;
        assert_eq!(offered(&snap), Vec::<PathBuf>::new(), "no tool, no offer");
    }

    /// The JSON census object has the same keys in every state, and caps its
    /// list like the text form does.
    #[test]
    fn the_json_census_parses_in_both_states_and_is_capped() {
        let keys = [
            "state",
            "claimants",
            "conflicting",
            "sole",
            "age_ms",
            "conflicts",
            "truncated",
        ];
        let snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
                .expect("pending json parses");
        for key in keys {
            assert!(
                json["census"].get(key).is_some(),
                "{key} in the unanswered form"
            );
        }
        assert_eq!(json["census"]["state"].as_str(), Some("off"));

        let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        let mut found = vec![claimant(
            "/A/aterm.app",
            DrClass::Identity,
            "developer-id",
            true,
        )];
        for n in 0..(aterm_containment::consent::MAX_CLAIMANT_ROWS + 3) {
            found.push(claimant(
                &format!("/A/copy-{n}.app"),
                DrClass::Cdhash,
                "adhoc",
                false,
            ));
        }
        snap.claimants = Some((census_answer(found), Duration::from_millis(7)));
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
                .expect("answered json parses");
        for key in keys {
            assert!(
                json["census"].get(key).is_some(),
                "{key} in the answered form"
            );
        }
        assert_eq!(
            json["census"]["conflicts"].as_array().map(Vec::len),
            Some(aterm_containment::consent::MAX_CLAIMANT_ROWS)
        );
        assert_eq!(json["census"]["truncated"].as_u64(), Some(3));
        assert_eq!(json["census"]["conflicting"].as_u64(), Some(11));
    }

    /// An unmeasured folder's state is never inferred from the grant: before an
    /// observed access the only answer is `unknown`. Under a held grant only
    /// services S4 proved covered read `covered-by-fda`; the four unmeasured
    /// rows stay `unknown`.
    #[test]
    fn no_unmeasured_folder_state_is_ever_inferred_from_the_grant() {
        // S4 could not isolate Documents/Desktop/Downloads, and never exercised
        // the two volume classes.
        const STILL_UNMEASURED: &[&str] = &[
            "documents",
            "desktop",
            "downloads",
            "network-volumes",
            "removable-volumes",
        ];
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
            let folder = lines
                .iter()
                .find(|l| l.starts_with("folder "))
                .expect("a folder row");
            for name in STILL_UNMEASURED {
                assert!(
                    folder.contains(&format!("{name}=unknown")),
                    "{name} has no measurement of its own: {folder}"
                );
            }
            // The measured exception, and only under a held grant.
            let expected = if fda == FdaState::Granted {
                "app-data=covered-by-fda"
            } else {
                "app-data=unknown"
            };
            assert!(folder.contains(expected), "{fda:?}: {folder}");
            let expected_source = if fda == FdaState::Granted {
                " source=fda"
            } else {
                " source=none"
            };
            assert!(folder.ends_with(expected_source), "{fda:?}: {folder}");
        }
    }

    /// An observation aterm made outranks the coverage rule, and a warm-up row
    /// reaches the wire.
    #[test]
    fn an_observed_warmup_row_outranks_the_coverage_rule_and_reaches_the_wire() {
        use crate::consent_warmup::WarmupRow;
        let warmed = [
            (Folder::Documents, WarmupRow::Denied),
            (Folder::Desktop, WarmupRow::Allowed),
            // An observed denial for a class the grant is measured to cover
            // wins: the grant is a rule, this is a measurement.
            (Folder::AppData, WarmupRow::Denied),
        ];
        let lines =
            snapshot_with_warmup(1, FdaState::Granted, SpikeEvidence::UNMEASURED, &warmed).lines();
        let folder = lines
            .iter()
            .find(|l| l.starts_with("folder "))
            .expect("a folder row");
        assert!(folder.contains("documents=denied"), "{folder}");
        assert!(folder.contains("desktop=allowed"), "{folder}");
        assert!(folder.contains("app-data=denied"), "{folder}");
        assert!(folder.contains("downloads=unknown"), "{folder}");
        // Provenance names the warm-up; `fda` is absent because the one service
        // the rule would have covered was overridden by an observation.
        assert!(folder.ends_with(" source=warmup"), "{folder}");

        // A row the worker has not answered yet is NOT a verdict.
        let asking = [(Folder::Documents, WarmupRow::Asking)];
        let lines =
            snapshot_with_warmup(1, FdaState::Denied, SpikeEvidence::UNMEASURED, &asking).lines();
        let folder = lines
            .iter()
            .find(|l| l.starts_with("folder "))
            .expect("a folder row");
        assert!(folder.contains("documents=asking"), "{folder}");
        assert!(folder.ends_with(" source=warmup"), "{folder}");
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
        state.set_debugger_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
        let wait = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };

        let cold = state.debugger(ProbeGate::on());
        assert_eq!(cold, DebuggerRow::refused(DebuggerProbeLabel::Pending));
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
        state
            .shared
            .debugger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retry_after = None;
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

    /// `[privacy] enabled = false`: the debugger row reads `unknown` with
    /// `probe=refused_disabled` (design §4.1) and consults nothing — no
    /// `DevToolsSecurity`, no `csops` — and a warm answer is retired, so
    /// re-enabling asks again.
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
        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let lines = app.read_privacy(PrivacyForm::Lines, Scope::Owner).lines;
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
        state
            .shared
            .debugger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry = Some((
            DebuggerEntry {
                probe: fake_debugger(&AtomicUsize::new(0)),
                self_hardened: SelfHardening::Hardened,
            },
            Instant::now(),
        ));
        let closed = ProbeGate {
            enabled: false,
            check: true,
        };
        assert_eq!(
            state.debugger(closed),
            DebuggerRow::refused(DebuggerProbeLabel::RefusedDisabled)
        );
        assert!(
            state
                .shared
                .debugger
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry
                .is_none(),
            "the closed gate retired the warm answer"
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 0);
    }

    /// Only the full report dispatches the debugger and Developer Tools
    /// workers (design §8 P0 item 2): the FDA `await consent` tuple,
    /// `status`'s `session_consent`, `debugger_cached` and `devtools_cached`
    /// (a hidden Security page's read) are polled, and none may start a
    /// `DevToolsSecurity -status` run, read the hardening bits or start a
    /// `log show` — nor may the debugger tuple start the latter — even with
    /// the gate open, the observer on, a subject and live-flagged counting
    /// arms. (`await consent kind=debugger` asks the debugger slot, under its
    /// TTL.) The positive control is the same App's `Lines` read, which
    /// dispatches both — so the silence before it is the path, not a closed
    /// gate.
    #[test]
    fn the_polled_consent_paths_never_dispatch_a_probe_worker() {
        use std::sync::atomic::AtomicUsize;
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static HARDENING_READS: AtomicUsize = AtomicUsize::new(0);
        static DEVTOOLS_RUNS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_gate: ProbeGate) -> DebuggerProbe {
            fake_debugger(&RUNS)
        }
        fn hardened() -> SelfHardening {
            HARDENING_READS.fetch_add(1, Ordering::AcqRel);
            SelfHardening::Hardened
        }
        fn counting_devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            fake_devtools(&DEVTOOLS_RUNS, 0)
        }
        let mut app = crate::App::headless_for_test();
        app.consent.probes.live = true;
        app.consent.probes.debugger = counting;
        app.consent.probes.hardening = hardened;
        app.consent.probes.devtools = counting_devtools;
        app.consent.probes.subject = fake_subject;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_debugger_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
        let (devtools_tx, devtools_rx) = std::sync::mpsc::channel();
        app.consent.set_devtools_wake(Arc::new(move || {
            let _ = devtools_tx.send(());
        }));
        assert!(
            app.consent_policy().gate.permits(),
            "the gate is open, so only the path can keep the probes quiet"
        );
        let debugger_idle = |app: &crate::App| {
            let slot = app
                .consent
                .shared
                .debugger
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            !slot.in_flight && slot.entry.is_none()
        };
        let devtools_idle = |app: &crate::App| {
            let slot = app.consent.shared.devtools.lock_or_recover();
            !slot.in_flight && slot.entry.is_none()
        };

        for _ in 0..3 {
            let _ = app.read_privacy(PrivacyForm::Tuple(0), Scope::Owner);
            let _ = app.session_consent(0, None);
            let _ = app.consent.debugger_cached(ProbeGate::on());
            let _ = app.consent.devtools_cached(ProbeGate::on(), false);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 0, "a poll spawned the probe");
        assert_eq!(HARDENING_READS.load(Ordering::Acquire), 0);
        assert!(
            debugger_idle(&app),
            "a poll left a debugger worker in flight"
        );
        for _ in 0..3 {
            let _ = app.read_privacy(PrivacyForm::DebuggerTuple(0), Scope::Owner);
        }
        assert_eq!(
            DEVTOOLS_RUNS.load(Ordering::Acquire),
            0,
            "a poll ran log show"
        );
        assert!(
            devtools_idle(&app),
            "a poll left a devtools worker in flight"
        );

        // The positive control: the full report dispatches both.
        let _ = app.read_privacy(PrivacyForm::Lines, Scope::Owner);
        devtools_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the full report's devtools worker publishes");
        assert_eq!(DEVTOOLS_RUNS.load(Ordering::Acquire), 1);
        assert!(!devtools_idle(&app), "the full report's answer is cached");
        if RUNS.load(Ordering::Acquire) == 0 {
            rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the full report's debugger worker publishes");
        }
        assert!(RUNS.load(Ordering::Acquire) >= 1);
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
        state.set_devtools_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
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
        state.shared.devtools.lock_or_recover().retry_after = None;
        assert!(state.devtools(ProbeGate::on(), false).pending());
        wait();
        assert!(!state.devtools(ProbeGate::on(), false).pending());
        assert_eq!(RUNS.load(Ordering::Acquire), 2);

        // Past the TTL one worker refreshes it, the stale answer served
        // meanwhile.
        {
            let mut slot = state.shared.devtools.lock_or_recover();
            let entry = slot.entry.as_mut().expect("a cached answer");
            entry.1 = Instant::now()
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
            app.read_privacy(PrivacyForm::Lines, Scope::Owner)
                .lines
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
            app.consent.shared.devtools.lock_or_recover().entry = Some((
                DevToolsEntry {
                    probe: fake_devtools(&AtomicUsize::new(0), 0),
                    list: DevToolList::pending(),
                    subject: fake_subject(),
                },
                Instant::now(),
            ));
        };
        let cold = |app: &crate::App| {
            app.consent
                .shared
                .devtools
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
            app.config.privacy = Some(privacy);
            warm(&app);
            refused(&app, "refused_disabled");
            assert!(cold(&app), "the closed gate retired the warm answer");
        }
        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            observer: Some(false),
            ..Default::default()
        });
        warm(&app);
        refused(&app, "refused_observer_off");
        assert!(cold(&app), "observer = false retired the warm answer");
        app.config.privacy = None;
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent.set_devtools_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
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
        app.consent.shared.devtools.lock_or_recover().retry_after = None;
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
        state.set_devtools_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
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

    /// `await consent kind=debugger` end to end on the read path, with a fake
    /// probe arm standing in for `DevToolsSecurity -status`: a cold slot is
    /// pending and arms nothing; the first completed reading is the baseline
    /// and cannot latch; twenty more polls inside `DEBUGGER_TTL` run the probe
    /// no further time; a retired answer (the owner's change re-probing, or
    /// focus gain) is pending again, not a change; and the re-read that
    /// reports another mode latches `debugger_mode=enabled`. A wait addressed
    /// through a session that is gone is `Exited`. (One refresh per TTL is the
    /// shared slot's, driven by the Developer Tools test.)
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
        app.consent.set_debugger_wake(Arc::new(move || {
            let _ = tx.send(());
        }));
        let published = || {
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the worker publishes")
        };
        // Step past the retry floor rather than race it.
        let clear_floor = |app: &crate::App| {
            app.consent
                .shared
                .debugger
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .retry_after = None;
        };
        let observe = |app: &crate::App| {
            consent_observation(
                app.read_privacy(PrivacyForm::DebuggerTuple(0), Scope::Owner)
                    .lines,
            )
        };
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
        assert_eq!(
            baseline,
            ConsentObservation::Complete("debugger_mode=disabled".to_string())
        );
        assert_eq!(wait.observe_at(baseline, now), ConsentWaitDecision::Wait);
        // Polls inside the TTL are served from the slot: no probe per poll,
        // even with the rule changed underneath (only a re-read can see it).
        MODE.store(1, Ordering::Release);
        for _ in 0..20 {
            clear_floor(&app);
            let same = observe(&app);
            assert_eq!(wait.observe_at(same, now), ConsentWaitDecision::Wait);
        }
        assert_eq!(RUNS.load(Ordering::Acquire), 1, "a poll spawned the probe");

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
            ConsentWaitDecision::Changed("debugger_mode=enabled".to_string())
        );
        assert_eq!(RUNS.load(Ordering::Acquire), 2);

        // A session that is gone ends the wait.
        assert_eq!(
            consent_observation(
                app.read_privacy(PrivacyForm::DebuggerTuple(999), Scope::Owner)
                    .lines
            ),
            ConsentObservation::Exited
        );
        assert_eq!(
            wait.observe_at(ConsentObservation::Exited, now),
            ConsentWaitDecision::Exited
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

        app.config.privacy = Some(crate::app_config::PrivacyConfig {
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

        let lines = app.read_privacy(PrivacyForm::Lines, Scope::Owner).lines;
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

    /// A grant that cannot be validated gets its own note, keyed on the
    /// anchor: `running=` and `full_disk_access=granted` can both read healthy
    /// after a swap, and the probe's `granted` must not stand alone. The two
    /// non-faults — a dev run outside a bundle, a path that could not be
    /// read — never raise it.
    #[test]
    fn an_unvalidatable_identity_is_reported_and_does_not_hide_behind_a_granted_probe() {
        use aterm_containment::ImageAnchor;
        for (anchor, notes) in [
            (ImageAnchor::Live, 1),
            (ImageAnchor::NotBundled, 1),
            (ImageAnchor::Unknown, 1),
            (ImageAnchor::Displaced, 2),
            (ImageAnchor::Deleted, 2),
        ] {
            let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
            snap.anchor = anchor;
            let lines = snap.lines();
            assert!(
                lines
                    .iter()
                    .any(|l| l.ends_with(&format!(" anchor={}", anchor.as_str()))),
                "{anchor:?}: {lines:?}"
            );
            assert_eq!(
                lines.iter().filter(|l| l.starts_with("note ")).count(),
                notes,
                "{anchor:?}: {lines:?}"
            );
        }
    }

    /// `remediate` names only PLACES — Settings panes and aterm's Security
    /// page — never a command: an enable token would hand an in-session agent
    /// the debugger switch (design §4.1), and no command a program could run
    /// adds aterm to the Developer Tools list. Free text rides one
    /// pct-encoded field.
    #[test]
    fn the_remediate_row_names_only_places() {
        let lines = snapshot(1, FdaState::Denied, SpikeEvidence::UNMEASURED).lines();
        let remediate = lines
            .iter()
            .find_map(|l| l.strip_prefix("remediate "))
            .expect("a remediate row");
        for field in remediate.split(' ') {
            let (_, place) = field.split_once('=').expect("k=v");
            assert!(
                place.starts_with("settings:") || place.starts_with("aterm-settings:"),
                "{field}"
            );
        }
        assert_eq!(
            lines.last().expect("a note row").split(' ').count(),
            2,
            "one field after `note`"
        );
    }

    /// The observer row's three answers: `off` is a probe not consulted,
    /// `unavailable` one that could not answer, and `ok` one that ran —
    /// whatever it answered, a denial or the SPI's "not yours" included.
    #[test]
    fn unavailable_is_distinct_from_off_and_from_a_negative_answer() {
        for (label, want) in [
            (ProbeLabel::RefusedDisabled, "off"),
            (ProbeLabel::RefusedOutOfBundle, "unavailable"),
            (ProbeLabel::RefusedNoHome, "unavailable"),
            (ProbeLabel::UnsupportedPlatform, "unavailable"),
            (ProbeLabel::OpenOk, "ok"),
            (ProbeLabel::OpenEperm, "ok"),
            (ProbeLabel::OpenErrno(13), "ok"),
        ] {
            assert_eq!(observer_fda_value(label), want, "{label:?}");
        }
        for (answers, want) in [
            (&[Err(ResponsibleError::Unsupported)][..], "off"),
            (
                &[Err(ResponsibleError::SymbolUnavailable)][..],
                "unavailable",
            ),
            (&[Err(ResponsibleError::NotOurs), Ok(1)][..], "ok"),
            (&[][..], "off"),
        ] {
            assert_eq!(observer_responsible_value(answers), want, "{answers:?}");
        }
    }

    /// Where the Full Disk Access question stands: `renew_at` is a week after
    /// the ask (`-` unset), and the state token ranks the owner's *Not now*
    /// over a grant and a grant over an ask, and lapses an ask to `quiet`
    /// once its week is spent.
    #[test]
    fn the_access_card_row_reports_where_the_question_stands() {
        use crate::consent_card::{AccessRecord, Marker, RENEW_AFTER, report_state};
        let asked = AccessRecord {
            asked_at: Some(1_000),
            ..AccessRecord::default()
        };
        let week = 1_000 + RENEW_AFTER.as_secs();
        let mut snap = snapshot(0, FdaState::Denied, SpikeEvidence::UNMEASURED);
        snap.access_card = AccessCardReport {
            state: "asked",
            record: asked,
            needs: 3,
        };
        let row = snap
            .lines()
            .into_iter()
            .find(|l| l.starts_with("access_card "))
            .expect("an access_card row");
        let renew = format!("renew_at={week}");
        for field in [renew.as_str(), "verified_at=-", "needs=3"] {
            assert!(row.split(' ').any(|f| f == field), "{field}: {row}");
        }
        for (marker, record, fda, now, want) in [
            (None, AccessRecord::default(), FdaState::Denied, 5, "quiet"),
            (
                Some(Marker::NotNow),
                asked,
                FdaState::Granted,
                5,
                "declined",
            ),
            (None, asked, FdaState::Granted, 5, "granted"),
            (None, asked, FdaState::Denied, 1_500, "asked"),
            (None, asked, FdaState::Denied, week, "quiet"),
        ] {
            assert_eq!(report_state(marker, &record, fda, now), want, "{want}");
        }
    }

    /// The `--json` form is one framed body line that parses, carries the
    /// sub-objects the contract names, escapes rather than pct-encodes, and
    /// keeps every key in every state: a row with nothing observed carries
    /// `null` where the text form prints `-`.
    #[test]
    fn the_json_form_carries_the_documented_sub_objects() {
        let parse = |snap: &PrivacySnapshot| -> aterm_json::Value {
            let body = cmd_privacy_json(snap);
            assert!(body.starts_with("OK 1\n"), "{body}");
            assert_eq!(body.lines().count(), 2, "a JSON reply is one body line");
            aterm_json::from_str(body.lines().nth(1).expect("a body line")).expect("it parses")
        };
        let observed = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
        let json = parse(&observed);
        for key in ["folders", "debugger", "devtools", "observers", "remediate"] {
            assert!(json[key].is_object(), "{key}: {json:?}");
        }
        assert_eq!(json["sessions"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["sessions"][0]["cwd"].as_str(), Some("/Users//a b/src"));
        assert_eq!(json["debugger"]["probe_age_ms"].as_u64(), Some(1204));
        assert_eq!(json["devtools"]["answer_age_ms"].as_u64(), Some(4100));

        let mut unobserved = observed.clone();
        unobserved.debugger = DebuggerRow::refused(DebuggerProbeLabel::Pending);
        unobserved.devtools = DevToolsRow::refused(DevToolProbeLabel::Pending, None);
        let pending = parse(&unobserved);
        for row in ["debugger", "devtools"] {
            let keys = |v: &aterm_json::Value| -> Vec<String> {
                v[row]
                    .as_object()
                    .expect("an object")
                    .keys()
                    .cloned()
                    .collect()
            };
            assert_eq!(keys(&pending), keys(&json), "{row}: no key dropped");
            assert!(pending[row]["probe_age_ms"].is_null(), "{row}");
        }
        assert!(pending["devtools"]["subject"].is_null());
    }

    /// The verb's grammar: the flag in both spellings, and a usage error for
    /// anything else.
    #[test]
    fn the_verb_takes_only_the_json_flag() {
        assert_eq!(parse_privacy_form(""), Ok(PrivacyForm::Lines));
        assert_eq!(parse_privacy_form("   "), Ok(PrivacyForm::Lines));
        assert_eq!(parse_privacy_form("--json"), Ok(PrivacyForm::Json));
        assert_eq!(parse_privacy_form("json"), Ok(PrivacyForm::Json));
        let err = parse_privacy_form("folders").expect_err("a guessed modifier is refused");
        assert!(err.starts_with("ERR usage: privacy [--json]"), "{err}");
        assert!(err.contains("folders"), "the usage echoes the input: {err}");
    }

    /// `await consent`'s deadline: finite by default (the dialog it waits
    /// behind never expires) and capped with the rest of the wait family, for
    /// either kind and in either order; an unknown kind is the usage line,
    /// never a silent fall back to the FDA tuple.
    #[test]
    fn await_consent_has_a_finite_default_and_a_shared_ceiling() {
        let args = |kind, timeout_ms| Ok(ConsentArgs { kind, timeout_ms });
        for (rest, want) in [
            ("consent", args(ConsentKind::Fda, 300_000)),
            ("consent timeout 1500", args(ConsentKind::Fda, 1_500)),
            ("consent timeout=99999999", args(ConsentKind::Fda, 600_000)),
            (
                "consent kind=debugger",
                args(ConsentKind::Debugger, 300_000),
            ),
            (
                "consent timeout 2500 kind=debugger",
                args(ConsentKind::Debugger, 2_500),
            ),
            (
                "consent kind=debugger timeout=99999999",
                args(ConsentKind::Debugger, 600_000),
            ),
        ] {
            assert_eq!(parse_consent_args(rest), want, "{rest:?}");
        }
        for bad in [
            "consent kind=fda",
            "consent kind debugger",
            "consent idle 10",
        ] {
            assert!(
                parse_consent_args(bad).is_err_and(|e| e.starts_with("ERR usage: await consent")),
                "{bad:?}"
            );
        }
    }

    /// A debugger reading that observed no mode is the internal pending
    /// marker, which the waiter treats as neither a baseline nor a change.
    #[test]
    fn a_debugger_reading_with_no_mode_is_pending_to_the_waiter() {
        for label in [
            DebuggerProbeLabel::Pending,
            DebuggerProbeLabel::RefusedDisabled,
            DebuggerProbeLabel::TimedOut,
            DebuggerProbeLabel::StatusUnrecognized,
        ] {
            assert_eq!(
                consent_observation(vec![debugger_tuple_line(DebuggerProbe::refused(label))]),
                ConsentObservation::Pending,
                "{label:?} observed no mode"
            );
        }
    }

    /// The tuple `await consent` arms on and replies with: the three values
    /// the contract names, each read from its own source.
    #[test]
    fn the_await_baseline_is_the_whole_consent_tuple() {
        let line = |fs_consent, fda, attribution| {
            let row = SessionRow {
                sid: "s-1".to_string(),
                attribution,
                responsible_pid: None,
                responsible: Responsible::Unknown,
                fs_consent,
                cwd: None,
            };
            PrivacySnapshot::tuple_line(&row, fda)
        };
        assert_eq!(
            line(FsConsent::Unknown, FdaState::Denied, Attribution::Live),
            "fs_consent=unknown fda=denied attribution=live"
        );
        assert_eq!(
            line(FsConsent::Denied, FdaState::Granted, Attribution::Adopted),
            "fs_consent=denied fda=granted attribution=adopted"
        );
    }
}

#[cfg(test)]
mod asynchronous_probe_tests {
    use super::*;
    use std::sync::mpsc;

    fn synthetic_state() -> ConsentState {
        let mut state = ConsentState::inert();
        state.probes.live = true;
        state
    }

    fn key() -> ConsentKey {
        ConsentKey::new("synthetic.app", "synthetic requirement")
    }

    fn grant() -> FdaProbe {
        consent::classify_probe(consent::ProbeOutcome::Ok)
    }

    fn probe_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AsyncConsentProbePublication {
                const Buggy = 0;
                var epoch = 0;
                var request_epoch = 0;
                var flight = 0;
                var cached = 0;
                var cached_epoch = 0;
                var ready = 1;
                action Start when (flight == 0 && cached == 0 && ready == 1) {
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
                invariant OneWorkerOrCache: flight + cached <= 1;
            }
        }
    }

    #[test]
    fn async_probe_model_proves_and_catches_stale_publication() {
        let model = probe_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
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
    fn focus_and_identity_churn_conform_to_the_admission_floor() {
        let model = admission_model();
        for change_identity in [false, true] {
            let state = synthetic_state();
            let now = Instant::now();
            let mut expected = model.init_state();
            let ttl = Duration::from_secs(5);
            let (_, request) = state.read_at(ProbeGate::on(), ttl, key(), now);
            assert!(model.fire("Start", &mut expected));
            state.shared.complete(&request.unwrap(), grant(), now);
            assert!(model.fire("Complete", &mut expected));
            for index in 0..100 {
                let at = now + Duration::from_millis(index);
                state.invalidate();
                assert!(model.fire("Invalidate", &mut expected));
                let current_key = if change_identity {
                    ConsentKey::new("synthetic.app", format!("requirement-{index}"))
                } else {
                    key()
                };
                let (answer, request) = state.read_at(ProbeGate::on(), ttl, current_key, at);
                assert_eq!(answer.0.label, ProbeLabel::Pending);
                assert_eq!(request.is_some(), model.action_enabled("Start", &expected));
                assert!(!state.shared.slot.lock().unwrap().in_flight);
                assert_eq!(
                    state.next_refresh_deadline(at, ttl),
                    Some(now + PROBE_RETRY_FLOOR)
                );
                // An invalidation that clears the deadline fails the model
                // before another worker could be admitted.
                let mut cleared_floor = expected.clone();
                cleared_floor.insert("ready", 1);
                assert!(!model.check_invariant("FloorSurvivesInvalidation", &cleared_floor));
            }
            assert!(model.fire("Cooldown", &mut expected));
            let (_, request) = state.read_at(ProbeGate::on(), ttl, key(), now + PROBE_RETRY_FLOOR);
            assert_eq!(request.is_some(), model.action_enabled("Start", &expected));
            assert!(request.is_some());
        }
    }

    fn wait_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AsyncConsentWaitObservation {
                const Buggy = 0;
                var baseline = 0;
                var result = 0;
                var expired = 0;
                var complete = 0;
                var armed = 0;
                action Pending when (result == 0 && expired == 0) {
                    complete = 0;
                    armed = if baseline == 0 { 0 } else { 1 };
                    result = if Buggy == 1 && baseline > 0 { 1 } else { 0 };
                }
                action ObserveA when (result == 0 && expired == 0) {
                    complete = 1;
                    armed = if baseline == 0 { 0 } else { 1 };
                    result = if baseline == 2 { 1 } else { 0 };
                    baseline = if baseline == 0 { 1 } else { baseline };
                }
                action ObserveB when (result == 0 && expired == 0) {
                    complete = 1;
                    armed = if baseline == 0 { 0 } else { 1 };
                    result = if baseline == 1 { 1 } else { 0 };
                    baseline = if baseline == 0 { 2 } else { baseline };
                }
                action Exit when (result == 0 && expired == 0) { result = 2; }
                action Expire when (result == 0 && expired == 0) { expired = 1; }
                action Timeout when (result == 0 && expired == 1) { result = 3; }
                invariant OnlyCompletedChanges: result == 0 || result > 1 || (complete == 1 && armed == 1);
                invariant DeadlineWins: expired == 0 || result == 0 || result == 3;
                invariant Bounds: baseline <= 2 && result <= 3 && expired <= 1 && complete <= 1 && armed <= 1;
            }
        }
    }

    #[test]
    fn wait_model_proves_and_catches_pending_refresh_as_a_false_change() {
        let model = wait_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn real_await_guards_conform_for_cold_warm_pending_and_deadline_observations() {
        let model = wait_model();
        for sequence in 0..256 {
            for expire_at in 0..=4 {
                let now = Instant::now();
                let deadline = now + Duration::from_millis(10);
                let mut wait = ConsentWait::new(now, Duration::from_millis(10));
                let mut expected = model.init_state();
                for index in 0..4 {
                    let choice = (sequence >> (index * 2)) & 3;
                    let (action, observation) = match choice {
                        0 => ("Pending", ConsentObservation::Pending),
                        1 => ("ObserveA", ConsentObservation::Complete("A".into())),
                        2 => ("ObserveB", ConsentObservation::Complete("B".into())),
                        _ => ("Exit", ConsentObservation::Exited),
                    };
                    let at = if index >= expire_at { deadline } else { now };
                    if index >= expire_at {
                        assert!(model.fire("Expire", &mut expected));
                        assert!(model.fire("Timeout", &mut expected));
                    } else {
                        assert!(model.fire(action, &mut expected));
                    }
                    let result = wait.observe_at(observation, at);
                    let projected = match result {
                        ConsentWaitDecision::Wait => 0,
                        ConsentWaitDecision::Changed(_) => 1,
                        ConsentWaitDecision::Exited => 2,
                        ConsentWaitDecision::TimedOut => 3,
                    };
                    assert_eq!(projected, expected["result"]);
                    assert_eq!(
                        match wait.baseline.as_deref() {
                            None => 0,
                            Some("A") => 1,
                            Some("B") => 2,
                            other => panic!("unexpected baseline: {other:?}"),
                        },
                        expected["baseline"]
                    );
                    assert_eq!(wait.deadline, deadline, "pending cannot extend the request");
                    assert!(model.check_invariant("OnlyCompletedChanges", &expected));
                    assert!(model.check_invariant("DeadlineWins", &expected));
                    if action == "Pending" && expected["baseline"] != 0 && projected == 0 {
                        let mut old_string_comparison = expected.clone();
                        old_string_comparison.insert("result", 1);
                        assert!(
                            !model.check_invariant("OnlyCompletedChanges", &old_string_comparison)
                        );
                    }
                    if projected != 0 {
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn expiring_real_cache_never_completes_await_until_the_observed_tuple_changes() {
        let state = synthetic_state();
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let row = SessionRow {
            sid: "s-synthetic".into(),
            attribution: Attribution::Live,
            responsible_pid: None,
            responsible: Responsible::Unknown,
            fs_consent: FsConsent::Unknown,
            cwd: None,
        };
        let sample =
            |probe| consent_observation(vec![PrivacySnapshot::observed_tuple_line(&row, probe)]);
        let mut wait = ConsentWait::new(now, ttl * 4);
        let ((pending, _), request) = state.read_at(ProbeGate::on(), ttl, key(), now);
        assert_eq!(
            wait.observe_at(sample(pending), now),
            ConsentWaitDecision::Wait
        );
        assert!(wait.baseline.is_none(), "cold pending is not the baseline");
        let denied = consent::classify_probe(consent::ProbeOutcome::Errno(consent::ERRNO_EPERM));
        state.shared.complete(&request.unwrap(), denied, now);
        let ((probe, _), _) = state.read_at(ProbeGate::on(), ttl, key(), now);
        assert_eq!(
            wait.observe_at(sample(probe), now),
            ConsentWaitDecision::Wait
        );
        let baseline = wait.baseline.clone();
        for (at, refreshed) in [(now + ttl, denied), (now + ttl * 2, grant())] {
            let ((pending, _), request) = state.read_at(ProbeGate::on(), ttl, key(), at);
            assert_eq!(pending.label, ProbeLabel::Pending);
            assert_eq!(
                wait.observe_at(sample(pending), at),
                ConsentWaitDecision::Wait
            );
            assert_eq!(wait.baseline, baseline, "refresh retains the armed tuple");
            state.shared.complete(&request.unwrap(), refreshed, at);
            let ((probe, _), _) = state.read_at(ProbeGate::on(), ttl, key(), at);
            let result = wait.observe_at(sample(probe), at);
            if refreshed == denied {
                assert_eq!(result, ConsentWaitDecision::Wait);
            } else {
                assert_eq!(
                    result,
                    ConsentWaitDecision::Changed(PrivacySnapshot::tuple_line(
                        &row,
                        FdaState::Granted
                    ))
                );
            }
        }
    }

    /// Tier-1: admission and publication are the exact shipping seams. Replay
    /// every placement of two invalidations around two requests, including an
    /// invalidation while the first worker is held. A stale worker retains its
    /// slot and cannot publish a grant when it eventually finishes.
    #[test]
    fn async_probe_real_publication_conforms_to_the_bounded_model() {
        for invalidations_before in 0..=2 {
            for invalidations_during in 0..=2 - invalidations_before {
                let state = synthetic_state();
                // Establish the key without changing the model's initial epoch.
                state.shared.slot.lock().unwrap().key = Some(key());
                let now = Instant::now();
                let ttl = Duration::from_secs(5);
                let model = probe_model();
                let mut expected = model.init_state();
                let observed_at = std::cell::Cell::new(now);
                let check = |expected: &std::collections::BTreeMap<&'static str, i64>| {
                    let slot = state.shared.slot.lock().unwrap();
                    let epoch = state.shared.epoch.load(Ordering::Acquire);
                    assert_eq!(epoch as i64, expected["epoch"]);
                    assert_eq!(slot.request_epoch as i64, expected["request_epoch"]);
                    assert_eq!(i64::from(slot.in_flight), expected["flight"]);
                    assert_eq!(i64::from(slot.entry.is_some()), expected["cached"]);
                    assert_eq!(
                        i64::from(slot.retry_after.is_none_or(|at| at <= observed_at.get())),
                        expected["ready"]
                    );
                    assert_eq!(
                        slot.entry.as_ref().map_or(0, |e| e.request.epoch) as i64,
                        expected["cached_epoch"]
                    );
                    assert!(model.check_invariant("CurrentPublication", expected));
                    assert!(model.check_invariant("OneWorkerOrCache", expected));
                };
                for _ in 0..invalidations_before {
                    state.invalidate();
                    assert!(model.fire("Invalidate", &mut expected));
                    check(&expected);
                }
                let (pending, request) = state.read_at(ProbeGate::on(), ttl, key(), now);
                assert_eq!(pending.0.label, ProbeLabel::Pending);
                let request = request.unwrap();
                assert!(model.fire("Start", &mut expected));
                check(&expected);
                for _ in 0..invalidations_during {
                    state.invalidate();
                    assert!(model.fire("Invalidate", &mut expected));
                    check(&expected);
                }
                for _ in 0..100 {
                    let (pending, next) = state.read_at(ProbeGate::on(), ttl, key(), now);
                    assert_eq!(pending.0.label, ProbeLabel::Pending);
                    assert!(next.is_none());
                    assert!(!model.action_enabled("Start", &expected));
                    check(&expected);
                }
                let mut old_unchecked_publisher = expected.clone();
                old_unchecked_publisher.insert("flight", 0);
                old_unchecked_publisher.insert("cached", 1);
                old_unchecked_publisher.insert("cached_epoch", request.epoch as i64);
                if invalidations_during != 0 {
                    assert!(!model.check_invariant("CurrentPublication", &old_unchecked_publisher));
                }
                state.shared.complete(&request, grant(), now);
                assert!(model.fire("Complete", &mut expected));
                check(&expected);
                if invalidations_during != 0 {
                    let (pending, blocked) = state.read_at(ProbeGate::on(), ttl, key(), now);
                    assert_eq!(pending.0.label, ProbeLabel::Pending);
                    assert!(
                        blocked.is_none(),
                        "invalidation retains the admission floor"
                    );
                    assert!(!model.action_enabled("Start", &expected));
                    observed_at.set(now + PROBE_RETRY_FLOOR);
                    assert!(model.fire("Cooldown", &mut expected));
                    let (pending, current) =
                        state.read_at(ProbeGate::on(), ttl, key(), observed_at.get());
                    assert_eq!(pending.0.state, FdaState::Unknown);
                    assert!(model.fire("Start", &mut expected));
                    check(&expected);
                    state
                        .shared
                        .complete(&current.unwrap(), grant(), observed_at.get());
                    assert!(model.fire("Complete", &mut expected));
                    check(&expected);
                }
                assert_eq!(
                    state
                        .read_at(ProbeGate::on(), ttl, key(), observed_at.get())
                        .0
                        .0,
                    grant()
                );
                // A real expired cache is retired by the same read that admits
                // its refresh. Model the two decisions separately.
                observed_at.set(observed_at.get() + ttl);
                assert!(model.fire("Cooldown", &mut expected));
                let (pending, refresh) =
                    state.read_at(ProbeGate::on(), ttl, key(), observed_at.get());
                assert_eq!(pending.0.state, FdaState::Unknown);
                assert!(refresh.is_some());
                assert!(model.fire("Expire", &mut expected));
                assert!(model.fire("Start", &mut expected));
                check(&expected);
            }
        }
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
                let (_, request) = state.read_at(ProbeGate::on(), ttl, key(), now);
                let request = request.unwrap();
                if finish_before_disable {
                    state.shared.complete(&request, grant(), now);
                }
                let (disabled, next) = state.read_at(gate, ttl, key(), now);
                assert_eq!(disabled.0.label, ProbeLabel::RefusedDisabled);
                assert_eq!(disabled.0.state, FdaState::Unknown);
                assert!(next.is_none());
                if !finish_before_disable {
                    assert!(state.shared.slot.lock().unwrap().in_flight);
                    state.shared.complete(&request, grant(), now);
                }
                let (reenabled, request) =
                    state.read_at(ProbeGate::on(), ttl, key(), now + PROBE_RETRY_FLOOR);
                assert_eq!(reenabled.0.label, ProbeLabel::Pending);
                assert!(request.is_some(), "reenabling requires a new probe");
            }
        }
    }

    #[test]
    fn identity_changes_and_contended_invalidation_refuse_old_grants() {
        let state = synthetic_state();
        let now = Instant::now();
        let ttl = Duration::from_secs(5);
        let (_, old) = state.read_at(ProbeGate::on(), ttl, key(), now);
        let other = ConsentKey::new("another.app", "another requirement");
        assert!(
            state
                .read_at(ProbeGate::on(), ttl, other.clone(), now)
                .1
                .is_none()
        );
        state.shared.complete(&old.unwrap(), grant(), now);
        let now = now + PROBE_RETRY_FLOOR;
        let (answer, request) = state.read_at(ProbeGate::on(), ttl, other.clone(), now);
        assert_eq!(answer.0.label, ProbeLabel::Pending);
        state.shared.complete(&request.unwrap(), grant(), now);
        let guard = state.shared.slot.lock().unwrap();
        // Neither operation can wait on this held lock, including the policy
        // invalidation. The old answer stays physically cached until unlocked.
        assert_eq!(
            state
                .read_at(ProbeGate::on(), ttl, other.clone(), now)
                .0
                .0
                .label,
            ProbeLabel::Pending
        );
        state.invalidate();
        drop(guard);
        assert_eq!(
            state.read_at(ProbeGate::on(), ttl, other, now).0.0.label,
            ProbeLabel::Pending
        );
    }

    #[test]
    fn pending_deadlines_stay_future_and_cold_inert_reads_stay_inert() {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::from_millis(1), key(), now);
        assert!(request.is_some());
        for step in 0..100 {
            let at = now + Duration::from_millis(step);
            assert_eq!(
                state.next_refresh_deadline(at, Duration::from_millis(1)),
                Some(at + PROBE_RETRY_FLOOR)
            );
            assert!(
                state
                    .read_at(ProbeGate::on(), Duration::from_millis(1), key(), at)
                    .1
                    .is_none()
            );
        }
        let inert = ConsentState::inert();
        assert_eq!(
            inert.fda(ProbeGate::on(), Duration::ZERO, "dr").0.label,
            ProbeLabel::RefusedDisabled
        );
        assert_eq!(inert.next_refresh_deadline(now, Duration::ZERO), None);
    }

    #[test]
    fn zero_interval_cannot_turn_completion_into_a_probe_wake_loop() {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::ZERO, key(), now);
        state.shared.complete(&request.unwrap(), grant(), now);
        for elapsed in [Duration::ZERO, PROBE_RETRY_FLOOR - Duration::from_nanos(1)] {
            let (answer, next) =
                state.read_at(ProbeGate::on(), Duration::ZERO, key(), now + elapsed);
            assert_eq!(answer.0, grant());
            assert!(next.is_none(), "completion must not immediately re-arm");
            assert_eq!(
                state.next_refresh_deadline(now + elapsed, Duration::ZERO),
                Some(now + PROBE_RETRY_FLOOR)
            );
        }
        let (expired, refresh) = state.read_at(
            ProbeGate::on(),
            Duration::ZERO,
            key(),
            now + PROBE_RETRY_FLOOR,
        );
        assert_eq!(expired.0.label, ProbeLabel::Pending);
        assert!(refresh.is_some(), "minimum freshness still expires");
    }

    #[test]
    fn failed_worker_creation_releases_its_slot_with_bounded_backoff() {
        let state = synthetic_state();
        let now = Instant::now();
        let (_, request) = state.read_at(ProbeGate::on(), Duration::ZERO, key(), now);
        let request = request.unwrap();
        state
            .shared
            .failed_request
            .store(request.epoch, Ordering::Release);
        for offset in [Duration::ZERO, PROBE_RETRY_FLOOR - Duration::from_nanos(1)] {
            let (answer, next) =
                state.read_at(ProbeGate::on(), Duration::ZERO, key(), now + offset);
            assert_eq!(answer.0.label, ProbeLabel::Pending);
            assert!(next.is_none());
            assert!(!state.shared.slot.lock().unwrap().in_flight);
        }
        assert!(
            state
                .read_at(
                    ProbeGate::on(),
                    Duration::ZERO,
                    key(),
                    now + PROBE_RETRY_FLOOR
                )
                .1
                .is_some()
        );
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
        state.set_completion_wake(Arc::new(move || {
            assert!(
                weak.upgrade().unwrap().slot.try_lock().is_ok(),
                "wake runs unlocked"
            );
            done_tx.send(()).unwrap();
        }));
        let ttl = Duration::from_secs(5);
        assert_eq!(
            state.fda(ProbeGate::on(), ttl, "dr").0.label,
            ProbeLabel::Pending
        );
        entered_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap();
        // There is no elapsed-time assertion: reaching the release below while
        // the worker waits is the nonblocking witness, even on a loaded box.
        for _ in 0..100 {
            assert_eq!(
                state.fda(ProbeGate::on(), ttl, "dr").0.label,
                ProbeLabel::Pending
            );
            state.invalidate();
        }
        assert!(state.shared.slot.lock().unwrap().in_flight);
        assert_eq!(BLOCKED_CALLS.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
        assert!(!state.shared.slot.lock().unwrap().in_flight);
        assert!(
            state.shared.slot.lock().unwrap().entry.is_none(),
            "stale grant refused"
        );
    }
}
