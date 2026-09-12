// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl privacy` — the macOS consent posture in one hop, plus the
//! instance half of the two additive `status` fields and the `await consent`
//! predicate (design §5.1/§5.2/§5.3).
//!
//! # What this module may and may not touch
//!
//! Everything the OS can be asked about consent arrives through
//! [`ConsentProbes`] — a bundle of injected function pointers with an INERT
//! arm. A headless-constructible `App` is exactly what a unit test is, and on
//! 2026-08-17 a test binary's first `WindowServer` touch made `tccd` `readdir`
//! a 1.1-million-entry `target/debug/deps` until `WindowServer` was killed
//! (`AGENTS.md` rule 5, `tools/grep_guard.sh` B9). So the pattern here is the
//! `lock_modifiers` / `user_input_recent` one, for the same reason: a windowed
//! instance gets the live arms, a headless instance and every unit test get
//! arms that answer `unknown` without a syscall.
//!
//! The consequence is stated rather than hidden: a `--headless` instance
//! ANSWERS this verb (that is why the row is `AnyScopeMeta`), but its
//! `full_disk_access=` reads `unknown` with `probe=refused_disabled` — the
//! probe was deliberately not consulted, which is a different fact from a
//! denial and is spelled differently.
//!
//! Live reads are asynchronous: a cold, invalidated or expired observation
//! reports `full_disk_access=unknown probe=pending probe_age_ms=-` (JSON age
//! `null`). One instance-owned worker refreshes it and wakes the GUI; even a
//! stalled probe cannot create additional workers. Freshness and retry spacing
//! have a 500 ms floor, including when the configured interval is zero.
//!
//! # No protected-folder literal lives here
//!
//! Every protected path comes from `aterm_containment::consent` as already
//! resolved data (`protected_roots`, `Folder::path`). This module writes none
//! of its own — the B13 rule, and the reason the guard block passes on the
//! commit that adds the feature.
//!
//! # What is deliberately NOT claimed
//!
//! Spikes S1 (grant scope) and S4 (which services a grant covers) have not
//! been run, so `SpikeEvidence::UNMEASURED` is what the join sees:
//! `fda_scope` is always `unknown`, `covers=` is empty, `prompt_possible` is
//! `yes` even while the grant is held, and no folder is ever reported
//! `covered-by-fda`. Making a stronger claim is a named field flip a reviewer
//! can see, not a sentence that drifts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    self, Attribution, ConsentKey, ConsentPosture, DrClass, FdaProbe, FdaState, Folder, FsConsent,
    PostureInputs, ProbeGate, ProbeLabel, Responsible, ResponsibleError, SpikeEvidence,
};
use aterm_control::wire::{json_ok, json_str_field, pct_encode};
use winit::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// The TCC service classes a Full Disk Access grant is claimed — by Apple, not
/// by measurement — to subsume. Until §7 S4 measures them they are reported
/// `unmeasured`, never `uncovered` — except the [`NEVER_COVERED`] class, which
/// is `uncovered` measured or not: see [`covers_split`].
const SERVICES: &[&str] = &[
    "documents",
    "desktop",
    "downloads",
    "network-volumes",
    "removable-volumes",
    "app-data",
    "file-provider-domains",
];

/// The service classes Full Disk Access does NOT reliably cover, by the
/// design's own record (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4:
/// EPERM despite FDA has been reported under `~/Library/CloudStorage` /
/// FileProvider domains). Always `uncovered`, measured or not.
const NEVER_COVERED: &[&str] = &["file-provider-domains"];

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

/// The refusal for a selector on an instance-wide verb.
const NO_SELECTOR: &str = "ERR privacy is instance-wide and takes no selector\n";

/// Internal main-thread reply only; never emitted by the public privacy verb.
/// An absent row still means the target exited, distinct from an unfinished
/// observation. A complete tuple always begins with `fs_consent=`.
const PENDING_CONSENT_TUPLE: &str = "consent-probe-pending";

// ---------------------------------------------------------------------------
// The injected probes
// ---------------------------------------------------------------------------

/// THE FENCE. Every OS question this feature asks goes through one of these
/// function pointers, injected at `App` construction exactly like
/// `App::lock_modifiers` and `App::user_input_recent`.
///
/// * [`ConsentProbes::live`] — a WINDOWED instance: the real probes.
/// * [`ConsentProbes::inert`] — a headless instance and every unit test:
///   answers without performing a syscall.
///
/// `fda` additionally passes through `consent::probe_fda`'s own in-bundle
/// guard, so even the live arm refuses from a binary outside a `.app`. The two
/// gates are deliberately redundant: this one is the GUI's, and the module's is
/// the one that also protects the bundled CLI (design §3.3 guardrails 1 and 2).
#[derive(Clone, Copy)]
pub(crate) struct ConsentProbes {
    /// One `open(TCC.db, O_RDONLY)`, fd closed at once, contents never read.
    fda: fn(ProbeGate) -> FdaProbe,
    /// `responsibility_get_pid_responsible_for_pid`, through `dlsym`. Not a
    /// TCC or `WindowServer` contact, but it is still an OS call made once per
    /// live session, so it takes the same fence.
    responsible: fn(i32) -> Result<i32, ResponsibleError>,
    /// Whether these are the live arms. Read only by the `Debug` impl and the
    /// tests: the `observer` row never carries it, because the inert arms
    /// answer with labels the row already renders as `off` (`RefusedDisabled`,
    /// `Unsupported` — see [`observer_fda_value`]) — a probe that was never
    /// consulted is not a probe that answered no.
    live: bool,
}

impl ConsentProbes {
    /// The windowed instance's arms.
    pub(crate) const fn live() -> Self {
        Self {
            fda: consent::probe_fda,
            responsible: consent::responsible_pid_detailed,
            live: true,
        }
    }

    /// The headless / unit-test arms: no syscall, and `unknown` said out loud.
    pub(crate) const fn inert() -> Self {
        Self {
            fda: inert_fda_probe,
            responsible: inert_responsible,
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

// ---------------------------------------------------------------------------
// Instance-owned consent state
// ---------------------------------------------------------------------------

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

#[derive(Default)]
struct SharedProbe {
    /// Invalidation never waits for the worker or its publication mutex.
    epoch: AtomicU64,
    /// A spawn failure can be published without waiting for the slot mutex.
    failed_request: AtomicU64,
    slot: Mutex<ProbeSlot>,
    wake: OnceLock<ProbeWake>,
}

impl SharedProbe {
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
/// invokes the live FDA probe. Reads return Unknown/Pending until a current
/// result exists; an expired grant is never served while refreshing.
///
/// There is at most one in-flight worker per instance, including across
/// activation, identity and policy invalidation. A stuck syscall therefore
/// cannot multiply threads. A successor starts with no cached observation.
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

    /// Synthetic cached verdict for host tests. Even a later refresh uses the
    /// inert function pointer, so this helper cannot contact the OS.
    #[cfg(test)]
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

// ---------------------------------------------------------------------------
// The signing identity a grant is keyed to
// ---------------------------------------------------------------------------

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

/// The running process's signing identity, read at most once.
///
/// Deliberately resolved on the CONTROL thread (`cmd_privacy` warms it before
/// the main-thread hop): it may spawn `codesign`, which is a 50–150 ms stall
/// that must never land on the event loop. It touches neither `tccd` nor
/// `WindowServer`, and it does nothing at all unless the running executable
/// resolves inside a `.app` — so a test binary never spawns anything.
pub(crate) fn signing_identity() -> &'static SigningIdentity {
    identity_cell().get_or_init(read_signing_identity)
}

/// The identity ONLY if it has already been read.
///
/// This is what the event loop uses. Reading the identity may spawn
/// `codesign`, a 50–150 ms stall that must never land on the main thread, so
/// the main-thread paths degrade to [`SigningIdentity::unknown`] until a
/// control-thread verb has warmed it — and `cmd_privacy` warms it before its
/// own hop, so the posture readout never sees the cold value.
fn signing_identity_if_warm() -> &'static SigningIdentity {
    identity_cell()
        .get()
        .unwrap_or_else(SigningIdentity::unknown)
}

fn identity_cell() -> &'static OnceLock<SigningIdentity> {
    static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
    &IDENTITY
}

/// The running bundle's `CFBundleIdentifier`, or `None` when the running
/// executable is not inside a `.app` (or its `Info.plist` may not be read).
///
/// Callers that need an identity string must treat `None` as "no identity to
/// claim" and omit the claim, never substitute the release channel's id — that
/// is the whole point of reading it (design §3.1 blast radius).
pub(crate) fn running_bundle_id() -> Option<&'static str> {
    signing_identity().bundle_id.as_deref()
}

fn read_signing_identity() -> SigningIdentity {
    let exe = running_exe();
    let Some(root) = consent::app_bundle_root(exe) else {
        return SigningIdentity::unknown().clone();
    };
    // The guard: an `Info.plist` under a protected root is exactly the read
    // this design exists to avoid, so the resolver refuses it and everything
    // below degrades to `unknown`.
    let plist = consent::readable_info_plist(exe, &consent::protected_roots(&[]));
    let text = plist.as_deref().and_then(read_bounded);
    let bundle_id = text
        .as_deref()
        .and_then(|t| plist_string(t, "CFBundleIdentifier"))
        .map(str::to_owned);
    let display_name = text
        .as_deref()
        .and_then(|t| plist_string(t, "CFBundleDisplayName").or(plist_string(t, "CFBundleName")))
        .map(str::to_owned);
    let dev_build = text.as_deref().map(plist_marks_dev_build);
    let codesign = codesign_report(&root);
    let dr_text = codesign
        .as_deref()
        .and_then(designated_requirement)
        .unwrap_or_default();
    SigningIdentity {
        bundle_id,
        display_name,
        signing: codesign.as_deref().map_or("unknown", classify_signing),
        team: codesign.as_deref().and_then(team_identifier),
        dr: consent::classify_dr(&dr_text),
        dr_text,
        dev_build,
    }
}

/// `codesign -d -r- --verbose=2` over a bundle, stdout and stderr joined
/// (`codesign -d` writes its report to stderr). `None` when it cannot run.
#[cfg(target_os = "macos")]
fn codesign_report(root: &Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .arg("-d")
        .arg("-r-")
        .arg("--verbose=2")
        .arg(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

#[cfg(not(target_os = "macos"))]
fn codesign_report(_root: &Path) -> Option<String> {
    None
}

/// The `designated => …` clause of a `codesign -d -r-` report.
fn designated_requirement(report: &str) -> Option<String> {
    report
        .lines()
        .find(|line| line.trim_start().starts_with("designated =>"))
        .map(|line| line.trim().to_owned())
}

/// The Team ID, when the signature carries a real one. Apple prints
/// `TeamIdentifier=not set` for an ad-hoc signature, which is not an id.
fn team_identifier(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        let value = line.trim().strip_prefix("TeamIdentifier=")?;
        let value = value.trim();
        (!value.is_empty() && value != "not set").then(|| value.to_owned())
    })
}

/// How the running code is signed, from the same report. Fails toward the
/// weaker claim: anything unrecognised is `unknown`, never `developer-id`.
fn classify_signing(report: &str) -> &'static str {
    if report.contains("code object is not signed at all") {
        return "unsigned";
    }
    if report.lines().any(|line| line.trim() == "Signature=adhoc") {
        return "adhoc";
    }
    if team_identifier(report).is_some() {
        return "developer-id";
    }
    "unknown"
}

/// One XML plist string value, by exact key. Only XML plists are understood —
/// the same precedent `aterm_update::which_copy` sets, and lossless for the
/// shapes that matter here.
fn plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text;
    loop {
        let at = rest.find("<key>")?;
        let after = &rest[at + "<key>".len()..];
        let end = after.find("</key>")?;
        let found = after[..end].trim();
        let tail = &after[end + "</key>".len()..];
        if found == key {
            let value = tail.trim_start().strip_prefix("<string>")?;
            let close = value.find("</string>")?;
            let value = value[..close].trim();
            return (!value.is_empty()).then_some(value);
        }
        rest = tail;
    }
}

/// Whether an `Info.plist`'s text carries the `ATermDevBuild` mark — the same
/// key and the same fail-open reading as `aterm_update`'s own predicate, which
/// is not reachable from here (its module is private to that crate). Fails
/// OPEN: anything unclear is "not a dev build", so a corrupt plist can never
/// invent a dev identity for a release.
fn plist_marks_dev_build(text: &str) -> bool {
    plist_string(text, "ATermDevBuild").is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// A bounded text read: an `Info.plist` over the cap is unreadable rather than
/// unbounded work.
fn read_bounded(path: &Path) -> Option<String> {
    const MAX_PLIST_BYTES: u64 = 1 << 20;
    let meta = std::fs::metadata(path).ok()?;
    (meta.len() <= MAX_PLIST_BYTES)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
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
            let text = read_bounded(Path::new(
                "/System/Library/CoreServices/SystemVersion.plist",
            ))?;
            plist_string(&text, "ProductVersion").map(str::to_owned)
        })
        .as_deref()
}

// ---------------------------------------------------------------------------
// The snapshot
// ---------------------------------------------------------------------------

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

/// The whole posture, assembled once and rendered two ways. Pure data: the
/// renderers below are total functions of it, so the wire format is testable
/// without an event loop, a window, or an OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrivacySnapshot {
    platform: &'static str,
    os: Option<String>,
    identity: SigningIdentity,
    /// The install posture the first-open doctor classifies
    /// (`aterm_update::which_copy::posture_from`), as a token.
    install: &'static str,
    /// The canonical path this process runs from, when it can be read.
    running: Option<String>,
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
    observer_fda: &'static str,
    observer_responsible: &'static str,
    reset_command: Option<String>,
}

/// The `covers=` / `uncovered=` / `unmeasured=` split — THREE buckets, because
/// "not measured" is not "measured as not covered" (2026-09-10). Until §7 S4
/// runs, nothing is measured, so `covers` is empty and every service except
/// the permanently-uncovered ones sits in `unmeasured`. The earlier rendering
/// put them all in `uncovered`, which told the owner — and any agent parsing
/// the JSON — that Full Disk Access covers nothing, app-data included: the
/// exact opposite of Apple's documented order of evaluation, and an argument
/// against the one switch that stops the prompts they were seeing. And when
/// the measurement flips, the bucket [`NEVER_COVERED`] stays put instead of
/// being swept into `covers` with everything else.
pub(crate) fn covers_split(evidence: SpikeEvidence) -> CoverageSplit {
    let never = |s: &&&str| NEVER_COVERED.contains(s);
    let uncovered: Vec<&'static str> = SERVICES.iter().filter(never).copied().collect();
    let rest: Vec<&'static str> = SERVICES.iter().filter(|s| !never(s)).copied().collect();
    if evidence.fda_coverage_measured {
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
    }
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

/// The `install=` token and `running=` path: the same classification the
/// first-open doctor makes (`aterm_update::which_copy::posture_from`), read
/// off this process's canonical executable path. Pure path work; no TCC.
fn install_posture_rows() -> (&'static str, Option<String>) {
    let Ok(exe) = std::env::current_exe() else {
        return ("unknown", None);
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let token = match aterm_update::which_copy::posture_from(&exe) {
        aterm_update::which_copy::InstallPosture::Installed => "installed",
        aterm_update::which_copy::InstallPosture::MountedImage => "mounted-image",
        aterm_update::which_copy::InstallPosture::Translocated => "translocated",
        aterm_update::which_copy::InstallPosture::NotABundle => "not-a-bundle",
    };
    (token, Some(exe.to_string_lossy().into_owned()))
}

/// The `observer fda=` value: `off` when the probe was deliberately not
/// consulted, `unavailable` when it was permitted but could not run, `ok` when
/// a syscall actually produced the answer. `unavailable` is a THIRD value —
/// not `off`, and not `false`.
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

/// The closing prose row. It states the two things a reader otherwise infers
/// wrongly, and it promises nothing about elimination. Its `covers is empty`
/// clause is written for the `SpikeEvidence::UNMEASURED` every `read_privacy`
/// report carries today; the measured arm of [`PrivacySnapshot::lines`] (reached
/// only from the tests) renders a `covers=` list above this same sentence, so the
/// row must turn evidence-conditional when §7 S4 lands.
const NOTE: &str = "per-folder state is unknown by construction: reading a folder is the act \
                    that raises the prompt; which services a grant covers is not measured here, \
                    so covers is empty and prompt_possible stays yes";

impl PrivacySnapshot {
    /// `prompt_possible` — the module's own rule, not a second copy of it.
    fn prompt_possible(&self) -> bool {
        ConsentPosture::join(PostureInputs {
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

    /// The line body. `OK <n>` counts exactly these rows, and every live
    /// session contributes exactly one of them — there is no truncation, so
    /// `sessions_total=` always equals the number of `session` rows.
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
        // WHERE THIS PROCESS IS RUNNING FROM (2026-09-10): a grant is keyed to
        // the bundle at a path, so an install posture the doctor would flag —
        // translocated, a mounted image — is a grant that cannot apply, and
        // the owner deserves to read that on the same report as the grant.
        out.push(format!(
            "install={} running={}",
            self.install,
            opt(self.running.as_deref())
        ));
        out.push(format!(
            "full_disk_access={} probe={} probe_age_ms={} fda_scope={}",
            self.fda.as_str(),
            self.probe.as_str(),
            self.probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            self.evidence.fda_scope.as_str(),
        ));
        let split = covers_split(self.evidence);
        out.push(format!("covers={}", list_or_dash(&split.covers)));
        out.push(format!("uncovered={}", list_or_dash(&split.uncovered)));
        out.push(format!("unmeasured={}", list_or_dash(&split.unmeasured)));
        let mut folder = String::from("folder");
        for name in folder_names() {
            folder.push(' ');
            folder.push_str(name);
            folder.push_str("=unknown");
        }
        folder.push_str(" source=none");
        out.push(folder);
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
            if self.protected.is_empty() {
                "-".to_string()
            } else {
                self.protected
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
            "observer fda={} responsible={} log=unavailable",
            self.observer_fda, self.observer_responsible,
        ));
        out.push(format!(
            "remediate fda={} files={} reset={}",
            pct_encode("settings:Privacy_AllFiles"),
            pct_encode("settings:Privacy_FilesAndFolders"),
            opt(self.reset_command.as_deref()),
        ));
        out.push(format!("note {NOTE}"));
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

/// Every name on the `folder` row: the three `$HOME` folders the consent
/// module resolves, then the two volume classes that have no path.
fn folder_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Folder::ALL.iter().map(|f| f.as_str()).collect();
    names.extend_from_slice(VOLUME_ROWS);
    names
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
/// Named `cmd_privacy_json` because `json_ok_sites_match_the_json_capable_verbs`
/// binds `JSON_CAPABLE_VERBS` to a handler of exactly this name.
fn cmd_privacy_json(snapshot: &PrivacySnapshot) -> String {
    use std::fmt::Write as _;

    let split = covers_split(snapshot.evidence);
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
        "{},\"running\":{},",
        json_str_field("install", snapshot.install),
        json_opt(snapshot.running.as_deref()),
    );
    let _ = write!(
        body,
        "{},{},\"probe_age_ms\":{},{},",
        json_str_field("full_disk_access", snapshot.fda.as_str()),
        json_str_field("probe", snapshot.probe.as_str()),
        snapshot
            .probe_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        json_str_field("fda_scope", snapshot.evidence.fda_scope.as_str()),
    );
    let _ = write!(
        body,
        "\"covers\":{},\"uncovered\":{},\"unmeasured\":{},",
        json_str_array(&split.covers),
        json_str_array(&split.uncovered),
        json_str_array(&split.unmeasured),
    );
    let _ = write!(body, "\"folders\":{{\"source\":\"none\"");
    for name in folder_names() {
        let _ = write!(body, ",{}", json_str_field(name, "unknown"));
    }
    let _ = write!(
        body,
        "}},\"prompt_possible\":{},",
        snapshot.prompt_possible()
    );
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
                .protected
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
    let _ = write!(
        body,
        "\"observers\":{{{},{},{}}},",
        json_str_field("fda", snapshot.observer_fda),
        json_str_field("responsible", snapshot.observer_responsible),
        json_str_field("log", "unavailable"),
    );
    let _ = write!(
        body,
        "\"remediate\":{{{},{},{}}},",
        json_str_field("fda", "settings:Privacy_AllFiles"),
        json_str_field("files", "settings:Privacy_FilesAndFolders"),
        format_args!("\"reset\":{}", json_opt(snapshot.reset_command.as_deref())),
    );
    let _ = write!(body, "{}}}", json_str_field("note", NOTE));
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

// ---------------------------------------------------------------------------
// The main-thread read
// ---------------------------------------------------------------------------

/// Which shape of the posture the main thread should assemble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivacyForm {
    /// The full posture, as lines.
    Lines,
    /// The full posture, as one JSON object.
    Json,
    /// One session's consent tuple, for `await consent`.
    Tuple(u64),
}

impl App {
    /// Assemble the consent posture on the main thread (`Wake::ReadPrivacy`).
    ///
    /// A pure read of `App` state plus the injected probes. Reading it raises
    /// NO dialog: the Full Disk Access probe reads state that already exists,
    /// and nothing here is inferred from anything else — in particular no
    /// folder's state is ever derived from the grant.
    pub(crate) fn read_privacy(&self, form: PrivacyForm) -> Vec<String> {
        let policy = self.consent_policy();
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
                // No production site observes a per-session EPERM yet: the
                // warm-up (§3.5) and aterm's own file work (§3.6) are the two
                // that will, and the join is table-tested on both values.
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
            return sessions
                .iter()
                .find(|row| self.session_sid_matches(target, &row.sid))
                .map(|row| vec![PrivacySnapshot::observed_tuple_line(row, probe)])
                .unwrap_or_default();
        }

        let mode = aterm_containment::mode_or_containment();
        let (install, running) = install_posture_once().clone();
        let snapshot = PrivacySnapshot {
            platform: std::env::consts::OS,
            os: os_version().map(str::to_owned),
            identity: identity.clone(),
            install,
            running,
            fda: probe.state,
            probe: probe.label,
            probe_age_ms: completed_probe_age_ms(probe.label, age),
            evidence: SpikeEvidence::UNMEASURED,
            attribution_root: policy.adoption(self.instance_attribution()),
            sessions,
            containment_mode: mode.to_string().to_ascii_lowercase(),
            seatbelt: if aterm_containment::actuator::network_sandbox_actuated(mode) {
                "applied"
            } else {
                "none"
            },
            protected: policy
                .protected
                .iter()
                .map(|p| crate::app_tabs::home_abbreviated(&p.to_string_lossy()))
                .collect(),
            warmup: self.config.privacy_warmup().as_str(),
            // The warm-up's own completion stamp (design §3.5), supplied by the
            // Security panel's change. Read WITHOUT draining: `read_privacy` is
            // `&self`, and a pass that finished but whose poke has not been
            // folded yet is reported on the next read rather than made up here.
            warmup_last_ms: self.consent_warmup.last_pass_ms(),
            observer_fda: observer_fda_value(probe.label),
            observer_responsible: observer_responsible_value(&answers),
            // The reset recipe is built from the RUNNING bundle id, never a
            // literal, so the dev channel can only ever name its own rows.
            // With no bundle id there is no recipe: `-`, not a half-written
            // command a reader could complete wrongly.
            reset_command: identity
                .bundle_id
                .as_deref()
                .map(|id| consent::tccutil_reset_command(id, Folder::Documents).join(" ")),
        };
        match form {
            PrivacyForm::Json => vec![cmd_privacy_json(&snapshot)],
            PrivacyForm::Lines | PrivacyForm::Tuple(_) => snapshot.lines(),
        }
    }

    /// The Security panel's slice of the posture (design §3.4).
    ///
    /// The SAME state `privacy` reports — the cached probe, the identity only
    /// if a control-thread verb already warmed it, and the `[privacy]` master
    /// switch — assembled for a renderer instead of for a wire. Consent probes
    /// run on the worker, executable posture is cached once, and this never
    /// spawns `codesign`; a cold identity degrades to `DrClass::Unknown`, which
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

    /// THIS instance's own adoption record: `adopted` when it took over a
    /// running root shell from a predecessor across an in-place apply, `live`
    /// otherwise. Per-session attribution is reported per session.
    fn instance_attribution(&self) -> Attribution {
        if self.bootstrap_session_adopted {
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

/// What the Security panel needs from the consent model, as pure data.
///
/// A compact projection of [`PrivacySnapshot`]: the panel needs installation
/// posture and session counts, not individual session/cwd rows or containment
/// paths. Its service coverage comes from the shared [`covers_split`].
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

// ---------------------------------------------------------------------------
// The two additive `status` fields (design §5.2)
// ---------------------------------------------------------------------------

/// What the `status` record adds for one session: two fields and one
/// `reasons=` token. Additive — `schema=1` does not move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionConsent {
    /// aterm's OWN adoption record for this session.
    pub(crate) attribution: Attribution,
    /// `covered` requires the grant AND `attribution=live` AND a measured
    /// coverage; `denied` requires an EPERM aterm itself observed for this
    /// session. Everything else is `unknown`. Because §7 S4 has not been run,
    /// `covered` is currently unreachable — and that is correct.
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
    /// `protected_roots`, already resolved to absolute paths by the consent
    /// module — this crate holds no protected-path literal of its own.
    protected: Vec<PathBuf>,
    /// `report_attribution`.
    report_attribution: bool,
    /// The master switch.
    enabled: bool,
}

impl ConsentPolicy {
    /// With the lane switched off every consent field reads `unknown` — the
    /// honest word for "aterm stopped looking", which is a different claim
    /// from `denied`. That includes the adoption record, which is otherwise
    /// reported verbatim.
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
            protected: self.config.privacy_protected_roots(),
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

    /// The consent fields for one session's `status` record.
    ///
    /// Runs on the event loop under a poll, so it never blocks: it takes the
    /// cached probe — refreshed by one `open(TCC.db)` at most once per
    /// `probe_interval_ms`, never by `codesign` — and the identity only if
    /// already warm, and `cwd` is passed in by the caller — which already holds
    /// (or failed to take) that session's terminal lock, and must not be asked
    /// to take it twice.
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
            // The responsibility SPI is NOT consulted here. `status` is a poll
            // verb, and the corroboration changes neither field it produces —
            // `attribution` is the adoption record verbatim and `fs_consent`
            // never reads it — so a per-poll syscall would buy nothing. The
            // `privacy` verb reports the corroboration, once, per session.
            responsible: Responsible::Unknown,
            // See `read_privacy`: no production site observes a per-session
            // EPERM yet, and the join is table-tested on both values.
            observed_eperm: false,
            evidence: SpikeEvidence::UNMEASURED,
        });
        SessionConsent {
            attribution: posture.attribution,
            fs_consent: posture.fs_consent,
            at_risk: consent_at_risk(posture.fs_consent, cwd, &policy.protected),
        }
    }
}

/// THE `consent_at_risk` CONJUNCTION (design §3.6): this session's
/// `fs_consent` is not `covered`, AND its shell-integration cwd is under a
/// protected root.
///
/// Two observed facts joined. It is NOT a claim that a dialog is showing —
/// aterm cannot see one — and it is simply not emitted when the cwd is
/// unknown, which is whenever shell integration is absent. Pure and lexical:
/// no path is touched to decide it.
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

/// One session's attribution, from aterm's OWN adoption record — the handoff
/// id the successor stamped on every shell it inherited (`spawn::Adopted`),
/// never from the responsibility SPI (design §3.9 pt 1).
fn session_attribution(session: &crate::Session) -> Attribution {
    if session.handoff_local_id.is_some() {
        Attribution::Adopted
    } else {
        Attribution::Live
    }
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// `privacy [--json]` — the macOS consent posture, instance-wide.
///
/// Runs on a control worker: it warms the signing identity HERE (it may spawn
/// `codesign`, which must never land on the event loop) and then takes the one
/// main-thread hop that reads `App` state and the injected probes.
pub(crate) fn cmd_privacy(rest: &str, proxy: &EventLoopProxy<Wake>) -> String {
    let form = match parse_privacy_form(rest) {
        Ok(form) => form,
        Err(usage) => return usage,
    };
    let json = form == PrivacyForm::Json;
    // Warm the process-wide identity off the event loop.
    let _ = signing_identity();
    let lines = match crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
        form,
        reply: tx,
    }) {
        Ok(lines) => lines,
        Err(e) => return format!("ERR {e}\n"),
    };
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

/// The verb's whole grammar: an optional `--json` (or bare `json`) flag and
/// nothing else. A guessed modifier is an honest `ERR usage`, never a silently
/// ignored token that leaves the caller unable to tell a wrong guess from a
/// no-op.
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

/// `await consent [timeout=<ms>]` — wait for THIS session's observed tuple to
/// change. A pending refresh is not a change. On a cold cache, the first
/// completed observation establishes the baseline; it cannot itself satisfy
/// the wait. The timeout remains anchored to the original request.
///
/// A latch says aterm's own posture CHANGED. It does NOT say a human answered
/// a dialog: aterm cannot observe the answer.
pub(crate) fn cmd_await_consent(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    let timeout_ms = match parse_consent_timeout(rest) {
        Ok(ms) => ms,
        Err(usage) => return usage,
    };
    let armed = Instant::now();
    let mut wait = ConsentWait::new(armed, Duration::from_millis(timeout_ms));
    let _ = signing_identity();
    let sample = |proxy: &EventLoopProxy<Wake>| -> Result<ConsentObservation, String> {
        match crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
            form: PrivacyForm::Tuple(session),
            reply: tx,
        }) {
            Ok(lines) => Ok(consent_observation(lines)),
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
        std::thread::sleep(
            AWAIT_CONSENT_TICK.min(wait.deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// `timeout=<ms>` / `timeout <ms>`, defaulting to [`AWAIT_CONSENT_DEFAULT_MS`]
/// and capped at [`AWAIT_MAX_MS`], the ceiling the rest of the wait family
/// shares.
fn parse_consent_timeout(rest: &str) -> Result<u64, String> {
    const USAGE: &str = "ERR usage: await consent [timeout=<ms>]\n";
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut timeout = AWAIT_CONSENT_DEFAULT_MS;
    let mut i = 0;
    // `consent` itself is the leading token the caller already matched on.
    if toks.first() == Some(&"consent") {
        i = 1;
    }
    while i < toks.len() {
        if let Some(v) = toks[i].strip_prefix("timeout=") {
            timeout = v.parse().map_err(|_| USAGE.to_string())?;
            i += 1;
        } else if toks[i] == "timeout" && i + 1 < toks.len() {
            timeout = toks[i + 1].parse().map_err(|_| USAGE.to_string())?;
            i += 2;
        } else {
            return Err(USAGE.to_string());
        }
    }
    Ok(timeout.min(AWAIT_MAX_MS))
}

#[cfg(test)]
mod tests {
    use aterm_containment::consent::{FdaScope, ProbeOutcome};

    use super::*;

    /// A snapshot with no OS in it, so the wire format is a total function of
    /// data a test can name.
    fn snapshot(sessions: usize, fda: FdaState, evidence: SpikeEvidence) -> PrivacySnapshot {
        PrivacySnapshot {
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
            observer_fda: "ok",
            observer_responsible: "ok",
            reset_command: Some(
                "/usr/bin/tccutil reset SystemPolicyDocumentsFolder com.aterm.aterm".to_string(),
            ),
        }
    }

    /// THE GATE TEST (design §3.3 guardrail 2). A headless-constructed `App` is
    /// exactly what a unit test is, and it must reach `tccd` through nothing.
    ///
    /// The assertion is on the LABEL, not merely on `unknown`, because that is
    /// what distinguishes the two arms: the live arm run from this very test
    /// binary would answer `refused_out_of_bundle` (the consent module's own
    /// in-bundle guard catching it), while the inert arm answers
    /// `refused_disabled` — the probe was never consulted at all. Seeing
    /// `refused_disabled` here is therefore positive evidence that the inert
    /// arm is the one wired, not just that nothing blew up.
    #[test]
    fn a_headless_app_wires_the_inert_consent_probe_arm() {
        let app = crate::App::headless_for_test();
        let lines = app.read_privacy(PrivacyForm::Lines);
        let fda = lines
            .iter()
            .find(|l| l.starts_with("full_disk_access="))
            .expect("the posture carries a full_disk_access row");
        assert!(
            fda.contains("full_disk_access=unknown"),
            "a headless instance knows nothing about the grant: {fda}"
        );
        assert!(
            fda.contains("probe=refused_disabled"),
            "the INERT arm must be the wired one: {fda}"
        );
        let observer = lines
            .iter()
            .find(|l| l.starts_with("observer "))
            .expect("the posture carries an observer row");
        assert!(
            observer.contains("fda=off"),
            "a probe that was deliberately not consulted is `off`: {observer}"
        );
        assert!(
            observer.contains("responsible=off"),
            "so is the responsibility SPI: {observer}"
        );
    }

    /// The line body's exact shape, and the `OK <n>` arithmetic behind it: the
    /// count is the number of rows, and every live session contributes exactly
    /// one `session` row — never truncated, so `sessions_total=` and the row
    /// count can never disagree.
    #[test]
    fn the_line_body_is_the_documented_shape_and_ok_n_counts_it() {
        let lines = snapshot(4, FdaState::Denied, SpikeEvidence::UNMEASURED).lines();
        // 16 fixed rows (2026-09-10: `install=` and `unmeasured=` joined the 14).
        assert_eq!(lines.len(), 4 + 16, "16 fixed rows plus one per session");
        assert_eq!(lines[0], "schema=1");
        assert_eq!(lines[1], "platform=macos os=26.6.2");
        assert_eq!(
            lines[2],
            "bundle_id=com.aterm.aterm display_name=aterm signing=developer-id team=A66A9P66Z7 \
             dr=identity grant_stable=yes dev_build=false"
        );
        assert_eq!(
            lines[3],
            "install=installed running=/Applications/aterm.app/Contents/MacOS/aterm"
        );
        assert_eq!(
            lines[4],
            "full_disk_access=denied probe=open_eperm probe_age_ms=1840 fda_scope=unknown"
        );
        assert_eq!(lines[5], "covers=-");
        assert_eq!(lines[6], "uncovered=file-provider-domains");
        assert!(
            lines[7].starts_with("unmeasured=documents,"),
            "{}",
            lines[7]
        );
        assert_eq!(
            lines[10],
            "attribution_root=live sessions_total=4 sessions_adopted=1"
        );
        let session_rows: Vec<&String> =
            lines.iter().filter(|l| l.starts_with("session ")).collect();
        assert_eq!(session_rows.len(), 4);
        assert_eq!(
            session_rows[0].as_str(),
            "session sid=s-0 attribution=live responsible_pid=4711 responsible=self \
             fs_consent=unknown cwd=/Users//a%20b/src",
            "free text takes the wire's OWN pct encoder (the one `status`'s \
             `subject=` uses), which escapes the space that would split the \
             field and leaves ascii-graphic bytes such as `/` alone"
        );
        assert!(session_rows[1].as_str().contains("attribution=adopted"));
        assert!(lines.last().expect("a note row").starts_with("note "));
        // The header the handler writes is the row count.
        assert_eq!(format!("OK {}", lines.len()), "OK 20");
    }

    /// A big instance is still exact: no cap, no ellipsis, no "…and N more".
    #[test]
    fn every_live_session_gets_one_row_with_no_truncation() {
        for count in [0, 1, 40] {
            let lines = snapshot(count, FdaState::Unknown, SpikeEvidence::UNMEASURED).lines();
            let rows = lines.iter().filter(|l| l.starts_with("session ")).count();
            assert_eq!(rows, count);
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&format!("sessions_total={count}"))),
                "sessions_total must equal the row count for {count}"
            );
        }
    }

    /// THE HONESTY POSTURE, as a table. Under today's evidence — spikes S1 and
    /// S4 unrun — holding the grant changes NOTHING about what is claimed:
    /// `covers` stays empty, every service but the never-covered class is
    /// `unmeasured`, `fda_scope` stays
    /// `unknown`, and `prompt_possible` stays `yes`. The only thing that moves
    /// any of it is a NAMED `SpikeEvidence` field, which a reviewer sees.
    #[test]
    fn the_grant_alone_never_buys_a_coverage_claim() {
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
            assert!(lines.contains(&"covers=-".to_string()), "{fda:?}");
            // Unmeasured is NOT uncovered: only the permanently-uncovered class
            // is called uncovered before the measurement has run.
            assert!(
                lines.contains(&"uncovered=file-provider-domains".to_string()),
                "{fda:?}: {lines:?}"
            );
            assert!(
                lines.contains(&format!(
                    "unmeasured={}",
                    SERVICES
                        .iter()
                        .filter(|s| !NEVER_COVERED.contains(s))
                        .copied()
                        .collect::<Vec<_>>()
                        .join(",")
                )),
                "{fda:?}"
            );
            assert!(
                lines.contains(&"prompt_possible=yes".to_string()),
                "{fda:?}"
            );
            assert!(
                lines.iter().any(|l| l.contains("fda_scope=unknown")),
                "{fda:?}"
            );
        }
        // And the flip is exactly one named field.
        let measured = SpikeEvidence {
            fda_coverage_measured: true,
            handoff_attribution_measured: false,
            fda_scope: FdaScope::ThisProcess,
        };
        let lines = snapshot(1, FdaState::Granted, measured).lines();
        // Measured: everything moves to `covers` EXCEPT the class the design
        // records as not reliably covered, which stays `uncovered`.
        assert!(lines.contains(&format!(
            "covers={}",
            SERVICES
                .iter()
                .filter(|s| !NEVER_COVERED.contains(s))
                .copied()
                .collect::<Vec<_>>()
                .join(",")
        )));
        assert!(lines.contains(&"uncovered=file-provider-domains".to_string()));
        assert!(lines.contains(&"unmeasured=-".to_string()));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("install=installed running=/Applications/")),
            "the install posture rides on the same report: {lines:?}"
        );
        assert!(lines.contains(&"prompt_possible=no".to_string()));
        assert!(lines.iter().any(|l| l.contains("fda_scope=this_process")));
    }

    /// A folder's state is NEVER inferred from the grant. Testing a folder is
    /// the act that raises the prompt, so the only honest answer before an
    /// access was observed is `unknown` — whatever Full Disk Access says.
    #[test]
    fn no_folder_state_is_ever_inferred_from_the_grant() {
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
            let folder = lines
                .iter()
                .find(|l| l.starts_with("folder "))
                .expect("a folder row");
            for name in folder_names() {
                assert!(folder.contains(&format!("{name}=unknown")), "{folder}");
            }
            assert!(folder.ends_with(" source=none"), "{folder}");
        }
    }

    /// `unavailable` is a THIRD value: not `off`, and not `false`. `off` is a
    /// probe deliberately not consulted; `unavailable` is one that could not
    /// answer. Collapsing them would report a configuration choice as a
    /// missing capability, or worse, as a denial.
    #[test]
    fn unavailable_is_distinct_from_off_and_from_a_negative_answer() {
        assert_eq!(observer_fda_value(ProbeLabel::RefusedDisabled), "off");
        assert_eq!(
            observer_fda_value(ProbeLabel::RefusedOutOfBundle),
            "unavailable"
        );
        assert_eq!(observer_fda_value(ProbeLabel::RefusedNoHome), "unavailable");
        assert_eq!(
            observer_fda_value(ProbeLabel::UnsupportedPlatform),
            "unavailable"
        );
        // A probe that RAN is `ok` whatever it answered — including the denial.
        assert_eq!(observer_fda_value(ProbeLabel::OpenOk), "ok");
        assert_eq!(observer_fda_value(ProbeLabel::OpenEperm), "ok");
        assert_eq!(observer_fda_value(ProbeLabel::OpenErrno(13)), "ok");

        assert_eq!(
            observer_responsible_value(&[Err(ResponsibleError::Unsupported)]),
            "off"
        );
        assert_eq!(
            observer_responsible_value(&[Err(ResponsibleError::SymbolUnavailable)]),
            "unavailable"
        );
        // A refusal is an ANSWER: the SPI ran and said "not yours".
        assert_eq!(
            observer_responsible_value(&[Err(ResponsibleError::NotOurs), Ok(1)]),
            "ok"
        );
        assert_eq!(observer_responsible_value(&[]), "off");
    }

    /// The `consent_at_risk` conjunction (§3.6), and the case it must stay
    /// silent on: an unknown cwd, which is every session without shell
    /// integration. Silence there is the point — a token emitted on missing
    /// evidence would be a guess.
    #[test]
    fn consent_at_risk_is_a_conjunction_and_is_silent_without_a_cwd() {
        let roots = vec![PathBuf::from("/u/me/Protected"), PathBuf::from("/vol")];
        let inside = "/u/me/Protected/work";
        let outside = "/u/me/src/aterm";

        assert!(consent_at_risk(FsConsent::Unknown, Some(inside), &roots));
        assert!(consent_at_risk(FsConsent::Denied, Some(inside), &roots));
        // `covered` is the one verdict that clears it.
        assert!(!consent_at_risk(FsConsent::Covered, Some(inside), &roots));
        // …and so does a cwd that is not under a protected root.
        assert!(!consent_at_risk(FsConsent::Unknown, Some(outside), &roots));
        // NEVER on an unknown cwd, for any verdict.
        for verdict in [FsConsent::Unknown, FsConsent::Denied, FsConsent::Covered] {
            assert!(!consent_at_risk(verdict, None, &roots));
        }
        // The root itself counts, and a sibling that merely shares a prefix
        // does not.
        assert!(consent_at_risk(
            FsConsent::Unknown,
            Some("/u/me/Protected"),
            &roots
        ));
        assert!(!consent_at_risk(
            FsConsent::Unknown,
            Some("/u/me/Protected-elsewhere"),
            &roots
        ));
    }

    /// An ADOPTED session reports `unknown` and NEVER `denied` (design §3.9
    /// pt 3), and its `attribution` comes from aterm's own handoff record — not
    /// from the responsibility SPI, which is not consulted on this path at all.
    #[test]
    fn an_adopted_session_is_attributed_from_the_handoff_record_and_never_denied() {
        let mut app = crate::App::headless_for_test();
        let fresh = app.session_consent(0, None);
        assert_eq!(fresh.attribution, Attribution::Live);
        assert_eq!(fresh.fs_consent, FsConsent::Unknown);

        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .handoff_local_id = Some(7);
        let adopted = app.session_consent(0, None);
        assert_eq!(
            adopted.attribution,
            Attribution::Adopted,
            "the adoption record is the authority"
        );
        assert_eq!(
            adopted.fs_consent,
            FsConsent::Unknown,
            "never `denied` before S2"
        );

        // A session that is not in the pool answers `unknown`, not a guess.
        let gone = app.session_consent(4242, None);
        assert_eq!(gone.attribution, Attribution::Unknown);
        assert_eq!(gone.fs_consent, FsConsent::Unknown);
    }

    /// A successor inherits NOTHING: the consent state is instance-owned, so a
    /// freshly constructed `App` starts with an empty probe cache whatever the
    /// predecessor had learned.
    #[test]
    fn a_fresh_instance_inherits_no_probe_cache() {
        let state = ConsentState::inert();
        let (probe, _) = state.fda(ProbeGate::on(), Duration::from_millis(5_000), "dr");
        assert_eq!(probe.state, FdaState::Unknown);
        let successor = ConsentState::inert();
        assert!(
            format!("{successor:?}").contains("cache_empty: true"),
            "a successor's cache starts empty"
        );
    }

    /// `[privacy] enabled = false` makes every consent field read `unknown` —
    /// the honest word for "aterm stopped looking". It must NOT read like a
    /// denial, and it must not leave the adoption record leaking through: the
    /// master switch turns the whole lane off, not just the probe.
    #[test]
    fn the_master_switch_makes_every_consent_field_unknown() {
        let mut app = crate::App::headless_for_test();
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

        let lines = app.read_privacy(PrivacyForm::Lines);
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

    /// The `[privacy]` resolvers are the ones consulted — not a constant in
    /// this file. `warmup=` echoes the configured mode, and
    /// `report_attribution = false` removes the corroboration COLUMN (the
    /// observer row says `off`) without touching any verdict.
    #[test]
    fn the_report_reads_the_privacy_config_section() {
        let mut app = crate::App::headless_for_test();
        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            warmup: Some("never".to_string()),
            report_attribution: Some(false),
            ..Default::default()
        });
        let lines = app.read_privacy(PrivacyForm::Lines);
        assert!(
            lines.iter().any(|l| l.starts_with("warmup=never ")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("observer ") && l.contains("responsible=off")),
            "{lines:?}"
        );
        // The verdict is untouched: attribution still comes from the adoption
        // record, which the SPI never decided.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("session ") && l.contains("attribution=live")),
            "{lines:?}"
        );
    }

    /// The `--json` form carries the four sub-objects the contract names, is
    /// framed as a single body line, and escapes rather than pct-encodes.
    #[test]
    fn the_json_form_carries_the_documented_sub_objects() {
        let body = cmd_privacy_json(&snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED));
        assert!(body.starts_with("OK 1\n"), "{body}");
        assert_eq!(body.lines().count(), 2, "a JSON reply is one body line");
        let json = body.lines().nth(1).expect("a body line");
        for key in [
            "\"folders\":{",
            "\"sessions\":[",
            "\"observers\":{",
            "\"remediate\":{",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        assert!(json.contains("\"cwd\":\"/Users//a b/src\""), "{json}");
        assert!(json.contains("\"sessions_total\":2"), "{json}");
        assert!(json.contains("\"prompt_possible\":true"), "{json}");
        assert!(json.contains("\"covers\":[]"), "{json}");
        assert!(
            json.contains("\"uncovered\":[\"file-provider-domains\"]"),
            "unmeasured is not uncovered: {json}"
        );
        assert!(json.contains("\"unmeasured\":[\"documents\","), "{json}");
        assert!(json.contains("\"install\":\"installed\""), "{json}");
        assert!(
            json.contains("\"running\":\"/Applications/aterm.app/Contents/MacOS/aterm\""),
            "{json}"
        );
        let opens = json.chars().filter(|c| *c == '{').count();
        let closes = json.chars().filter(|c| *c == '}').count();
        assert_eq!(opens, closes, "balanced object braces: {json}");
        assert_eq!(
            json.chars().filter(|c| *c == '[').count(),
            json.chars().filter(|c| *c == ']').count(),
            "balanced array brackets: {json}"
        );
    }

    /// The verb's grammar: the flag in both spellings, and an honest usage
    /// error for anything else — a caller must be able to tell a wrong guess
    /// from a no-op.
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
    /// behind never expires), capped with the rest of the wait family, and
    /// accepting both spellings the family uses.
    #[test]
    fn await_consent_has_a_finite_default_and_a_shared_ceiling() {
        assert_eq!(parse_consent_timeout("consent"), Ok(300_000));
        assert_eq!(parse_consent_timeout(""), Ok(300_000));
        assert_eq!(parse_consent_timeout("consent timeout=1500"), Ok(1_500));
        assert_eq!(parse_consent_timeout("consent timeout 1500"), Ok(1_500));
        assert_eq!(
            parse_consent_timeout("consent timeout=99999999"),
            Ok(600_000),
            "capped with `await`/`ready`"
        );
        assert!(parse_consent_timeout("consent idle 10").is_err());
        assert!(parse_consent_timeout("consent timeout=soon").is_err());
    }

    /// The tuple `await consent` arms on is the three values the contract
    /// names, and a change in ANY of them is a change.
    #[test]
    fn the_await_baseline_is_the_whole_consent_tuple() {
        let row = SessionRow {
            sid: "s-1".to_string(),
            attribution: Attribution::Live,
            responsible_pid: None,
            responsible: Responsible::Unknown,
            fs_consent: FsConsent::Unknown,
            cwd: None,
        };
        let baseline = PrivacySnapshot::tuple_line(&row, FdaState::Denied);
        assert_eq!(
            baseline, "fs_consent=unknown fda=denied attribution=live",
            "the tuple is (fs_consent, fda, attribution)"
        );
        // Each component alone moves it.
        assert_ne!(
            PrivacySnapshot::tuple_line(&row, FdaState::Granted),
            baseline,
            "the instance grant"
        );
        let mut adopted = row.clone();
        adopted.attribution = Attribution::Adopted;
        assert_ne!(
            PrivacySnapshot::tuple_line(&adopted, FdaState::Denied),
            baseline,
            "this session's attribution"
        );
        let mut denied = row;
        denied.fs_consent = FsConsent::Denied;
        assert_ne!(
            PrivacySnapshot::tuple_line(&denied, FdaState::Denied),
            baseline,
            "this session's fs_consent"
        );
    }

    /// The signing readers, over recorded `codesign -d -r- --verbose=2`
    /// output. They fail toward the WEAKER claim: anything unrecognised is
    /// `unknown`, never `developer-id`.
    #[test]
    fn the_signing_readers_fail_toward_the_weaker_claim() {
        let devid = "Executable=/Applications/aterm.app/Contents/MacOS/aterm\n\
                     Identifier=com.aterm.aterm\n\
                     TeamIdentifier=A66A9P66Z7\n\
                     designated => identifier \"com.aterm.aterm\" and anchor apple generic\n";
        assert_eq!(classify_signing(devid), "developer-id");
        assert_eq!(team_identifier(devid).as_deref(), Some("A66A9P66Z7"));
        assert_eq!(
            consent::classify_dr(&designated_requirement(devid).unwrap()),
            DrClass::Identity
        );

        let adhoc = "Identifier=com.aterm.aterm.dev\n\
                     Signature=adhoc\n\
                     TeamIdentifier=not set\n\
                     designated => cdhash H\"abc\"\n";
        assert_eq!(classify_signing(adhoc), "adhoc");
        assert_eq!(team_identifier(adhoc), None, "`not set` is not a team id");
        assert_eq!(
            consent::classify_dr(&designated_requirement(adhoc).unwrap()),
            DrClass::Cdhash,
            "a cdhash pin does not survive a rebuild"
        );

        let unsigned = "/x: code object is not signed at all\n";
        assert_eq!(classify_signing(unsigned), "unsigned");
        assert_eq!(designated_requirement(unsigned), None);

        assert_eq!(classify_signing("something else entirely\n"), "unknown");
    }

    /// The plist reader: XML only, exact key, and a binary plist reads as
    /// absent rather than as a wrong answer.
    #[test]
    fn the_plist_reader_is_exact_and_xml_only() {
        let xml = "<plist><dict>\
                   <key>CFBundleIdentifier</key><string>com.aterm.aterm.dev</string>\
                   <key>CFBundleName</key><string>aterm (dev)</string>\
                   <key>ATermDevBuild</key><string>TRUE</string>\
                   </dict></plist>";
        assert_eq!(
            plist_string(xml, "CFBundleIdentifier"),
            Some("com.aterm.aterm.dev")
        );
        assert_eq!(plist_string(xml, "CFBundleName"), Some("aterm (dev)"));
        assert_eq!(plist_string(xml, "CFBundleDisplayName"), None);
        assert!(plist_marks_dev_build(xml), "the mark is case-insensitive");
        // A key whose value is not the next element must not bind to a later
        // string.
        assert_eq!(
            plist_string("<key>A</key><key>B</key><string>v</string>", "A"),
            None
        );
        // Fails OPEN: an unreadable plist is never a dev build.
        assert!(!plist_marks_dev_build("bplist00\u{0}\u{1}"));
        assert!(!plist_marks_dev_build(
            "<key>ATermDevBuild</key><string>false</string>"
        ));
    }

    /// `Responsible` on the wire: the token for the named states, the decimal
    /// pid for `Other`, and never a blank.
    #[test]
    fn the_responsible_token_renders_every_state() {
        assert_eq!(responsible_token(Responsible::SelfProcess), "self");
        assert_eq!(responsible_token(Responsible::Exited), "exited");
        assert_eq!(responsible_token(Responsible::Unknown), "unknown");
        assert_eq!(responsible_token(Responsible::Other(4711)), "4711");
        // The classifier this renders can never turn an error into "self".
        for err in [
            ResponsibleError::SymbolUnavailable,
            ResponsibleError::NotOurs,
            ResponsibleError::Errno(9),
            ResponsibleError::Unsupported,
        ] {
            assert_ne!(
                consent::classify_responsible(99, Err(err)),
                Responsible::SelfProcess
            );
        }
    }

    /// The inert arms are inert: they answer without asking the OS, and their
    /// answers are the ones the renderers key on.
    #[test]
    fn the_inert_arms_answer_without_a_syscall() {
        let probes = ConsentProbes::inert();
        let probe = (probes.fda)(ProbeGate::on());
        assert_eq!(probe.state, FdaState::Unknown);
        assert_eq!(probe.label, ProbeLabel::RefusedDisabled);
        assert!(probe.label.refused(), "the inert arm refuses the probe");
        assert_eq!(
            (probes.responsible)(1),
            Err(ResponsibleError::Unsupported),
            "the SPI is not consulted either"
        );
        // `for_instance` picks the arm from the one bit that decides it.
        assert!(!ConsentProbes::for_instance(true).live);
        assert_eq!(
            ConsentProbes::for_instance(false).live,
            cfg!(target_os = "macos")
        );
        // Sanity on the module's own classifier, so this file's `ok` label is
        // pinned to a real probe outcome and not to a guess.
        assert_eq!(
            consent::classify_probe(ProbeOutcome::Ok).label,
            ProbeLabel::OpenOk
        );
    }

    /// A selector is refused rather than silently reinterpreted: this verb is
    /// instance-wide, so `@<sid> privacy` would read as a per-session claim it
    /// does not make.
    #[test]
    fn a_selector_is_refused_with_a_reason() {
        let err = no_selector_error();
        assert!(err.starts_with("ERR "), "{err}");
        assert!(err.ends_with('\n'), "{err}");
        assert!(err.contains("instance-wide"), "{err}");
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
                // Historical invalidation cleared the deadline. Projecting
                // that implementation makes the model fail before another
                // worker could even be admitted.
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
        assert_eq!(ProbeLabel::Pending.as_str(), "pending");
        assert!(!ProbeLabel::Pending.refused());
        assert_eq!(observer_fda_value(ProbeLabel::Pending), "pending");
        assert_eq!(
            completed_probe_age_ms(ProbeLabel::Pending, Duration::ZERO),
            None
        );
        assert_eq!(
            completed_probe_age_ms(ProbeLabel::OpenOk, Duration::ZERO),
            Some(0)
        );
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
            .recv_timeout(Duration::from_secs(10))
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
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
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
        done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!state.shared.slot.lock().unwrap().in_flight);
        assert!(
            state.shared.slot.lock().unwrap().entry.is_none(),
            "stale grant refused"
        );
    }
}
