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
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_containment::consent::{
    self, Attribution, ConsentCache, ConsentKey, ConsentPosture, DrClass, FdaProbe, FdaState,
    Folder, FsConsent, PostureInputs, ProbeGate, ProbeLabel, Responsible, ResponsibleError,
    SpikeEvidence,
};
use aterm_control::wire::{json_ok, json_str_field, pct_encode};
use winit::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// The TCC service classes a Full Disk Access grant is claimed — by Apple, not
/// by measurement — to subsume. Every one of them is reported `uncovered`
/// until §7 S4 measures it; see [`covers_split`].
const SERVICES: &[&str] = &[
    "documents",
    "desktop",
    "downloads",
    "network-volumes",
    "removable-volumes",
    "app-data",
    "file-provider-domains",
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

/// The refusal for a selector on an instance-wide verb.
const NO_SELECTOR: &str = "ERR privacy is instance-wide and takes no selector\n";

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
    /// Whether these are the live arms. Reported as the `observer` row's third
    /// value: a probe that was never consulted is not a probe that answered no.
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

    /// `live()` for a windowed instance, `inert()` for a headless one — the
    /// same shape the `lock_modifiers` / `user_input_recent` injections use.
    pub(crate) const fn for_instance(headless: bool) -> Self {
        if headless {
            Self::inert()
        } else {
            Self::live()
        }
    }
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

/// The instance's consent state: its injected probes and its probe cache.
///
/// There is deliberately NO process-global here. A successor started by an
/// in-place apply constructs a fresh `App`, so it inherits an EMPTY cache and
/// re-probes on first demand — a stale `granted` cannot survive an apply that
/// changed the signing identity (design §3.3).
pub(crate) struct ConsentState {
    probes: ConsentProbes,
    cache: ConsentCache,
}

impl ConsentState {
    /// Wire the arms for this instance.
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            probes: ConsentProbes::for_instance(headless),
            cache: ConsentCache::new(),
        }
    }

    /// The headless / unit-test instance.
    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// The cached Full Disk Access probe, taking a fresh one at most once per
    /// `interval`. `(probe, age)`.
    ///
    /// `dr` is the designated requirement the grant is bound to; it is part of
    /// the key because a rebuild that changes the identity must invalidate a
    /// cached `granted`. A caller that must not block — the `status` poll,
    /// which runs on the event loop and may not spawn `codesign` — passes the
    /// empty string until the identity is warm, which costs at most one extra
    /// `open()` per process and never a stale answer.
    ///
    /// `gate` is `[privacy] enabled`/`check` as the consent module's OWN gate
    /// type, so the switched-off case is refused inside the module, before any
    /// syscall, and comes back labelled `refused_disabled` rather than as a
    /// denial. `interval` is `[privacy] probe_interval_ms`.
    fn fda(&self, gate: ProbeGate, interval: Duration, dr: &str) -> (FdaProbe, Duration) {
        let key = ConsentKey::new(cache_bundle().clone(), dr.to_owned());
        let probes = self.probes;
        let cached = self
            .cache
            .get_or_probe(&key, interval, || (probes.fda)(gate));
        (cached.probe, cached.age)
    }

    /// The responsibility SPI's answer for one pid, unclassified so the
    /// `observer` row can tell an absent symbol from a refusal.
    fn responsible_answer(&self, pid: i32) -> Result<i32, ResponsibleError> {
        (self.probes.responsible)(pid)
    }
}

impl Default for ConsentState {
    /// Default-safe: the inert arms. A construction that forgets to choose
    /// gets the arms that cannot reach the OS.
    fn default() -> Self {
        Self::inert()
    }
}

impl std::fmt::Debug for ConsentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentState")
            .field("probes_live", &self.probes.live)
            .field("cache_empty", &self.cache.is_empty())
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

/// The `covers=` / `uncovered=` split. Under today's evidence NOTHING is
/// measured as covered, so `covers` is empty and every service is listed as
/// uncovered — the honest rendering of "a grant removes this class of
/// interruption for the folders it covers, and which those are is not measured
/// here".
fn covers_split(evidence: SpikeEvidence) -> (Vec<&'static str>, Vec<&'static str>) {
    if evidence.fda_coverage_measured {
        (SERVICES.to_vec(), Vec::new())
    } else {
        (Vec::new(), SERVICES.to_vec())
    }
}

/// The `observer fda=` value: `off` when the probe was deliberately not
/// consulted, `unavailable` when it was permitted but could not run, `ok` when
/// a syscall actually produced the answer. `unavailable` is a THIRD value —
/// not `off`, and not `false`.
fn observer_fda_value(probe: ProbeLabel) -> &'static str {
    match probe {
        ProbeLabel::RefusedDisabled => "off",
        label if label.refused() => "unavailable",
        _ => "ok",
    }
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
/// wrongly, and it promises nothing about elimination.
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
        out.push(format!(
            "full_disk_access={} probe={} probe_age_ms={} fda_scope={}",
            self.fda.as_str(),
            self.probe.as_str(),
            self.probe_age_ms
                .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
            self.evidence.fda_scope.as_str(),
        ));
        let (covers, uncovered) = covers_split(self.evidence);
        out.push(format!("covers={}", list_or_dash(&covers)));
        out.push(format!("uncovered={}", list_or_dash(&uncovered)));
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

    let (covers, uncovered) = covers_split(snapshot.evidence);
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
        "\"covers\":{},\"uncovered\":{},",
        json_str_array(&covers),
        json_str_array(&uncovered),
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
                .map(|row| vec![PrivacySnapshot::tuple_line(row, probe.state)])
                .unwrap_or_default();
        }

        let mode = aterm_containment::mode_or_containment();
        let snapshot = PrivacySnapshot {
            platform: std::env::consts::OS,
            os: os_version().map(str::to_owned),
            identity: identity.clone(),
            fda: probe.state,
            probe: probe.label,
            probe_age_ms: (!probe.label.refused()).then_some(age.as_millis()),
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
    /// switch — assembled for a renderer instead of for a wire. It performs no
    /// syscall of its own and never spawns `codesign`, so it is legal on the
    /// event loop; a cold identity degrades to `DrClass::Unknown`, which
    /// suppresses no repair and promises no durability.
    pub(crate) fn consent_panel_facts(&self) -> ConsentPanelFacts {
        let policy = self.consent_policy();
        let identity = signing_identity_if_warm();
        let (probe, _age) = self
            .consent
            .fda(policy.gate, policy.interval, &identity.dr_text);
        ConsentPanelFacts {
            enabled: policy.enabled,
            fda: probe.state,
            probe: probe.label,
            dr: identity.dr,
            bundle_id: identity.bundle_id.clone(),
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
/// Deliberately NOT the whole [`PrivacySnapshot`]: the panel renders no session
/// rows, no containment mode and no `covers=` list, and handing it those would
/// invite a claim it is not entitled to make.
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

    /// The consent fields for one session's `status` record.
    ///
    /// Runs on the event loop under a poll, so it never blocks: it takes the
    /// CACHED probe and the identity only if already warm, and `cwd` is passed
    /// in by the caller — which already holds (or failed to take) that
    /// session's terminal lock, and must not be asked to take it twice.
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

/// `await consent [timeout=<ms>]` — park until THIS session's consent tuple
/// changes.
///
/// The predicate ARMS at the moment the request is received, taking that tuple
/// as its baseline, and latches on the first observed change from THAT
/// baseline. It never latches on a value that was already true when the call
/// was made, and never compares against a baseline captured earlier — the
/// `needs_arm_eval` edge-trigger bug class, by name.
///
/// A latch says aterm's own posture CHANGED. It does NOT say a human answered
/// a dialog: aterm cannot observe the answer.
pub(crate) fn cmd_await_consent(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    let timeout_ms = match parse_consent_timeout(rest) {
        Ok(ms) => ms,
        Err(usage) => return usage,
    };
    let _ = signing_identity();
    let sample = |proxy: &EventLoopProxy<Wake>| -> Result<Option<String>, String> {
        match crate::control::control_media::call_main(proxy, |tx| Wake::ReadPrivacy {
            form: PrivacyForm::Tuple(session),
            reply: tx,
        }) {
            Ok(lines) => Ok(lines.into_iter().next()),
            Err(e) => Err(format!("ERR {e}\n")),
        }
    };
    // ARM: the baseline is taken now, from this request.
    let baseline = match sample(proxy) {
        Ok(Some(line)) => line,
        Ok(None) => return "ERR exited\n".to_string(),
        Err(e) => return e,
    };
    let armed = Instant::now();
    let deadline = armed + Duration::from_millis(timeout_ms);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return "OK timeout\n".to_string();
        }
        std::thread::sleep(AWAIT_CONSENT_TICK.min(deadline.saturating_duration_since(now)));
        match sample(proxy) {
            Ok(Some(line)) if line != baseline => {
                return format!(
                    "OK consent {line} elapsed_ms={}\n",
                    armed.elapsed().as_millis()
                );
            }
            Ok(Some(_)) => {}
            Ok(None) => return "ERR exited\n".to_string(),
            Err(e) => return e,
        }
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
        assert_eq!(lines.len(), 4 + 14, "14 fixed rows plus one per session");
        assert_eq!(lines[0], "schema=1");
        assert_eq!(lines[1], "platform=macos os=26.6.2");
        assert_eq!(
            lines[2],
            "bundle_id=com.aterm.aterm display_name=aterm signing=developer-id team=A66A9P66Z7 \
             dr=identity grant_stable=yes dev_build=false"
        );
        assert_eq!(
            lines[3],
            "full_disk_access=denied probe=open_eperm probe_age_ms=1840 fda_scope=unknown"
        );
        assert_eq!(
            lines[8],
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
        assert_eq!(format!("OK {}", lines.len()), "OK 18");
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
    /// `covers` stays empty, every service is `uncovered`, `fda_scope` stays
    /// `unknown`, and `prompt_possible` stays `yes`. The only thing that moves
    /// any of it is a NAMED `SpikeEvidence` field, which a reviewer sees.
    #[test]
    fn the_grant_alone_never_buys_a_coverage_claim() {
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
            assert!(lines.contains(&"covers=-".to_string()), "{fda:?}");
            assert!(
                lines.contains(&format!("uncovered={}", SERVICES.join(","))),
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
        assert!(lines.contains(&format!("covers={}", SERVICES.join(","))));
        assert!(lines.contains(&"uncovered=-".to_string()));
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

        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let off = app.session_consent(0, None);
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
        assert!(probe.label.refused(), "`refused` means no syscall ran");
        assert_eq!(
            (probes.responsible)(1),
            Err(ResponsibleError::Unsupported),
            "the SPI is not consulted either"
        );
        // `for_instance` picks the arm from the one bit that decides it.
        assert!(!ConsentProbes::for_instance(true).live);
        assert!(ConsentProbes::for_instance(false).live);
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
