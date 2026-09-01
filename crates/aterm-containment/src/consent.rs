// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS privacy-consent (TCC) POSTURE — observation only, never actuation.
//!
//! This module is the foundation of the consent tier described in
//! `docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.3. It answers three
//! questions about the machine, and joins the answers into one posture that a
//! human or an agent can read:
//!
//! 1. does the responsible app hold **Full Disk Access** ([`FdaState`])?
//! 2. which process is **responsible** for a given pid ([`responsible_pid`],
//!    [`responsible_app`])?
//! 3. is a session's file access **live** or **adopted** across an in-place
//!    apply ([`Attribution`])?
//!
//! ## What this module deliberately does NOT do
//!
//! It never answers a consent dialog, never asks macOS for consent, never
//! writes to `TCC.db`, and never touches the private write-side TCC SPI. The
//! only mutation it can describe is the §3.7 denied-state repair
//! ([`ResetPlan`], [`tccutil_reset_command`]), and that is a pure command
//! *builder* — it spawns nothing, and the argv it builds names the running
//! bundle and no other. macOS asks; the human consents.
//!
//! It also does not promise that a Full Disk Access grant removes every macOS
//! consent interruption. Full Disk Access removes **this class of
//! interruption for the folders it covers**, and how much it covers has not
//! been measured on this tree (design §7 S4). Until it is, every per-folder
//! row is `unknown` **by construction** — the only way to learn a folder's
//! state is to attempt an access, which is the interruption we are trying to
//! avoid — and [`FdaScope`] stays [`FdaScope::Unknown`]. See [`SpikeEvidence`].
//!
//! ## Purity, and the three thin OS wrappers
//!
//! Everything here is a pure function over data except four thin wrappers:
//!
//! * [`probe_fda`] — ONE `open(…, O_RDONLY)` of the user's `TCC.db`, whose fd
//!   is closed immediately and whose contents are never read. This is
//!   prompt-free: `kTCCServiceSystemPolicyAllFiles` is structurally
//!   non-promptable (tccd logs `Service kTCCServiceSystemPolicyAllFiles does
//!   not allow prompting; recording denied.`), verified on macOS 26.6.2.
//! * [`responsible_pid`] — `responsibility_get_pid_responsible_for_pid`,
//!   resolved through `dlsym` because no SDK header declares it.
//! * [`responsible_app`] — `proc_pidpath`, plus at most one bounded read of an
//!   `Info.plist` that is **not** under a protected root.
//! * [`tccutil_presence`] — one `stat` of `/usr/bin/tccutil`, to decide whether
//!   §3.7's repair button may be shown. `/usr/bin` is not a protected root and
//!   a file mode is not a privacy question, so this reaches neither `tccd` nor
//!   `WindowServer`.
//!
//! Each wrapper has a pure classifier beside it ([`classify_probe`],
//! [`classify_responsible`], [`display_name`]) so the interesting logic is
//! table-testable without an operating system.
//!
//! ## The in-bundle guard is the safety gate, and it is not optional
//!
//! On 2026-08-17 a unit test's first `WindowServer` touch made `tccd`
//! `readdir` a 1.1-million-entry `target/debug/deps`; `WindowServer` outran its 40 s
//! watchdog and was killed, taking every GUI session on the machine with it
//! (`AGENTS.md` rule 5, `tools/grep_guard.sh` B9). The standing rule for the
//! class is that a platform probe a headless-constructible path can reach must
//! be gated, because a headless process is exactly what a unit test is.
//!
//! So [`probe_fda`] **refuses**, performing NO syscall at all, unless the
//! running executable resolves inside a `.app` (`…/Contents/MacOS/…` —
//! [`path_is_in_app_bundle`]). A binary under `target/debug/deps` gets
//! [`FdaState::Unknown`] with [`ProbeLabel::RefusedOutOfBundle`], and
//! `probe_refuses_out_of_bundle_without_touching_the_os` proves the syscall is
//! not merely skipped but unreachable: the injected syscall closure panics if
//! it is ever called.
//!
//! ## Protected paths are resolved here, once
//!
//! `aterm-gui` and `aterm-cli` may not write a protected-folder path literal
//! as a syscall argument. [`protected_roots`] resolves them once, from
//! [`crate::sbpl::PRIVATE_SUBDIRS`] — the same list the Containment tier
//! denies — so the containment tier and the consent tier cannot disagree about
//! which paths are sensitive. Callers receive already-resolved [`PathBuf`]s
//! and thread them as data.

use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// errno values, pinned so classification stays pure on every platform
// ---------------------------------------------------------------------------

/// `EPERM`. A TCC denial is `EPERM`, **not** `EACCES` (design §1.5) — the
/// distinction is the whole probe. Pinned as a literal so the classifiers stay
/// pure and compile off macOS; `errno_constants_match_the_platform` asserts it
/// equals `libc::EPERM` on macOS.
pub const ERRNO_EPERM: i32 = 1;

/// `ESRCH` — "no such process". Distinguishes "the pid is gone" from "the pid
/// is not ours" when the responsibility SPI returns `-1`.
pub const ERRNO_ESRCH: i32 = 3;

/// The user's TCC store, relative to `$HOME`. Reaching it requires Full Disk
/// Access; its contents are never read.
const TCC_DB_RELATIVE: &str = "Library/Application Support/com.apple.TCC/TCC.db";

/// Hard cap on an `Info.plist` read. A plist over the cap reads as absent —
/// unbounded work on a path we do not control is not acceptable in a probe.
#[cfg(target_os = "macos")]
const INFO_PLIST_MAX_BYTES: u64 = 256 * 1024;

// ---------------------------------------------------------------------------
// Full Disk Access
// ---------------------------------------------------------------------------

/// Whether the responsible app holds Full Disk Access
/// (`kTCCServiceSystemPolicyAllFiles`).
///
/// `Unknown` is a real answer, not a failure: it is what an out-of-bundle
/// caller, a disabled probe, or an unexpected errno all report, and it must
/// never be rendered as "denied".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FdaState {
    /// The `TCC.db` open succeeded — the grant is held by this identity.
    Granted,
    /// The open returned [`ERRNO_EPERM`] — the grant is not held.
    Denied,
    /// Not measured. The probe refused, was disabled, or the errno was one
    /// this classification does not claim to understand.
    #[default]
    Unknown,
}

impl FdaState {
    /// The `full_disk_access=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

/// How far a Full Disk Access grant reaches: to the running process, or only
/// to processes started later.
///
/// Apple's own Settings sheet implies the latter, but it was never measured
/// for aterm's shape (design §7 S1, BLOCKING), so this stays
/// [`FdaScope::Unknown`] and callers must render it as unknown rather than
/// guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FdaScope {
    /// The running process observes the grant.
    ThisProcess,
    /// Only a process started after the grant observes it.
    NewProcesses,
    /// Not measured. The value every caller sees today.
    #[default]
    Unknown,
}

impl FdaScope {
    /// The `fda_scope=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThisProcess => "this_process",
            Self::NewProcesses => "new_processes",
            Self::Unknown => "unknown",
        }
    }
}

/// What the one probe syscall did. Kept separate from [`FdaState`] so the
/// classification is a pure table and the reason survives into the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeLabel {
    /// The open succeeded.
    OpenOk,
    /// The open returned [`ERRNO_EPERM`].
    OpenEperm,
    /// The open failed with some other errno (carried for the report).
    OpenErrno(i32),
    /// Refused: the running executable is not inside a `.app` bundle. NO
    /// syscall was performed. This is the 2026-08-17 fence.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved at all. NO
    /// syscall was performed.
    RefusedNoExe,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off. NO syscall
    /// was performed, and the row must name the config rather than imply a
    /// denial.
    RefusedDisabled,
    /// Refused: `$HOME` is unset, so there is no store to reach for. NO
    /// syscall was performed.
    RefusedNoHome,
    /// Refused: the resolved store path cannot be handed to a C API (it
    /// contains an interior NUL). NO syscall was performed.
    RefusedBadPath,
    /// Not macOS. There is no TCC here.
    UnsupportedPlatform,
}

impl ProbeLabel {
    /// The `probe=` token. [`ProbeLabel::OpenErrno`] renders as `open_errno`;
    /// callers that want the number read it from the variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenOk => "open_ok",
            Self::OpenEperm => "open_eperm",
            Self::OpenErrno(_) => "open_errno",
            Self::RefusedOutOfBundle => "refused_out_of_bundle",
            Self::RefusedNoExe => "refused_no_exe",
            Self::RefusedDisabled => "refused_disabled",
            Self::RefusedNoHome => "refused_no_home",
            Self::RefusedBadPath => "refused_bad_path",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }

    /// `true` when no syscall was performed. Every `Refused*` label and
    /// [`ProbeLabel::UnsupportedPlatform`] answer `true`.
    #[must_use]
    pub const fn refused(self) -> bool {
        matches!(
            self,
            Self::RefusedOutOfBundle
                | Self::RefusedNoExe
                | Self::RefusedDisabled
                | Self::RefusedNoHome
                | Self::RefusedBadPath
                | Self::UnsupportedPlatform
        )
    }
}

/// One probe result: the state, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FdaProbe {
    /// The classified state.
    pub state: FdaState,
    /// What the probe actually did.
    pub label: ProbeLabel,
}

impl FdaProbe {
    /// A refusal: [`FdaState::Unknown`] with the given reason.
    #[must_use]
    pub const fn refused(label: ProbeLabel) -> Self {
        Self {
            state: FdaState::Unknown,
            label,
        }
    }
}

/// The raw result of the one probe syscall, before classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeOutcome {
    /// The open succeeded (and the fd was closed immediately).
    Ok,
    /// The open failed with this errno.
    Errno(i32),
}

/// The two `[privacy]` switches that gate the probe. Both must be on for a
/// syscall to happen; either off is [`ProbeLabel::RefusedDisabled`], which the
/// report must attribute to configuration and never to a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProbeGate {
    /// `[privacy] enabled` — the master switch.
    pub enabled: bool,
    /// `[privacy] check` — the silent, prompt-free probe.
    pub check: bool,
}

impl ProbeGate {
    /// Both switches on — the shipped default.
    #[must_use]
    pub const fn on() -> Self {
        Self {
            enabled: true,
            check: true,
        }
    }

    /// `true` when a syscall is permitted by configuration alone (the
    /// in-bundle guard is checked separately, and is not a configuration).
    #[must_use]
    pub const fn permits(self) -> bool {
        self.enabled && self.check
    }
}

impl Default for ProbeGate {
    fn default() -> Self {
        Self::on()
    }
}

/// The user's `TCC.db` under `home`. Its contents are never read; only its
/// reachability is a fact about Full Disk Access.
#[must_use]
pub fn tcc_db_path(home: &Path) -> PathBuf {
    home.join(TCC_DB_RELATIVE)
}

/// Classify one probe syscall. Pure.
///
/// `Ok` ⇒ [`FdaState::Granted`]; [`ERRNO_EPERM`] ⇒ [`FdaState::Denied`];
/// **anything else** — `EACCES`, `ENOENT`, `EINTR`, … — ⇒
/// [`FdaState::Unknown`]. Only `EPERM` is a TCC denial (design §1.5); reading
/// any other errno as a denial would manufacture a verdict.
#[must_use]
pub const fn classify_probe(outcome: ProbeOutcome) -> FdaProbe {
    match outcome {
        ProbeOutcome::Ok => FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        },
        ProbeOutcome::Errno(ERRNO_EPERM) => FdaProbe {
            state: FdaState::Denied,
            label: ProbeLabel::OpenEperm,
        },
        ProbeOutcome::Errno(other) => FdaProbe {
            state: FdaState::Unknown,
            label: ProbeLabel::OpenErrno(other),
        },
    }
}

/// The answer on a platform that has no TCC. Compiled everywhere so it can be
/// tested everywhere; used as the whole implementation off macOS.
#[must_use]
pub const fn unsupported_probe() -> FdaProbe {
    FdaProbe::refused(ProbeLabel::UnsupportedPlatform)
}

/// The probe, with the executable path, the home directory and the syscall all
/// injected. This is the seam every guard is enforced at, and the seam a test
/// uses to prove a refusal performs no syscall.
///
/// `syscall` is called **at most once**, and only after every refusal has been
/// ruled out. It receives the resolved `TCC.db` path.
pub fn probe_fda_with<F>(
    gate: ProbeGate,
    exe: Option<&Path>,
    home: Option<&Path>,
    syscall: F,
) -> FdaProbe
where
    F: FnOnce(&Path) -> ProbeOutcome,
{
    if !gate.permits() {
        return FdaProbe::refused(ProbeLabel::RefusedDisabled);
    }
    let Some(exe) = exe else {
        return FdaProbe::refused(ProbeLabel::RefusedNoExe);
    };
    // THE FENCE (design §3.3 guardrail 1). A binary outside a `.app` — a test
    // binary under target/debug/deps above all — never reaches the syscall.
    if !path_is_in_app_bundle(exe) {
        return FdaProbe::refused(ProbeLabel::RefusedOutOfBundle);
    }
    let Some(home) = home else {
        return FdaProbe::refused(ProbeLabel::RefusedNoHome);
    };
    let db = tcc_db_path(home);
    if path_has_interior_nul(&db) {
        return FdaProbe::refused(ProbeLabel::RefusedBadPath);
    }
    classify_probe(syscall(&db))
}

/// Probe Full Disk Access for the running process.
///
/// Performs at most ONE `open(…, O_RDONLY)` of the user's `TCC.db`, closes the
/// fd immediately and never reads a byte. Prompt-free by construction:
/// `kTCCServiceSystemPolicyAllFiles` cannot prompt.
///
/// Refuses — with no syscall — when the gate is off, when `$HOME` is unset, or
/// when the running executable is not inside a `.app` bundle. Off macOS it is
/// [`unsupported_probe`].
#[must_use]
pub fn probe_fda(gate: ProbeGate) -> FdaProbe {
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        probe_fda_with(gate, exe.as_deref(), home.as_deref(), imp::open_probe)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = gate;
        unsupported_probe()
    }
}

// ---------------------------------------------------------------------------
// The in-bundle guard
// ---------------------------------------------------------------------------

/// `true` when `exe` resolves inside a `.app` bundle — that is, some component
/// ends in `.app` and is followed by `Contents`, `MacOS`, and at least one
/// more component.
///
/// Purely lexical, and deliberately so: it must be answerable without touching
/// the filesystem, because it is the gate that decides whether the filesystem
/// may be touched at all.
#[must_use]
pub fn path_is_in_app_bundle(exe: &Path) -> bool {
    app_bundle_root(exe).is_some()
}

/// The `.app` directory `exe` lives inside, if any.
///
/// `/Applications/aterm.app/Contents/MacOS/aterm` ⇒
/// `/Applications/aterm.app`. Also the cache key's bundle half.
#[must_use]
pub fn app_bundle_root(exe: &Path) -> Option<PathBuf> {
    let components: Vec<Component<'_>> = exe.components().collect();
    // `<X>.app` / `Contents` / `MacOS` / `<binary>` — four components at least.
    let last_start = components.len().checked_sub(4)?;
    for index in (0..=last_start).rev() {
        // `Path::extension` rather than a suffix test: it answers `None` for a
        // component that is exactly `.app` (a hidden file, not a bundle), it
        // needs no UTF-8 round trip, and macOS volumes are conventionally
        // case-insensitive.
        let name = Path::new(components[index].as_os_str());
        if !name
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            continue;
        }
        if components[index + 1].as_os_str() != "Contents"
            || components[index + 2].as_os_str() != "MacOS"
        {
            continue;
        }
        let mut root = PathBuf::new();
        for component in &components[..=index] {
            root.push(component.as_os_str());
        }
        return Some(root);
    }
    None
}

/// `true` when the path cannot be handed to a C API because it contains an
/// interior NUL byte.
#[must_use]
fn path_has_interior_nul(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().contains(&0)
}

// ---------------------------------------------------------------------------
// Responsibility
// ---------------------------------------------------------------------------

/// Why the responsibility SPI could not answer.
///
/// `responsibility_get_pid_responsible_for_pid` returns `-1` for every process
/// the caller does not **own** (the gate is a euid mismatch, not privilege),
/// so `-1` is overloaded and `errno` is what separates the cases. `-1` maps to
/// `Unknown` — **never** to "self".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResponsibleError {
    /// The symbol is not present in this process image.
    SymbolUnavailable,
    /// [`ERRNO_EPERM`] — the target is not ours.
    NotOurs,
    /// [`ERRNO_ESRCH`] — the target is gone.
    Gone,
    /// `-1` with some other errno.
    Errno(i32),
    /// Not macOS.
    Unsupported,
}

/// The `responsible=` corroboration. This is the SPI's opinion, and it is only
/// ever a corroboration: [`Attribution`] comes from aterm's own adoption
/// record, never from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Responsible {
    /// The responsible pid is this process.
    SelfProcess,
    /// The responsible pid is gone — the process that started this session has
    /// exited.
    Exited,
    /// The responsible pid is some other live process.
    Other(i32),
    /// The SPI is absent, refused, or answered `-1` for a reason that does not
    /// identify anything.
    #[default]
    Unknown,
}

impl Responsible {
    /// The `responsible=` token, or `None` for [`Responsible::Other`], whose
    /// token is the decimal pid the caller renders.
    #[must_use]
    pub const fn token(self) -> Option<&'static str> {
        match self {
            Self::SelfProcess => Some("self"),
            Self::Exited => Some("exited"),
            Self::Unknown => Some("unknown"),
            Self::Other(_) => None,
        }
    }

    /// The pid, when one was actually identified.
    #[must_use]
    pub const fn pid(self) -> Option<i32> {
        match self {
            Self::Other(pid) => Some(pid),
            _ => None,
        }
    }
}

/// Classify a responsibility answer against the asking process's own pid.
/// Pure.
///
/// [`ResponsibleError::Gone`] is the only error that says something positive
/// (the responsible process exited); every other error is
/// [`Responsible::Unknown`], because a `-1` that is not `ESRCH` identifies
/// nothing at all.
#[must_use]
pub const fn classify_responsible(
    self_pid: i32,
    answer: Result<i32, ResponsibleError>,
) -> Responsible {
    match answer {
        Ok(pid) if pid == self_pid => Responsible::SelfProcess,
        // A non-positive pid is not an identification; refuse to render it.
        Ok(pid) if pid > 0 => Responsible::Other(pid),
        Err(ResponsibleError::Gone) => Responsible::Exited,
        // Everything else — a `-1` that is not `ESRCH`, an absent symbol, a
        // zero or negative "success" — identifies nothing at all.
        Ok(_) | Err(_) => Responsible::Unknown,
    }
}

/// Map the SPI's `(return value, errno)` pair to a result. Pure.
pub const fn responsible_answer(ret: i32, errno: i32) -> Result<i32, ResponsibleError> {
    if ret >= 0 {
        return Ok(ret);
    }
    match errno {
        ERRNO_EPERM => Err(ResponsibleError::NotOurs),
        ERRNO_ESRCH => Err(ResponsibleError::Gone),
        other => Err(ResponsibleError::Errno(other)),
    }
}

/// The pid macOS holds responsible for `pid`, or `None`.
///
/// Resolved through `dlsym` (no SDK header declares
/// `responsibility_get_pid_responsible_for_pid`; it lives in
/// `/usr/lib/system/libquarantine.dylib`) with a `None` fallback, so a macOS
/// that drops the symbol degrades to `unknown` instead of failing to link.
///
/// Unprivileged, and it contacts neither tccd nor `WindowServer`:
/// `libquarantine`
/// imports no XPC, Mach or bootstrap symbol and the call is a single
/// `__mac_syscall` into the kernel Quarantine policy.
///
/// `-1` — returned for any process the caller does not own — is `None`, never
/// "self". Use [`responsible_pid_detailed`] when the reason matters.
#[must_use]
pub fn responsible_pid(pid: i32) -> Option<i32> {
    responsible_pid_detailed(pid).ok()
}

/// [`responsible_pid`], keeping the reason a `-1` carries.
pub fn responsible_pid_detailed(pid: i32) -> Result<i32, ResponsibleError> {
    #[cfg(target_os = "macos")]
    {
        imp::responsible_pid_detailed(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Err(ResponsibleError::Unsupported)
    }
}

/// The app macOS holds responsible for `pid`, resolved far enough to NAME it.
///
/// The verdict belongs to the responsible app — a probe run from a shell under
/// another terminal reports THAT terminal's Full Disk Access, not aterm's — so
/// a report that does not name it is misleading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsibleApp {
    /// The responsible pid.
    pub pid: i32,
    /// Its executable path, from `proc_pidpath`.
    pub path: PathBuf,
    /// Its display name: `CFBundleDisplayName`, else `CFBundleName`, else the
    /// executable's basename.
    pub display_name: String,
}

/// The `Info.plist` a display name may be read from, or `None` when there is
/// none we may touch.
///
/// **The guard.** Reading an `Info.plist` belonging to an app that lives under
/// a protected root — `~/Documents/Something.app` — is the very syscall this
/// design exists to avoid: it would raise the consent dialog while trying to
/// describe consent. So a bundle under any protected root answers `None` and
/// the caller falls back to the basename.
///
/// The containment test is lexical (see [`is_under_protected_root`]).
#[must_use]
pub fn readable_info_plist(exe: &Path, protected: &[PathBuf]) -> Option<PathBuf> {
    let root = app_bundle_root(exe)?;
    if is_under_protected_root(&root, protected) {
        return None;
    }
    Some(root.join("Contents").join("Info.plist"))
}

/// `true` when `path` is `root` or lives under it, for any root.
///
/// Lexical, component-wise: it neither canonicalizes nor stats, because it
/// guards a decision about whether to touch the filesystem at all. A protected
/// directory reached through a symlink from outside is therefore not caught —
/// stated here rather than discovered later.
#[must_use]
pub fn is_under_protected_root(path: &Path, protected: &[PathBuf]) -> bool {
    protected.iter().any(|root| path.starts_with(root))
}

/// The display name for an executable, given its bundle's `Info.plist` text if
/// — and only if — [`readable_info_plist`] allowed one to be read. Pure.
///
/// `CFBundleDisplayName`, else `CFBundleName`, else the executable's basename.
/// Only XML plists are understood; a binary plist reads as absent and falls
/// back to the basename, which for the shapes that matter
/// (`…/iTerm.app/Contents/MacOS/iTerm2`) is the same string anyway.
#[must_use]
pub fn display_name(exe: &Path, info_plist_text: Option<&str>) -> String {
    if let Some(text) = info_plist_text {
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let Some(value) = xml_plist_string(text, key) {
                return value.to_owned();
            }
        }
    }
    exe.file_name().map_or_else(
        || String::from("unknown"),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The string immediately following one exact XML plist key, non-empty and
/// trimmed. The value element must follow the key modulo whitespace, so a
/// malformed or intervening key's string can never be mis-bound.
fn xml_plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut haystack = text;
    loop {
        let at = haystack.find("<key>")?;
        let after = &haystack[at + "<key>".len()..];
        let end = after.find("</key>")?;
        let found = after[..end].trim();
        let tail = &after[end + "</key>".len()..];
        if found == key {
            let value_tail = tail.trim_start().strip_prefix("<string>")?;
            let string_end = value_tail.find("</string>")?;
            let value = value_tail[..string_end].trim();
            return (!value.is_empty()).then_some(value);
        }
        haystack = tail;
    }
}

/// The responsible app for `pid`, using the default protected roots.
#[must_use]
pub fn responsible_app(pid: i32) -> Option<ResponsibleApp> {
    responsible_app_with_roots(pid, &protected_roots(&[]))
}

/// [`responsible_app`], against a caller-resolved protected-root set (so a
/// `[privacy] protected_roots` override is honoured).
#[must_use]
pub fn responsible_app_with_roots(pid: i32, protected: &[PathBuf]) -> Option<ResponsibleApp> {
    #[cfg(target_os = "macos")]
    {
        imp::responsible_app_with_roots(pid, protected)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, protected);
        None
    }
}

// ---------------------------------------------------------------------------
// Designated-requirement class
// ---------------------------------------------------------------------------

/// The class of a code-signing **designated requirement**, which is what
/// `tccd` stores in the `csreq` column and re-validates the running code
/// against.
///
/// This is the difference between a grant that survives a rebuild and one that
/// does not: a Developer-ID DR names an `identifier` and pins a *certificate*;
/// an ad-hoc DR is a bare `cdhash` and dies the moment a single byte in the
/// bundle changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DrClass {
    /// `identifier "…" and anchor apple generic … certificate leaf …` — no
    /// cdhash. Survives rebuilds and in-place updates.
    Identity,
    /// The requirement pins a `cdhash`. Every rebuild is a different identity.
    Cdhash,
    /// The code object is not signed at all.
    Unsigned,
    /// Not recognised. Never treated as stable.
    #[default]
    Unknown,
}

impl DrClass {
    /// The `dr=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Cdhash => "cdhash",
            Self::Unsigned => "unsigned",
            Self::Unknown => "unknown",
        }
    }

    /// `grant_stable=` — whether a TCC grant keyed to this requirement
    /// survives a rebuild. Only [`DrClass::Identity`] does; everything else,
    /// including [`DrClass::Unknown`], is treated as unstable.
    #[must_use]
    pub const fn grant_stable(self) -> bool {
        matches!(self, Self::Identity)
    }
}

/// Classify a `codesign -d -r-` requirement STRING. Pure — it takes the
/// string; it does not shell out.
///
/// Precedence is deliberate and fails toward "unstable":
///
/// 1. an unsigned marker anywhere ⇒ [`DrClass::Unsigned`];
/// 2. a `cdhash` clause ⇒ [`DrClass::Cdhash`], **even alongside an
///    identifier** — a requirement that pins a code-directory hash is
///    invalidated by the next build whatever else it says;
/// 3. an `identifier` clause plus a certificate/anchor clause ⇒
///    [`DrClass::Identity`];
/// 4. otherwise [`DrClass::Unknown`].
#[must_use]
pub fn classify_dr(requirement: &str) -> DrClass {
    let text = requirement.trim();
    if text.is_empty() {
        return DrClass::Unknown;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("not signed at all") || lower.contains("code object is not signed") {
        return DrClass::Unsigned;
    }
    if lower.contains("cdhash") {
        return DrClass::Cdhash;
    }
    let has_identifier = lower.contains("identifier ") || lower.contains("identifier\"");
    let has_anchor = lower.contains("anchor apple")
        || lower.contains("certificate leaf")
        || lower.contains("certificate root")
        || lower.contains("subject.ou");
    if has_identifier && has_anchor {
        return DrClass::Identity;
    }
    DrClass::Unknown
}

// ---------------------------------------------------------------------------
// Protected roots
// ---------------------------------------------------------------------------

/// The protected roots, resolved once, as absolute paths.
///
/// With `overrides` empty this is [`crate::sbpl::PRIVATE_SUBDIRS`] joined onto
/// `$HOME` — the SAME list the Containment tier denies, on purpose: the
/// containment tier and the consent tier must not disagree about which paths
/// are sensitive.
///
/// An override is taken verbatim when absolute and expanded when it starts
/// `~/`; anything else is skipped (`--validate-config` warns about it — this
/// layer refuses to guess a base for a relative path).
///
/// Returns an empty vector when `$HOME` is unset and no absolute override was
/// given. Callers must treat "no roots" as "cannot answer", never as "nothing
/// is protected".
#[must_use]
pub fn protected_roots(overrides: &[String]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    protected_roots_in(home.as_deref(), overrides)
}

/// [`protected_roots`], with `$HOME` injected. Pure.
#[must_use]
pub fn protected_roots_in(home: Option<&Path>, overrides: &[String]) -> Vec<PathBuf> {
    if overrides.is_empty() {
        let Some(home) = home else {
            return Vec::new();
        };
        return crate::sbpl::PRIVATE_SUBDIRS
            .iter()
            .map(|sub| home.join(sub))
            .collect();
    }
    let mut roots = Vec::with_capacity(overrides.len());
    for entry in overrides {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some(rest) = entry.strip_prefix("~/") {
            if let Some(home) = home {
                roots.push(home.join(rest));
            }
            continue;
        }
        if entry == "~" {
            if let Some(home) = home {
                roots.push(home.to_path_buf());
            }
            continue;
        }
        let path = Path::new(entry);
        if path.is_absolute() {
            roots.push(path.to_path_buf());
        }
    }
    roots
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

/// One promptable per-folder service, named the way `[privacy] warmup_folders`
/// names it.
///
/// The path literals live HERE and nowhere else: `aterm-gui` and `aterm-cli`
/// receive resolved [`PathBuf`]s as data and never write one of these names as
/// a syscall argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    /// `~/Documents` — `kTCCServiceSystemPolicyDocumentsFolder`.
    Documents,
    /// `~/Desktop` — `kTCCServiceSystemPolicyDesktopFolder`.
    Desktop,
    /// `~/Downloads` — `kTCCServiceSystemPolicyDownloadsFolder`.
    Downloads,
}

impl Folder {
    /// Every folder this module knows, in the order the warm-up walks them.
    pub const ALL: &'static [Self] = &[Self::Documents, Self::Desktop, Self::Downloads];

    /// The configuration / report name (`documents`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Desktop => "desktop",
            Self::Downloads => "downloads",
        }
    }

    /// The `tccutil` service name (`SystemPolicyDocumentsFolder`) — the
    /// `kTCCService` prefix is not part of `tccutil`'s argument.
    #[must_use]
    pub const fn tcc_service(self) -> &'static str {
        match self {
            Self::Documents => "SystemPolicyDocumentsFolder",
            Self::Desktop => "SystemPolicyDesktopFolder",
            Self::Downloads => "SystemPolicyDownloadsFolder",
        }
    }

    /// Parse a configuration name. Case-insensitive; unknown names are `None`
    /// so `--validate-config` can warn rather than silently dropping one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "documents" => Some(Self::Documents),
            "desktop" => Some(Self::Desktop),
            "downloads" => Some(Self::Downloads),
            _ => None,
        }
    }

    /// The absolute path under `home`.
    #[must_use]
    pub fn path(self, home: &Path) -> PathBuf {
        match self {
            Self::Documents => home.join("Documents"),
            Self::Desktop => home.join("Desktop"),
            Self::Downloads => home.join("Downloads"),
        }
    }
}

/// Resolve configured folder names to `(folder, absolute path)` pairs, in the
/// order given, skipping names this module does not know.
///
/// This is how the warm-up worker gets its paths: as data, already resolved,
/// so the worker contains no path literal of its own.
#[must_use]
pub fn folder_paths_in(home: Option<&Path>, names: &[String]) -> Vec<(Folder, PathBuf)> {
    let Some(home) = home else {
        return Vec::new();
    };
    names
        .iter()
        .filter_map(|name| Folder::parse(name))
        .map(|folder| (folder, folder.path(home)))
        .collect()
}

/// [`folder_paths_in`], reading `$HOME` from the environment.
#[must_use]
pub fn folder_paths(names: &[String]) -> Vec<(Folder, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    folder_paths_in(home.as_deref(), names)
}

/// Build the `tccutil reset <service> <bundle id>` argv for one folder. PURE —
/// it constructs a command; it never runs one.
///
/// `bundle_id` must come from the **running** bundle, so a dev channel resets
/// its own rows and can never reset the release's. Nothing in this crate reads
/// a bundle id from a literal.
///
/// Reachable only from the Settings panel's owner gesture: it is a destructive
/// mutation of the human's privacy state, and a program inside a session that
/// could trigger it could re-arm a prompt the human already answered.
#[must_use]
pub fn tccutil_reset_command(bundle_id: &str, folder: Folder) -> Vec<String> {
    vec![
        String::from("/usr/bin/tccutil"),
        String::from("reset"),
        String::from(folder.tcc_service()),
        String::from(bundle_id),
    ]
}

// ---------------------------------------------------------------------------
// Denied-state repair (design §3.7) — PURE
// ---------------------------------------------------------------------------
//
// A persisted "Don't Allow" and a stale-`csreq` silent failure are
// indistinguishable from outside: `EPERM`, no dialog, and macOS never asks
// again. The only unprivileged repair is `tccutil reset`, which removes THIS
// app's row for one service so macOS can ask again. It grants nothing, it
// cannot help with Full Disk Access, and this module NEVER runs it: everything
// below builds argv and folds recorded results. The spawn lives in the
// Settings panel, behind an owner gesture, and is fenced off every control
// verb (§3.7) — a program inside a session that could trigger a reset could
// re-arm a prompt the human already answered.
//
// Nothing here claims what a grant covers once the human answers again. That
// is §7 S4's measurement, it has not been made on this tree, and no string
// built from these types may imply it.

/// The `tccutil` binary, absolute.
///
/// Absolute on purpose: a destructive mutation of the human's privacy state may
/// not be resolved through `PATH`, where anything could answer to the name.
pub const TCCUTIL_PATH: &str = "/usr/bin/tccutil";

/// Whether [`TCCUTIL_PATH`] can be run at all.
///
/// §3.7: "if `tccutil` is missing or not executable, the button is not shown at
/// all". This is a property of the FILE — it says nothing about the privacy
/// state, and it is never rendered as a privacy verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TccutilPresence {
    /// A file at [`TCCUTIL_PATH`] carrying an execute bit.
    Executable,
    /// Nothing usable at [`TCCUTIL_PATH`] — absent, or not a file.
    Missing,
    /// Present, but with no execute bit.
    NotExecutable,
    /// Not determined: the path could not be examined, or this is not a unix
    /// where an execute bit exists. Never treated as runnable.
    #[default]
    Unknown,
}

impl TccutilPresence {
    /// The report token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::Missing => "missing",
            Self::NotExecutable => "not-executable",
            Self::Unknown => "unknown",
        }
    }

    /// Whether an invocation may be attempted. Only [`Self::Executable`] —
    /// [`Self::Unknown`] fails toward "do not offer a destructive button".
    #[must_use]
    pub const fn can_run(self) -> bool {
        matches!(self, Self::Executable)
    }
}

/// Classify a stat of [`TCCUTIL_PATH`]. PURE — the caller does the stat.
///
/// A path that exists but is not a regular file is [`TccutilPresence::Missing`]:
/// there is no tool there.
#[must_use]
pub const fn classify_tccutil(is_file: bool, executable: bool) -> TccutilPresence {
    match (is_file, executable) {
        (false, _) => TccutilPresence::Missing,
        (true, true) => TccutilPresence::Executable,
        (true, false) => TccutilPresence::NotExecutable,
    }
}

/// One `stat` of [`TCCUTIL_PATH`] — no spawn, no `tccd`, no `WindowServer`,
/// and nothing under a protected root.
///
/// `/usr/bin` is not a protected root and this asks the kernel about a file
/// mode, so it is safe from a headless process and from a unit test; it is the
/// one function in this section that touches the operating system, and it
/// mutates nothing. A read failure that is not "no such file" answers
/// [`TccutilPresence::Unknown`] rather than claiming the tool is absent.
#[cfg(unix)]
#[must_use]
pub fn tccutil_presence() -> TccutilPresence {
    use std::os::unix::fs::PermissionsExt as _;

    match std::fs::metadata(TCCUTIL_PATH) {
        Ok(md) => classify_tccutil(md.is_file(), md.permissions().mode() & 0o111 != 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => TccutilPresence::Missing,
        Err(_) => TccutilPresence::Unknown,
    }
}

/// Off unix there is no execute bit to read, so the answer is
/// [`TccutilPresence::Unknown`] and the button is never offered.
#[cfg(not(unix))]
#[must_use]
pub fn tccutil_presence() -> TccutilPresence {
    TccutilPresence::Unknown
}

/// Whether a string may be used as the `tccutil` subject.
///
/// The rule is deliberately narrow, and every clause is a safety property
/// rather than a style preference:
///
/// * non-blank — an empty subject is the one argv shape that must never be
///   built, because `tccutil reset <service>` with no subject is the
///   every-app form;
/// * no interior whitespace and no control characters — a bundle id read out
///   of a plist that looks like that was not read correctly;
/// * no leading `-` — an argument that would be taken for a flag.
#[must_use]
pub fn bundle_id_is_usable(bundle_id: &str) -> bool {
    let id = bundle_id.trim();
    !id.is_empty()
        && !id.starts_with('-')
        && !id.contains(char::is_whitespace)
        && !id.chars().any(char::is_control)
}

/// What the Settings panel should do about the *Ask again* repair. A pure
/// DECISION — the panel owns the prose; this crate owns the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOffer {
    /// Show the button. It re-arms macOS's question for the folders in the
    /// plan; it makes no claim about what a grant that follows will cover.
    Offer,
    /// Show NO button. Say instead that the grant was invalidated by a
    /// **rebuild** rather than by the owner, and point at the dev-identity fix
    /// (§3.1): this code object's identity does not survive a build, so a
    /// reset could not hold and offering one would be a lie. The panel words
    /// this from the [`DrClass`] it already has.
    ExplainRebuild,
    /// Show nothing: [`TCCUTIL_PATH`] cannot be run here (missing, not
    /// executable, or not determinable).
    HideNoTool,
    /// Show nothing: the running bundle id is not known, so no argv can be
    /// built FROM THE RUNNING BUNDLE — and nothing here ever falls back to a
    /// literal, because a dev channel must never be able to reset the
    /// release's rows.
    HideNoBundleId,
}

impl ResetOffer {
    /// The report token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offer => "offer",
            Self::ExplainRebuild => "explain-rebuild",
            Self::HideNoTool => "hide-no-tool",
            Self::HideNoBundleId => "hide-no-bundle-id",
        }
    }

    /// Whether the *Ask again* button is rendered at all.
    #[must_use]
    pub const fn shows_button(self) -> bool {
        matches!(self, Self::Offer)
    }
}

/// Everything [`reset_offer`] reads. All observed: a classified designated
/// requirement, a stat, and the running bundle's own id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ResetOfferInputs<'a> {
    /// The designated-requirement class of the RUNNING code object
    /// ([`classify_dr`]).
    pub dr: DrClass,
    /// Whether [`TCCUTIL_PATH`] can be run ([`tccutil_presence`]).
    pub tccutil: TccutilPresence,
    /// The RUNNING bundle's `CFBundleIdentifier`; `None` when it could not be
    /// read. Never a literal, never the release's id on a dev build.
    pub bundle_id: Option<&'a str>,
}

/// Decide whether the denied-state repair may be offered. PURE.
///
/// Precedence, and why:
///
/// 1. **identity first.** [`DrClass::Cdhash`] is §3.7's named case — the
///    requirement pins a code-directory hash, so a grant made against this
///    identity does not survive a build, and §3.7 rules that the panel says
///    the grant was invalidated by a REBUILD rather than by the owner.
///    [`DrClass::Unsigned`] joins it because §3.1 says the same of an ad-hoc
///    bundle in its own words: access grants will not survive the next build
///    of that bundle. Neither sentence depends on `tccutil` existing, so this
///    outranks the tool check.
/// 2. **the tool.** No runnable [`TCCUTIL_PATH`], no button (§3.7).
/// 3. **the subject.** No usable running bundle id, no button — this builder
///    has no fallback and must not acquire one.
///
/// [`DrClass::Unknown`] does NOT suppress the offer: we did not observe an
/// unstable identity, and claiming a rebuild we cannot see would be exactly
/// the over-claim [`SpikeEvidence`] exists to prevent. The reset still re-arms
/// the question; only the durability of a later grant is unknown, and the
/// panel promises nothing about it.
///
/// This answers only "may the repair be offered". WHEN to render the repair at
/// all — a folder that is denied with no prompt possible — stays with the
/// panel, which owns that state.
#[must_use]
pub fn reset_offer(inputs: ResetOfferInputs<'_>) -> ResetOffer {
    match inputs.dr {
        DrClass::Cdhash | DrClass::Unsigned => ResetOffer::ExplainRebuild,
        DrClass::Identity | DrClass::Unknown => {
            if inputs.tccutil.can_run() {
                match inputs.bundle_id {
                    Some(id) if bundle_id_is_usable(id) => ResetOffer::Offer,
                    _ => ResetOffer::HideNoBundleId,
                }
            } else {
                ResetOffer::HideNoTool
            }
        }
    }
}

/// One owner-initiated *Ask again*: one `tccutil reset` per folder, every argv
/// built from the SAME running bundle id.
///
/// The plan is data. Building one runs nothing, and a plan carries each folder
/// exactly once — there is no retry and no loop in this design, so a repeated
/// folder cannot enter through the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResetPlan {
    bundle_id: String,
    folders: Vec<Folder>,
}

impl ResetPlan {
    /// Build a plan, or `None` when there is no safe argv to build: a bundle
    /// id that is not usable ([`bundle_id_is_usable`]) or no folder at all.
    ///
    /// Duplicate folders are dropped, keeping first-seen order.
    #[must_use]
    pub fn new(bundle_id: &str, folders: &[Folder]) -> Option<Self> {
        if !bundle_id_is_usable(bundle_id) {
            return None;
        }
        let mut wanted: Vec<Folder> = Vec::with_capacity(folders.len());
        for folder in folders {
            if !wanted.contains(folder) {
                wanted.push(*folder);
            }
        }
        if wanted.is_empty() {
            return None;
        }
        Some(Self {
            bundle_id: bundle_id.trim().to_owned(),
            folders: wanted,
        })
    }

    /// Build a plan only when [`reset_offer`] says the repair may be offered.
    ///
    /// This is the entry point the panel should use: it makes "the button is
    /// hidden" and "no argv exists" the same fact, so a suppressed repair
    /// cannot be run by a caller that forgot to check.
    #[must_use]
    pub fn for_offer(inputs: ResetOfferInputs<'_>, folders: &[Folder]) -> Option<Self> {
        if !reset_offer(inputs).shows_button() {
            return None;
        }
        Self::new(inputs.bundle_id?, folders)
    }

    /// The subject every invocation names — the running bundle's id, trimmed.
    #[must_use]
    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    /// The folders, in plan order, each appearing once.
    #[must_use]
    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    /// How many invocations this plan is. Never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.folders.len()
    }

    /// Always `false` — a plan with no folder is not constructible. Present
    /// because `len` is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    /// The argv for each folder, in plan order. PURE — it builds commands; it
    /// never runs one, and each is a separate, non-atomic invocation whose
    /// result is recorded on its own ([`ResetAttempt`]).
    #[must_use]
    pub fn commands(&self) -> Vec<(Folder, Vec<String>)> {
        self.folders
            .iter()
            .map(|folder| (*folder, tccutil_reset_command(&self.bundle_id, *folder)))
            .collect()
    }
}

/// What one `tccutil reset` invocation did to one folder's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetStatus {
    /// Exited 0: this folder's row is gone, so macOS can ask about it again.
    Reset,
    /// Ran and exited nonzero — this folder's row is UNCHANGED. `code` is the
    /// exit status the panel names in the one line this folder gets; `None`
    /// when the process was killed by a signal and there is no code.
    Declined {
        /// The process exit status, or `None` for a signal death.
        code: Option<i32>,
    },
    /// Not run: [`TCCUTIL_PATH`] is missing or not executable. Distinct from
    /// [`ResetStatus::Declined`] — nothing was asked, so nothing was declined,
    /// and the panel must not say macOS refused anything.
    ToolAbsent,
}

impl ResetStatus {
    /// The report token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Declined { .. } => "declined",
            Self::ToolAbsent => "tool-absent",
        }
    }

    /// Whether this folder's row actually changed.
    #[must_use]
    pub const fn is_reset(self) -> bool {
        matches!(self, Self::Reset)
    }

    /// Classify a finished invocation from `ExitStatus::code()`: `Some(0)` is
    /// the only success, and `None` (a signal) is a decline with no code.
    #[must_use]
    pub const fn from_exit_status(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Reset,
            Some(other) => Self::Declined { code: Some(other) },
            None => Self::Declined { code: None },
        }
    }

    /// [`Self::from_exit_status`] for a caller that already has a code.
    #[must_use]
    pub const fn from_exit_code(code: i32) -> Self {
        Self::from_exit_status(Some(code))
    }
}

/// One folder's independently recorded result. The gesture is three separate
/// processes and is not atomic, so there is no single status to fold into
/// until every attempt is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResetAttempt {
    /// The folder this invocation named.
    pub folder: Folder,
    /// What that invocation did.
    pub status: ResetStatus,
}

impl ResetAttempt {
    /// Record one attempt.
    #[must_use]
    pub const fn new(folder: Folder, status: ResetStatus) -> Self {
        Self { folder, status }
    }

    /// Record a finished invocation from `ExitStatus::code()`.
    #[must_use]
    pub const fn from_exit_status(folder: Folder, code: Option<i32>) -> Self {
        Self::new(folder, ResetStatus::from_exit_status(code))
    }

    /// Record an invocation that could not be made because the tool is not
    /// there.
    #[must_use]
    pub const fn tool_absent(folder: Folder) -> Self {
        Self::new(folder, ResetStatus::ToolAbsent)
    }

    /// Whether this folder's row changed.
    #[must_use]
    pub const fn is_reset(self) -> bool {
        self.status.is_reset()
    }
}

/// The folded result of ONE *Ask again* gesture.
///
/// The distinction between [`Self::AllReset`] and [`Self::Partial`] is the
/// whole point of folding rather than counting: §3.7 forbids the panel from
/// claiming every folder reset because one did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOutcome {
    /// Every invocation exited 0.
    AllReset,
    /// At least one folder reset and at least one did not. Reported as a
    /// PARTIAL success, naming the folders that failed and no others.
    Partial,
    /// At least one invocation ran and no folder reset. No row changed.
    NoneReset,
    /// Nothing ran at all: [`TCCUTIL_PATH`] is missing or not executable. §3.7:
    /// the button is not shown in the first place.
    ToolAbsent,
    /// There was nothing to attempt. Not a success, and never rendered as one.
    NotAttempted,
}

impl ResetOutcome {
    /// The report token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllReset => "all-reset",
            Self::Partial => "partial",
            Self::NoneReset => "none-reset",
            Self::ToolAbsent => "tool-absent",
            Self::NotAttempted => "not-attempted",
        }
    }

    /// Whether any folder's row changed — the gate on offering the warm-up
    /// afterwards.
    #[must_use]
    pub const fn any_reset(self) -> bool {
        matches!(self, Self::AllReset | Self::Partial)
    }

    /// Whether EVERY attempted folder reset. The only state in which a string
    /// may speak about the whole set.
    #[must_use]
    pub const fn all_reset(self) -> bool {
        matches!(self, Self::AllReset)
    }
}

/// Fold the per-invocation results of one gesture into one outcome. PURE and
/// total.
///
/// * no attempts ⇒ [`ResetOutcome::NotAttempted`];
/// * every attempt [`ResetStatus::ToolAbsent`] ⇒ [`ResetOutcome::ToolAbsent`],
///   because absence is a property of the machine, not of a folder;
/// * every attempt reset ⇒ [`ResetOutcome::AllReset`];
/// * some reset ⇒ [`ResetOutcome::Partial`];
/// * none reset, at least one having run ⇒ [`ResetOutcome::NoneReset`].
///
/// Nothing here retries: the fold reports what happened once.
#[must_use]
pub fn reset_outcome(attempts: &[ResetAttempt]) -> ResetOutcome {
    if attempts.is_empty() {
        return ResetOutcome::NotAttempted;
    }
    let reset = attempts.iter().filter(|a| a.is_reset()).count();
    if reset == attempts.len() {
        return ResetOutcome::AllReset;
    }
    if reset > 0 {
        return ResetOutcome::Partial;
    }
    if attempts
        .iter()
        .all(|a| matches!(a.status, ResetStatus::ToolAbsent))
    {
        return ResetOutcome::ToolAbsent;
    }
    ResetOutcome::NoneReset
}

/// The folders whose rows actually changed — exactly the set the warm-up is
/// offered for afterwards, and no other (§3.7).
#[must_use]
pub fn folders_reset(attempts: &[ResetAttempt]) -> Vec<Folder> {
    attempts
        .iter()
        .filter(|a| a.is_reset())
        .map(|a| a.folder)
        .collect()
}

/// The folders macOS declined, each with the exit status to name in its one
/// line. A [`ResetStatus::ToolAbsent`] attempt is NOT here: nothing was asked,
/// so nothing declined.
#[must_use]
pub fn folders_declined(attempts: &[ResetAttempt]) -> Vec<(Folder, Option<i32>)> {
    attempts
        .iter()
        .filter_map(|a| match a.status {
            ResetStatus::Declined { code } => Some((a.folder, code)),
            ResetStatus::Reset | ResetStatus::ToolAbsent => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Attribution and the posture join
// ---------------------------------------------------------------------------

/// Whether a session's file access is attributed to the live aterm process or
/// to one that has exited.
///
/// This is aterm's OWN adoption record — the successor's adoption path knows
/// exactly which sessions it took over. [`Responsible`] only corroborates it,
/// and never overrides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Attribution {
    /// Spawned by the process that is running now.
    Live,
    /// Adopted across an in-place apply: this session outlived the process
    /// that started it.
    Adopted,
    /// No adoption record. Not a claim in either direction.
    #[default]
    Unknown,
}

impl Attribution {
    /// The `attribution=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Adopted => "adopted",
            Self::Unknown => "unknown",
        }
    }
}

/// A session's file-consent verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FsConsent {
    /// Full Disk Access is held, the session is live, and the service was
    /// measured to be covered. Requires evidence on all three counts.
    Covered,
    /// aterm itself observed an `EPERM(1)` for this session.
    Denied,
    /// Everything else — including every case where a claim would be an
    /// inference rather than an observation.
    #[default]
    Unknown,
}

impl FsConsent {
    /// The `fs_consent=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }
}

/// Which blocking measurements from design §7 have actually been made.
///
/// Every field is `false`/`Unknown` on this tree — S1, S2 and S4 have not been
/// run — and the join is written so that flipping one of these is the ONLY way
/// a stronger claim can appear in a report. That is deliberate: it makes an
/// over-claim a code change a reviewer can see, rather than a sentence that
/// drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpikeEvidence {
    /// §7 S4 — how much a held Full Disk Access grant actually suppresses.
    /// Until this is `true`, no folder is ever reported `covered`.
    pub fda_coverage_measured: bool,
    /// §7 S2 — what an adopted-across-a-handoff session's attribution and
    /// access actually do. Until this is `true`, an adopted session reports
    /// `unknown` and NEVER `denied`.
    pub handoff_attribution_measured: bool,
    /// §7 S1 — how far a grant reaches. Stays [`FdaScope::Unknown`].
    pub fda_scope: FdaScope,
}

impl SpikeEvidence {
    /// Nothing measured — the state of this tree, and what every caller passes
    /// today.
    pub const UNMEASURED: Self = Self {
        fda_coverage_measured: false,
        handoff_attribution_measured: false,
        fda_scope: FdaScope::Unknown,
    };
}

impl Default for SpikeEvidence {
    fn default() -> Self {
        Self::UNMEASURED
    }
}

/// Everything the posture join reads. All of it is observed, none of it is
/// inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PostureInputs {
    /// aterm's own adoption record for this session.
    pub adoption: Attribution,
    /// The instance's Full Disk Access state.
    pub fda: FdaState,
    /// The responsibility SPI's corroboration. Informational: it never changes
    /// [`ConsentPosture::attribution`].
    pub responsible: Responsible,
    /// `true` only when aterm ITSELF took an `EPERM(1)` on a path under a
    /// protected root for this session. Never set from a guess.
    pub observed_eperm: bool,
    /// Which spikes have landed.
    pub evidence: SpikeEvidence,
}

impl Default for PostureInputs {
    fn default() -> Self {
        Self {
            adoption: Attribution::Unknown,
            fda: FdaState::Unknown,
            responsible: Responsible::Unknown,
            observed_eperm: false,
            evidence: SpikeEvidence::UNMEASURED,
        }
    }
}

/// The joined posture for one session. Pure product of [`PostureInputs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConsentPosture {
    /// Copied verbatim from the adoption record.
    pub attribution: Attribution,
    /// The joined verdict.
    pub fs_consent: FsConsent,
    /// The corroboration, carried through unchanged.
    pub responsible: Responsible,
    /// The instance Full Disk Access state, carried through unchanged.
    pub fda: FdaState,
    /// How far the grant reaches. [`FdaScope::Unknown`] until §7 S1.
    pub fda_scope: FdaScope,
    /// `true` when a macOS consent dialog remains possible for this session.
    pub prompt_possible: bool,
}

impl ConsentPosture {
    /// Join the observations. Pure, total, and table-tested.
    ///
    /// * `attribution` is the adoption record, verbatim. The responsibility
    ///   SPI never overrides it — it can be `Unknown` for reasons that have
    ///   nothing to do with adoption (it returns `-1` for any process the
    ///   caller does not own).
    /// * `fs_consent` is `Denied` only on an observed `EPERM`; `Covered` only
    ///   when the grant is held, the session is live, AND §7 S4 measured the
    ///   coverage; `Unknown` otherwise.
    /// * An adopted session is `Unknown` and never `Denied` until §7 S2 lands
    ///   (design §3.9 pt 3): aterm reports that the session outlived the
    ///   process that started it, and refuses to assert the TCC consequence.
    /// * `prompt_possible` is `false` only when the grant is held AND the
    ///   coverage was measured. A held grant whose breadth is unmeasured
    ///   leaves a prompt possible, and says so.
    #[must_use]
    pub const fn join(inputs: PostureInputs) -> Self {
        let attribution = inputs.adoption;
        let adopted_unproven = matches!(attribution, Attribution::Adopted)
            && !inputs.evidence.handoff_attribution_measured;

        let fs_consent = if adopted_unproven {
            FsConsent::Unknown
        } else if inputs.observed_eperm {
            FsConsent::Denied
        } else if matches!(inputs.fda, FdaState::Granted)
            && matches!(attribution, Attribution::Live)
            && inputs.evidence.fda_coverage_measured
        {
            FsConsent::Covered
        } else {
            FsConsent::Unknown
        };

        let prompt_possible =
            !(matches!(inputs.fda, FdaState::Granted) && inputs.evidence.fda_coverage_measured);

        Self {
            attribution,
            fs_consent,
            responsible: inputs.responsible,
            fda: inputs.fda,
            fda_scope: inputs.evidence.fda_scope,
            prompt_possible,
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// What a cached probe is keyed on: the bundle it was taken for, and the
/// designated requirement that bundle presented.
///
/// The DR is part of the key because a grant is bound to it. A rebuild that
/// changes the identity must invalidate a cached `granted`, and keying on the
/// path alone would not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConsentKey {
    /// The `.app` bundle root, or the executable path when there is no bundle.
    pub bundle: PathBuf,
    /// The designated requirement string, verbatim.
    pub dr: String,
}

impl ConsentKey {
    /// A key.
    #[must_use]
    pub fn new(bundle: impl Into<PathBuf>, dr: impl Into<String>) -> Self {
        Self {
            bundle: bundle.into(),
            dr: dr.into(),
        }
    }
}

/// A cached probe, with how old it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedProbe {
    /// The probe.
    pub probe: FdaProbe,
    /// Age at lookup — the `probe_age_ms=` field.
    pub age: Duration,
}

#[derive(Debug)]
struct CacheEntry {
    key: ConsentKey,
    probe: FdaProbe,
    at: Instant,
}

/// A consent probe cache with a caller-supplied freshness interval.
///
/// **Owned by the instance, never a process-global.** A successor that adopts
/// sessions across an in-place apply constructs its own, which starts empty
/// and re-probes on first demand, so a stale `granted` can never survive an
/// apply that changed the identity. There is deliberately no `static` here for
/// a successor to inherit.
///
/// Interior mutability, so a `&self` holder (the GUI's App, the CLI's report)
/// can read through it. [`ConsentCache::clear`] is the app-activation and
/// post-handoff hook.
#[derive(Debug, Default)]
pub struct ConsentCache {
    entry: Mutex<Option<CacheEntry>>,
}

impl ConsentCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything. Called on app activation (so a grant made while aterm
    /// was in the background is seen) and whenever the identity may have
    /// moved.
    pub fn clear(&self) {
        *self.lock() = None;
    }

    /// `true` when nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_none()
    }

    /// The cached probe for `key`, if one is present and younger than
    /// `interval`. A different key is always a miss.
    #[must_use]
    pub fn get(&self, key: &ConsentKey, interval: Duration) -> Option<CachedProbe> {
        self.get_at(key, interval, Instant::now())
    }

    /// [`ConsentCache::get`] with the clock injected.
    #[must_use]
    pub fn get_at(
        &self,
        key: &ConsentKey,
        interval: Duration,
        now: Instant,
    ) -> Option<CachedProbe> {
        let guard = self.lock();
        let entry = guard.as_ref()?;
        if entry.key != *key {
            return None;
        }
        let age = now.saturating_duration_since(entry.at);
        (age < interval).then_some(CachedProbe {
            probe: entry.probe,
            age,
        })
    }

    /// Record a probe against `key`.
    pub fn store(&self, key: ConsentKey, probe: FdaProbe) {
        self.store_at(key, probe, Instant::now());
    }

    /// [`ConsentCache::store`] with the clock injected.
    pub fn store_at(&self, key: ConsentKey, probe: FdaProbe, now: Instant) {
        *self.lock() = Some(CacheEntry {
            key,
            probe,
            at: now,
        });
    }

    /// The cached probe for `key`, or `probe()`'s answer, stored and returned.
    ///
    /// The probe closure is called at most once, and only on a miss.
    pub fn get_or_probe<F>(&self, key: &ConsentKey, interval: Duration, probe: F) -> CachedProbe
    where
        F: FnOnce() -> FdaProbe,
    {
        self.get_or_probe_at(key, interval, Instant::now(), probe)
    }

    /// [`ConsentCache::get_or_probe`] with the clock injected.
    pub fn get_or_probe_at<F>(
        &self,
        key: &ConsentKey,
        interval: Duration,
        now: Instant,
        probe: F,
    ) -> CachedProbe
    where
        F: FnOnce() -> FdaProbe,
    {
        if let Some(hit) = self.get_at(key, interval, now) {
            return hit;
        }
        let fresh = probe();
        self.store_at(key.clone(), fresh, now);
        CachedProbe {
            probe: fresh,
            age: Duration::ZERO,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<CacheEntry>> {
        // A poisoned consent cache is a stale probe, not a corrupted one:
        // recovering the value is strictly better than panicking in a probe.
        self.entry.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// ---------------------------------------------------------------------------
// macOS implementation — the only code here that touches the OS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        INFO_PLIST_MAX_BYTES, ProbeOutcome, ResponsibleApp, ResponsibleError, display_name,
        readable_info_plist, responsible_answer,
    };
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    // `proc_pidpath` is in `libproc`, which is part of `libSystem`; it links
    // directly. Only the responsibility SPI needs `dlsym`.
    unsafe extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }

    /// `PROC_PIDPATHINFO_MAXSIZE` — `4 * MAXPATHLEN`.
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;

    /// No SDK header declares this; LLDB, Qt Creator and Chromium all declare
    /// it themselves. It lives in `/usr/lib/system/libquarantine.dylib`.
    type ResponsibilityGetPid = unsafe extern "C" fn(libc::c_int) -> libc::c_int;

    const RESPONSIBILITY_SYMBOL: &[u8] = b"responsibility_get_pid_responsible_for_pid\0";

    /// The ONE probe syscall: open, close, never read.
    pub(super) fn open_probe(db: &Path) -> ProbeOutcome {
        let Ok(path) = CString::new(db.as_os_str().as_bytes()) else {
            // Unreachable: the caller already rejected interior NULs. Report
            // an errno rather than panicking in a probe.
            return ProbeOutcome::Errno(libc::EINVAL);
        };
        // SAFETY: `path` is a valid NUL-terminated C string that outlives the
        // call; `O_RDONLY` takes no third argument.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            return ProbeOutcome::Errno(errno());
        }
        // SAFETY: `fd` is a descriptor this call just created and no longer
        // uses. The contents are never read.
        unsafe {
            libc::close(fd);
        }
        ProbeOutcome::Ok
    }

    /// `errno` for the immediately preceding call.
    fn errno() -> i32 {
        // SAFETY: `__error()` returns a valid pointer to this thread's errno.
        unsafe { *libc::__error() }
    }

    /// Resolve the responsibility SPI, or `None`.
    ///
    /// Resolved per call rather than cached in a `static`: the call is not on
    /// any hot path (it runs at most once per probe interval per session), and
    /// this crate deliberately keeps lazy-init statics out of a path a GUI
    /// main thread can reach.
    fn responsibility_symbol() -> Option<ResponsibilityGetPid> {
        // SAFETY: `RESPONSIBILITY_SYMBOL` is NUL-terminated; `RTLD_DEFAULT` is
        // the documented pseudo-handle for a global search.
        let sym = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                RESPONSIBILITY_SYMBOL.as_ptr().cast::<libc::c_char>(),
            )
        };
        if sym.is_null() {
            return None;
        }
        // SAFETY: dyld resolved this name to the libquarantine function whose
        // C prototype is `pid_t f(pid_t)`, which is exactly
        // `ResponsibilityGetPid`.
        Some(unsafe { std::mem::transmute::<*mut libc::c_void, ResponsibilityGetPid>(sym) })
    }

    pub(super) fn responsible_pid_detailed(pid: i32) -> Result<i32, ResponsibleError> {
        let Some(func) = responsibility_symbol() else {
            return Err(ResponsibleError::SymbolUnavailable);
        };
        // `-1` is overloaded, so errno is the only way to tell "not ours"
        // from "gone"; clear it first so a stale value cannot be read back.
        // SAFETY: `__error()` returns a valid pointer to this thread's errno.
        unsafe {
            *libc::__error() = 0;
        }
        // SAFETY: `func` has the signature dyld resolved it to, and the
        // argument is a plain `pid_t`. The call is a single `__mac_syscall`
        // into the kernel Quarantine policy: no XPC, no Mach, no tccd, no
        // WindowServer.
        let ret = unsafe { func(pid) };
        responsible_answer(ret, errno())
    }

    pub(super) fn responsible_app_with_roots(
        pid: i32,
        protected: &[PathBuf],
    ) -> Option<ResponsibleApp> {
        let responsible = super::responsible_pid(pid)?;
        let path = pid_path(responsible)?;
        let plist = readable_info_plist(&path, protected).and_then(|p| read_info_plist(&p));
        let name = display_name(&path, plist.as_deref());
        Some(ResponsibleApp {
            pid: responsible,
            path,
            display_name: name,
        })
    }

    /// `proc_pidpath` for one pid.
    fn pid_path(pid: i32) -> Option<PathBuf> {
        let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `buf` is a live allocation of exactly the length passed, and
        // `proc_pidpath` writes at most that many bytes.
        let written = unsafe {
            proc_pidpath(
                pid,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
            )
        };
        let written = usize::try_from(written).ok()?;
        if written == 0 || written > buf.len() {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(&buf[..written])))
    }

    /// A bounded read of an `Info.plist` the guard already cleared. Anything
    /// over the cap, unreadable, or not UTF-8 reads as absent.
    fn read_info_plist(path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
            return None;
        }
        std::fs::read_to_string(path).ok()
    }
}

/// The bundle half of a [`ConsentKey`] for an executable: its `.app` root when
/// it has one, else the executable path itself.
#[must_use]
pub fn cache_bundle_for(exe: &Path) -> PathBuf {
    app_bundle_root(exe).unwrap_or_else(|| exe.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // -- the fence -----------------------------------------------------------

    #[test]
    fn out_of_bundle_paths_are_not_in_a_bundle() {
        for path in [
            "/Users//a/aterm/target/debug/deps/aterm_containment-abc123",
            "/Users//a/aterm/target/debug/aterm-gui",
            "/usr/local/bin/aterm",
            "/Applications/aterm.app",
            "/Applications/aterm.app/Contents",
            "/Applications/aterm.app/Contents/MacOS",
            "/Applications/.app/Contents/MacOS/aterm",
            "/Applications/aterm.app/Contents/Resources/aterm",
            "relative/Contents/MacOS/aterm",
        ] {
            assert!(
                !path_is_in_app_bundle(Path::new(path)),
                "must not read as in-bundle: {path}"
            );
        }
        // A relative path with a real bundle shape still resolves; the guard is
        // lexical and does not require an absolute path.
        assert!(path_is_in_app_bundle(Path::new(
            "sub/aterm.app/Contents/MacOS/aterm"
        )));
    }

    #[test]
    fn in_bundle_paths_resolve_to_their_bundle_root() {
        let cases = [
            (
                "/Applications/aterm.app/Contents/MacOS/aterm",
                "/Applications/aterm.app",
            ),
            (
                "/Applications/aterm (dev).app/Contents/MacOS/aterm",
                "/Applications/aterm (dev).app",
            ),
            (
                "/Applications/iTerm.app/Contents/MacOS/iTerm2",
                "/Applications/iTerm.app",
            ),
            (
                "/Applications/aterm.app/Contents/MacOS/helpers/aterm",
                "/Applications/aterm.app",
            ),
            // Nested bundles: the INNERMOST enclosing .app wins.
            (
                "/Applications/Outer.app/Contents/MacOS/Inner.app/Contents/MacOS/x",
                "/Applications/Outer.app/Contents/MacOS/Inner.app",
            ),
        ];
        for (exe, root) in cases {
            assert_eq!(
                app_bundle_root(Path::new(exe)),
                Some(p(root)),
                "bundle root for {exe}"
            );
        }
    }

    /// The load-bearing safety test: an out-of-bundle caller gets `Unknown`
    /// and the syscall is never reached. The injected closure panics, so a
    /// regression that moves the guard below the syscall fails here loudly.
    #[test]
    fn probe_refuses_out_of_bundle_without_touching_the_os() {
        let deps = p("/Users//a/aterm/target/debug/deps/aterm_containment-abc123");
        let home = p("/Users//a");
        let got = probe_fda_with(ProbeGate::on(), Some(&deps), Some(&home), |_| {
            panic!("the FDA probe performed a syscall from outside a .app bundle");
        });
        assert_eq!(got.state, FdaState::Unknown);
        assert_eq!(got.label, ProbeLabel::RefusedOutOfBundle);
        assert!(got.label.refused());
    }

    #[test]
    fn probe_refuses_when_disabled_or_unresolvable_without_touching_the_os() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let home = p("/Users//a");
        let boom = |_: &Path| -> ProbeOutcome { panic!("probe performed a syscall") };

        let off = ProbeGate {
            enabled: false,
            check: true,
        };
        assert_eq!(
            probe_fda_with(off, Some(&bundled), Some(&home), boom).label,
            ProbeLabel::RefusedDisabled
        );
        let no_check = ProbeGate {
            enabled: true,
            check: false,
        };
        assert_eq!(
            probe_fda_with(no_check, Some(&bundled), Some(&home), boom).label,
            ProbeLabel::RefusedDisabled
        );
        assert_eq!(
            probe_fda_with(ProbeGate::on(), None, Some(&home), boom).label,
            ProbeLabel::RefusedNoExe
        );
        assert_eq!(
            probe_fda_with(ProbeGate::on(), Some(&bundled), None, boom).label,
            ProbeLabel::RefusedNoHome
        );
        // Every refusal is Unknown — never Denied.
        for gate in [off, no_check] {
            assert_eq!(
                probe_fda_with(gate, Some(&bundled), Some(&home), boom).state,
                FdaState::Unknown
            );
        }
    }

    #[test]
    fn probe_reaches_the_syscall_exactly_once_from_inside_a_bundle() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let home = p("/Users//a");
        let mut seen: Option<PathBuf> = None;
        let got = probe_fda_with(ProbeGate::on(), Some(&bundled), Some(&home), |db| {
            seen = Some(db.to_path_buf());
            ProbeOutcome::Ok
        });
        assert_eq!(got.state, FdaState::Granted);
        assert_eq!(got.label, ProbeLabel::OpenOk);
        assert_eq!(
            seen,
            Some(p(
                "/Users//a/Library/Application Support/com.apple.TCC/TCC.db"
            ))
        );
    }

    #[test]
    fn probe_classification_is_a_table() {
        let cases = [
            (ProbeOutcome::Ok, FdaState::Granted, ProbeLabel::OpenOk),
            (
                ProbeOutcome::Errno(ERRNO_EPERM),
                FdaState::Denied,
                ProbeLabel::OpenEperm,
            ),
            // EACCES is NOT a TCC denial.
            (
                ProbeOutcome::Errno(13),
                FdaState::Unknown,
                ProbeLabel::OpenErrno(13),
            ),
            // ENOENT.
            (
                ProbeOutcome::Errno(2),
                FdaState::Unknown,
                ProbeLabel::OpenErrno(2),
            ),
            // EINTR.
            (
                ProbeOutcome::Errno(4),
                FdaState::Unknown,
                ProbeLabel::OpenErrno(4),
            ),
        ];
        for (outcome, state, label) in cases {
            let got = classify_probe(outcome);
            assert_eq!(got.state, state, "state for {outcome:?}");
            assert_eq!(got.label, label, "label for {outcome:?}");
        }
    }

    #[test]
    fn refusal_labels_all_report_no_syscall() {
        for label in [
            ProbeLabel::RefusedOutOfBundle,
            ProbeLabel::RefusedNoExe,
            ProbeLabel::RefusedDisabled,
            ProbeLabel::RefusedNoHome,
            ProbeLabel::RefusedBadPath,
            ProbeLabel::UnsupportedPlatform,
        ] {
            assert!(label.refused(), "{label:?} must report no syscall");
        }
        for label in [
            ProbeLabel::OpenOk,
            ProbeLabel::OpenEperm,
            ProbeLabel::OpenErrno(9),
        ] {
            assert!(!label.refused(), "{label:?} performed a syscall");
        }
    }

    // -- the stub ------------------------------------------------------------

    #[test]
    fn the_non_macos_answer_is_unknown() {
        let stub = unsupported_probe();
        assert_eq!(stub.state, FdaState::Unknown);
        assert_eq!(stub.label, ProbeLabel::UnsupportedPlatform);
        assert!(stub.label.refused());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_every_os_wrapper_answers_unknown() {
        assert_eq!(probe_fda(ProbeGate::on()).state, FdaState::Unknown);
        assert_eq!(
            probe_fda(ProbeGate::on()).label,
            ProbeLabel::UnsupportedPlatform
        );
        assert_eq!(responsible_pid(1), None);
        assert_eq!(
            responsible_pid_detailed(1),
            Err(ResponsibleError::Unsupported)
        );
        assert_eq!(responsible_app(1), None);
        assert_eq!(responsible_app_with_roots(1, &[]), None);
    }

    // -- responsibility ------------------------------------------------------

    #[test]
    fn responsible_answers_are_a_table() {
        let cases: [(i32, i32, Result<i32, ResponsibleError>); 5] = [
            (4711, 0, Ok(4711)),
            (0, 0, Ok(0)),
            (-1, ERRNO_EPERM, Err(ResponsibleError::NotOurs)),
            (-1, ERRNO_ESRCH, Err(ResponsibleError::Gone)),
            (-1, 22, Err(ResponsibleError::Errno(22))),
        ];
        for (ret, err, want) in cases {
            assert_eq!(responsible_answer(ret, err), want, "ret={ret} errno={err}");
        }
    }

    #[test]
    fn minus_one_never_becomes_self() {
        let me = 4711;
        for err in [
            ResponsibleError::NotOurs,
            ResponsibleError::SymbolUnavailable,
            ResponsibleError::Errno(22),
            ResponsibleError::Unsupported,
        ] {
            assert_eq!(
                classify_responsible(me, Err(err)),
                Responsible::Unknown,
                "{err:?} must be Unknown, never SelfProcess"
            );
        }
        assert_eq!(
            classify_responsible(me, Err(ResponsibleError::Gone)),
            Responsible::Exited
        );
        assert_eq!(classify_responsible(me, Ok(me)), Responsible::SelfProcess);
        assert_eq!(classify_responsible(me, Ok(4402)), Responsible::Other(4402));
        // A zero or negative "success" identifies nothing.
        assert_eq!(classify_responsible(me, Ok(0)), Responsible::Unknown);
        assert_eq!(classify_responsible(me, Ok(-1)), Responsible::Unknown);
    }

    #[test]
    fn responsible_tokens_render() {
        assert_eq!(Responsible::SelfProcess.token(), Some("self"));
        assert_eq!(Responsible::Exited.token(), Some("exited"));
        assert_eq!(Responsible::Unknown.token(), Some("unknown"));
        assert_eq!(Responsible::Other(42).token(), None);
        assert_eq!(Responsible::Other(42).pid(), Some(42));
        assert_eq!(Responsible::SelfProcess.pid(), None);
    }

    // -- responsible_app's Info.plist guard ----------------------------------

    #[test]
    fn info_plist_under_a_protected_root_is_refused_and_falls_back_to_the_basename() {
        let home = p("/Users//a");
        let protected = protected_roots_in(Some(&home), &[]);
        assert!(
            protected.contains(&p("/Users//a/Documents")),
            "PRIVATE_SUBDIRS must supply ~/Documents"
        );

        // An app inside ~/Documents: reading its Info.plist is the very
        // syscall this design exists to avoid.
        let evil = p("/Users//a/Documents/Sketchy.app/Contents/MacOS/Sketchy");
        assert_eq!(readable_info_plist(&evil, &protected), None);
        assert_eq!(display_name(&evil, None), "Sketchy");

        // Same shape under other protected roots.
        for root in ["Desktop", "Downloads", "Library/Messages"] {
            let exe = home.join(root).join("X.app/Contents/MacOS/X");
            assert_eq!(
                readable_info_plist(&exe, &protected),
                None,
                "must refuse under {root}"
            );
        }

        // An app outside every protected root is fine to read.
        let ok = p("/Applications/iTerm.app/Contents/MacOS/iTerm2");
        assert_eq!(
            readable_info_plist(&ok, &protected),
            Some(p("/Applications/iTerm.app/Contents/Info.plist"))
        );

        // A non-bundle executable has no Info.plist to read at all.
        assert_eq!(readable_info_plist(Path::new("/bin/zsh"), &protected), None);
        assert_eq!(display_name(Path::new("/bin/zsh"), None), "zsh");
    }

    #[test]
    fn display_name_prefers_display_then_name_then_basename() {
        let exe = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let both = "<plist><dict>\
            <key>CFBundleName</key><string>aterm-short</string>\
            <key>CFBundleDisplayName</key><string>aterm (dev)</string>\
            </dict></plist>";
        assert_eq!(display_name(&exe, Some(both)), "aterm (dev)");

        let name_only = "<plist><dict><key>CFBundleName</key><string>aterm</string></dict></plist>";
        assert_eq!(display_name(&exe, Some(name_only)), "aterm");

        let neither = "<plist><dict><key>CFBundleIdentifier</key><string>com.aterm.aterm</string></dict></plist>";
        assert_eq!(display_name(&exe, Some(neither)), "aterm");

        // A binary plist reads as absent — basename.
        assert_eq!(display_name(&exe, Some("bplist00\u{0}\u{1}")), "aterm");
        // An empty value is not a name.
        let empty = "<plist><dict><key>CFBundleDisplayName</key><string></string></dict></plist>";
        assert_eq!(display_name(&exe, Some(empty)), "aterm");
        // A key whose value element is not a string is not mis-bound: the
        // lookup fails for that key and falls through to the next one.
        let wrong = "<plist><dict><key>CFBundleDisplayName</key><true/><key>CFBundleName</key><string>ok</string></dict></plist>";
        assert_eq!(display_name(&exe, Some(wrong)), "ok");
        // With neither key carrying a string, the basename stands.
        let both_wrong = "<plist><dict><key>CFBundleDisplayName</key><true/><key>CFBundleName</key><false/></dict></plist>";
        assert_eq!(display_name(&exe, Some(both_wrong)), "aterm");
    }

    #[test]
    fn display_name_does_not_bind_a_prefix_key() {
        let exe = p("/Applications/aterm.app/Contents/MacOS/aterm");
        // `CFBundleNameSuffix` must not satisfy a lookup for `CFBundleName`.
        let text = "<plist><dict><key>CFBundleNameSuffix</key><string>nope</string></dict></plist>";
        assert_eq!(display_name(&exe, Some(text)), "aterm");
    }

    // -- designated requirement ---------------------------------------------

    #[test]
    fn dr_classification_is_a_table() {
        let cases = [
            (
                "designated => identifier \"com.aterm.aterm\" and anchor apple generic and \
                 certificate 1[field.1.2.840.113635.100.6.2.6] and \
                 certificate leaf[field.1.2.840.113635.100.6.1.13] and \
                 certificate leaf[subject.OU] = \"A66A9P66Z7\"",
                DrClass::Identity,
            ),
            (
                "identifier \"com.aterm.aterm.dev\" and certificate leaf H\"deadbeef\"",
                DrClass::Identity,
            ),
            (
                "designated => cdhash H\"0123456789abcdef\"",
                DrClass::Cdhash,
            ),
            ("cdhash H\"abc\" or cdhash H\"def\"", DrClass::Cdhash),
            // A cdhash pin invalidates the next build whatever else it says.
            (
                "identifier \"com.aterm.aterm\" and cdhash H\"abc\"",
                DrClass::Cdhash,
            ),
            (
                "test-requirement: code object is not signed at all",
                DrClass::Unsigned,
            ),
            ("code object is not signed at all", DrClass::Unsigned),
            ("", DrClass::Unknown),
            ("   ", DrClass::Unknown),
            ("anchor apple", DrClass::Unknown),
            ("identifier \"com.aterm.aterm\"", DrClass::Unknown),
            ("nonsense", DrClass::Unknown),
        ];
        for (input, want) in cases {
            assert_eq!(classify_dr(input), want, "classifying {input:?}");
        }
    }

    #[test]
    fn only_an_identity_requirement_is_grant_stable() {
        assert!(DrClass::Identity.grant_stable());
        for class in [DrClass::Cdhash, DrClass::Unsigned, DrClass::Unknown] {
            assert!(!class.grant_stable(), "{class:?} must not be grant-stable");
        }
        assert_eq!(DrClass::Identity.as_str(), "identity");
        assert_eq!(DrClass::Cdhash.as_str(), "cdhash");
        assert_eq!(DrClass::Unsigned.as_str(), "unsigned");
        assert_eq!(DrClass::Unknown.as_str(), "unknown");
    }

    // -- protected roots -----------------------------------------------------

    #[test]
    fn empty_overrides_resolve_to_private_subdirs() {
        let home = p("/Users//a");
        let roots = protected_roots_in(Some(&home), &[]);
        assert_eq!(roots.len(), crate::sbpl::PRIVATE_SUBDIRS.len());
        for sub in crate::sbpl::PRIVATE_SUBDIRS {
            assert!(
                roots.contains(&home.join(sub)),
                "PRIVATE_SUBDIRS entry {sub} missing from the consent roots"
            );
        }
        // Containment and consent cannot disagree.
        assert_eq!(
            roots,
            crate::sbpl::PRIVATE_SUBDIRS
                .iter()
                .map(|s| home.join(s))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn overrides_replace_the_default_and_are_expanded() {
        let home = p("/Users//a");
        let overrides = [
            String::from("~/Documents"),
            String::from("/Volumes"),
            String::from("~"),
            String::from("  ~/Library/CloudStorage  "),
            // Neither absolute nor ~-prefixed: refused rather than guessed at.
            String::from("relative/path"),
            String::new(),
        ];
        let roots = protected_roots_in(Some(&home), &overrides);
        assert_eq!(
            roots,
            vec![
                p("/Users//a/Documents"),
                p("/Volumes"),
                p("/Users//a"),
                p("/Users//a/Library/CloudStorage"),
            ]
        );
    }

    #[test]
    fn no_home_means_no_default_roots_and_absolute_overrides_only() {
        assert!(protected_roots_in(None, &[]).is_empty());
        let overrides = [String::from("~/Documents"), String::from("/Volumes")];
        assert_eq!(protected_roots_in(None, &overrides), vec![p("/Volumes")]);
    }

    #[test]
    fn containment_is_a_prefix_test_over_whole_components() {
        let roots = vec![p("/Users//a/Documents")];
        assert!(is_under_protected_root(
            Path::new("/Users//a/Documents"),
            &roots
        ));
        assert!(is_under_protected_root(
            Path::new("/Users//a/Documents/x/y.app"),
            &roots
        ));
        // A sibling whose name merely starts with the root's name is NOT under it.
        assert!(!is_under_protected_root(
            Path::new("/Users//a/DocumentsOld/x"),
            &roots
        ));
        assert!(!is_under_protected_root(Path::new("/Users//a"), &roots));
        assert!(!is_under_protected_root(Path::new("/Applications"), &[]));
    }

    // -- folders -------------------------------------------------------------

    #[test]
    fn folder_names_services_and_paths() {
        let home = p("/Users//a");
        let cases = [
            (
                Folder::Documents,
                "documents",
                "SystemPolicyDocumentsFolder",
                "/Users//a/Documents",
            ),
            (
                Folder::Desktop,
                "desktop",
                "SystemPolicyDesktopFolder",
                "/Users//a/Desktop",
            ),
            (
                Folder::Downloads,
                "downloads",
                "SystemPolicyDownloadsFolder",
                "/Users//a/Downloads",
            ),
        ];
        for (folder, name, service, path) in cases {
            assert_eq!(folder.as_str(), name);
            assert_eq!(folder.tcc_service(), service);
            assert_eq!(folder.path(&home), p(path));
            assert_eq!(Folder::parse(name), Some(folder));
            assert_eq!(Folder::parse(&name.to_uppercase()), Some(folder));
        }
        assert_eq!(Folder::parse("pictures"), None);
        assert_eq!(Folder::parse(""), None);
        assert_eq!(Folder::ALL.len(), 3);
    }

    #[test]
    fn folder_paths_are_resolved_data_for_the_warmup() {
        let home = p("/Users//a");
        let names = [
            String::from("documents"),
            String::from("nope"),
            String::from("downloads"),
        ];
        assert_eq!(
            folder_paths_in(Some(&home), &names),
            vec![
                (Folder::Documents, p("/Users//a/Documents")),
                (Folder::Downloads, p("/Users//a/Downloads")),
            ]
        );
        assert!(folder_paths_in(None, &names).is_empty());
    }

    #[test]
    fn the_reset_command_is_built_from_the_running_bundle_id() {
        assert_eq!(
            tccutil_reset_command("com.aterm.aterm.dev", Folder::Documents),
            vec![
                "/usr/bin/tccutil",
                "reset",
                "SystemPolicyDocumentsFolder",
                "com.aterm.aterm.dev",
            ]
        );
        // Nothing in the builder is a literal bundle id: a different id in
        // gives a different id out, always in the last position.
        for id in ["com.aterm.aterm", "com.aterm.aterm.dev", "com.example.x"] {
            for folder in Folder::ALL {
                let argv = tccutil_reset_command(id, *folder);
                assert_eq!(argv[3], id);
                assert_eq!(argv[2], folder.tcc_service());
            }
        }
    }

    // -- denied-state repair (§3.7) ------------------------------------------

    #[test]
    fn tccutil_presence_is_a_table_and_only_executable_may_run() {
        let (run, notx, gone) = (
            TccutilPresence::Executable,
            TccutilPresence::NotExecutable,
            TccutilPresence::Missing,
        );
        let cases = [
            (true, true, run, "executable", true),
            (true, false, notx, "not-executable", false),
            (false, false, gone, "missing", false),
            // A directory (or anything that is not a regular file) at the path
            // is "no tool there", never "present but unusable".
            (false, true, gone, "missing", false),
        ];
        for (is_file, executable, want, token, can_run) in cases {
            let got = classify_tccutil(is_file, executable);
            assert_eq!(got, want, "is_file={is_file} executable={executable}");
            assert_eq!(got.as_str(), token);
            assert_eq!(got.can_run(), can_run);
        }
        // The default fails toward "do not offer a destructive button".
        assert_eq!(TccutilPresence::default(), TccutilPresence::Unknown);
        assert!(!TccutilPresence::Unknown.can_run());
        assert_eq!(TccutilPresence::Unknown.as_str(), "unknown");
    }

    #[test]
    fn a_usable_bundle_id_is_non_blank_flagless_and_whitespace_free() {
        for good in ["a.b.c", "  a.b.c  ", "x", "a-b_c.d"] {
            assert!(bundle_id_is_usable(good), "{good:?} should be usable");
        }
        for bad in ["", "   ", "\t\n", "-x", "a b", "a\tb", "a\u{0}b", "a\u{7}"] {
            assert!(!bundle_id_is_usable(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_reset_command_can_never_take_the_every_app_form() {
        // `tccutil reset <service>` with no subject resets the service for
        // EVERY app. The builder is structurally incapable of emitting it:
        // four arguments, always, with the caller's subject last.
        for id in ["a.b.c", "", "   ", "-not-a-flag"] {
            for folder in Folder::ALL {
                let argv = tccutil_reset_command(id, *folder);
                assert_eq!(argv.len(), 4, "{id:?} {folder:?}");
                assert_eq!(argv[0], TCCUTIL_PATH);
                assert_eq!(argv[1], "reset");
                assert_eq!(argv[2], folder.tcc_service());
                assert_eq!(argv[3], id);
            }
        }
    }

    #[test]
    fn no_bundle_id_literal_lives_outside_the_tests() {
        // The subject comes from the RUNNING bundle. If an aterm bundle id
        // ever appears in the shipping half of this module, some code path can
        // reset rows that are not its own — the dev channel reaching the
        // release's.
        let source = include_str!("consent.rs");
        let (shipping, _tests) = source
            .split_once("#[cfg(test)]\nmod tests {")
            .expect("the test module marker is in this file");
        assert!(
            !shipping.contains("com.aterm"),
            "an aterm bundle id literal appeared outside the tests"
        );
    }

    #[test]
    fn a_plan_refuses_an_unusable_subject_or_an_empty_folder_set() {
        assert!(ResetPlan::new("a.b.c", Folder::ALL).is_some());
        assert!(ResetPlan::new("", Folder::ALL).is_none());
        assert!(ResetPlan::new("   ", Folder::ALL).is_none());
        assert!(ResetPlan::new("-x", Folder::ALL).is_none());
        assert!(ResetPlan::new("a b", Folder::ALL).is_none());
        assert!(ResetPlan::new("a.b.c", &[]).is_none());
    }

    #[test]
    fn a_plan_is_one_command_per_folder_in_plan_order_from_one_subject() {
        let plan = ResetPlan::new(" a.b.c ", Folder::ALL).expect("plan builds");
        assert_eq!(plan.bundle_id(), "a.b.c", "the subject is trimmed once");
        assert_eq!(plan.len(), 3);
        assert!(!plan.is_empty());
        assert_eq!(plan.folders(), Folder::ALL);

        let commands = plan.commands();
        assert_eq!(commands.len(), 3);
        for (folder, argv) in &commands {
            assert_eq!(argv[2], folder.tcc_service());
            assert_eq!(argv[3], "a.b.c", "every invocation names the same subject");
        }
        assert_eq!(
            commands.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            vec![Folder::Documents, Folder::Desktop, Folder::Downloads]
        );
    }

    #[test]
    fn a_repeated_folder_never_becomes_a_second_invocation() {
        // No retry and no loop: a folder listed twice is still one command.
        let plan = ResetPlan::new(
            "a.b.c",
            &[
                Folder::Downloads,
                Folder::Documents,
                Folder::Downloads,
                Folder::Documents,
            ],
        )
        .expect("plan builds");
        assert_eq!(plan.folders(), &[Folder::Downloads, Folder::Documents]);
        assert_eq!(plan.commands().len(), 2);
    }

    fn offer_inputs(
        dr: DrClass,
        tccutil: TccutilPresence,
        bundle_id: Option<&str>,
    ) -> ResetOfferInputs<'_> {
        ResetOfferInputs {
            dr,
            tccutil,
            bundle_id,
        }
    }

    #[test]
    fn the_repair_offer_is_a_table() {
        let (run, notx, gone, unk) = (
            TccutilPresence::Executable,
            TccutilPresence::NotExecutable,
            TccutilPresence::Missing,
            TccutilPresence::Unknown,
        );
        let id = Some("a.b.c");
        let cases = [
            // A stable identity, a runnable tool and a known subject: offer.
            (DrClass::Identity, run, id, ResetOffer::Offer),
            // Not observed unstable is NOT observed unstable: still offered,
            // with no promise about how long a later grant lasts.
            (DrClass::Unknown, run, id, ResetOffer::Offer),
            // §3.7's named case: the grant died with the last build.
            (DrClass::Cdhash, run, id, ResetOffer::ExplainRebuild),
            // §3.1 says the same of an ad-hoc bundle in its own words.
            (DrClass::Unsigned, run, id, ResetOffer::ExplainRebuild),
            // The rebuild explanation is true whether or not the tool exists,
            // so it outranks both later checks.
            (DrClass::Cdhash, gone, id, ResetOffer::ExplainRebuild),
            (DrClass::Cdhash, run, None, ResetOffer::ExplainRebuild),
            // No runnable tool, no button at all.
            (DrClass::Identity, gone, id, ResetOffer::HideNoTool),
            (DrClass::Identity, notx, id, ResetOffer::HideNoTool),
            (DrClass::Identity, unk, id, ResetOffer::HideNoTool),
            // No subject, no button — and never a fallback literal.
            (DrClass::Identity, run, None, ResetOffer::HideNoBundleId),
            (DrClass::Identity, run, Some(""), ResetOffer::HideNoBundleId),
            (
                DrClass::Identity,
                run,
                Some("  "),
                ResetOffer::HideNoBundleId,
            ),
            (
                DrClass::Identity,
                run,
                Some("-x"),
                ResetOffer::HideNoBundleId,
            ),
        ];
        for (dr, tool, bundle, want) in cases {
            let got = reset_offer(offer_inputs(dr, tool, bundle));
            assert_eq!(got, want, "dr={dr:?} tool={tool:?} bundle={bundle:?}");
            assert_eq!(got.shows_button(), want == ResetOffer::Offer);
        }
        // The all-unknown default never offers a destructive action.
        assert_eq!(
            reset_offer(ResetOfferInputs::default()),
            ResetOffer::HideNoTool
        );
    }

    #[test]
    fn a_rebuild_scoped_identity_suppresses_the_button_entirely() {
        let got = reset_offer(offer_inputs(
            DrClass::Cdhash,
            TccutilPresence::Executable,
            Some("a.b.c"),
        ));
        assert_eq!(got, ResetOffer::ExplainRebuild);
        assert!(
            !got.shows_button(),
            "a reset that cannot hold is not offered"
        );
        assert!(!DrClass::Cdhash.grant_stable());
    }

    #[test]
    fn only_an_offered_repair_can_be_built() {
        // "The button is hidden" and "no argv exists" are the same fact.
        let offered = ResetPlan::for_offer(
            offer_inputs(
                DrClass::Identity,
                TccutilPresence::Executable,
                Some("a.b.c"),
            ),
            Folder::ALL,
        );
        assert_eq!(offered.map(|p| p.len()), Some(3));

        let (run, notx, gone, unk) = (
            TccutilPresence::Executable,
            TccutilPresence::NotExecutable,
            TccutilPresence::Missing,
            TccutilPresence::Unknown,
        );
        for suppressed in [
            offer_inputs(DrClass::Cdhash, run, Some("a.b.c")),
            offer_inputs(DrClass::Unsigned, run, Some("a.b.c")),
            offer_inputs(DrClass::Identity, gone, Some("a.b.c")),
            offer_inputs(DrClass::Identity, notx, Some("a.b.c")),
            offer_inputs(DrClass::Identity, unk, Some("a.b.c")),
            offer_inputs(DrClass::Identity, run, None),
            offer_inputs(DrClass::Identity, run, Some("")),
        ] {
            assert!(
                ResetPlan::for_offer(suppressed, Folder::ALL).is_none(),
                "{suppressed:?} must not produce an argv"
            );
        }
    }

    #[test]
    fn one_invocations_status_is_read_from_its_exit_alone() {
        assert_eq!(ResetStatus::from_exit_status(Some(0)), ResetStatus::Reset);
        assert_eq!(ResetStatus::from_exit_code(0), ResetStatus::Reset);
        assert_eq!(
            ResetStatus::from_exit_code(1),
            ResetStatus::Declined { code: Some(1) }
        );
        assert_eq!(
            ResetStatus::from_exit_code(64),
            ResetStatus::Declined { code: Some(64) }
        );
        // Killed by a signal: a decline with no code, never a success.
        assert_eq!(
            ResetStatus::from_exit_status(None),
            ResetStatus::Declined { code: None }
        );
        assert!(!ResetStatus::from_exit_status(None).is_reset());
        assert!(ResetStatus::Reset.is_reset());
        assert!(!ResetStatus::ToolAbsent.is_reset());
        assert_eq!(
            ResetAttempt::from_exit_status(Folder::Desktop, Some(0)),
            ResetAttempt::new(Folder::Desktop, ResetStatus::Reset)
        );
        assert_eq!(
            ResetAttempt::tool_absent(Folder::Desktop).status,
            ResetStatus::ToolAbsent
        );
        assert!(ResetAttempt::from_exit_status(Folder::Desktop, Some(0)).is_reset());
    }

    #[test]
    fn the_reset_outcome_is_a_table() {
        let ok = |f: Folder| ResetAttempt::new(f, ResetStatus::Reset);
        let bad = |f: Folder, c: i32| ResetAttempt::from_exit_status(f, Some(c));
        let gone = ResetAttempt::tool_absent;
        let cases: [(&[ResetAttempt], ResetOutcome, &str); 7] = [
            // all ok
            (
                &[
                    ok(Folder::Documents),
                    ok(Folder::Desktop),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::AllReset,
                "all-reset",
            ),
            // one nonzero
            (
                &[
                    ok(Folder::Documents),
                    bad(Folder::Desktop, 1),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::Partial,
                "partial",
            ),
            // all nonzero
            (
                &[
                    bad(Folder::Documents, 1),
                    bad(Folder::Desktop, 1),
                    bad(Folder::Downloads, 70),
                ],
                ResetOutcome::NoneReset,
                "none-reset",
            ),
            // tccutil absent — a property of the machine, not of a folder
            (
                &[
                    gone(Folder::Documents),
                    gone(Folder::Desktop),
                    gone(Folder::Downloads),
                ],
                ResetOutcome::ToolAbsent,
                "tool-absent",
            ),
            // the tool vanished after one folder had already been reset
            (
                &[ok(Folder::Documents), gone(Folder::Desktop)],
                ResetOutcome::Partial,
                "partial",
            ),
            // nothing ran, and not because the tool is missing
            (
                &[bad(Folder::Documents, 1), gone(Folder::Desktop)],
                ResetOutcome::NoneReset,
                "none-reset",
            ),
            // nothing to do is not a success
            (&[], ResetOutcome::NotAttempted, "not-attempted"),
        ];
        for (attempts, want, token) in cases {
            let got = reset_outcome(attempts);
            assert_eq!(got, want, "{attempts:?}");
            assert_eq!(got.as_str(), token);
        }
    }

    #[test]
    fn a_partial_success_is_never_reported_as_every_folder() {
        let attempts = [
            ResetAttempt::new(Folder::Documents, ResetStatus::Reset),
            ResetAttempt::from_exit_status(Folder::Desktop, Some(1)),
            ResetAttempt::from_exit_status(Folder::Downloads, None),
        ];
        let outcome = reset_outcome(&attempts);
        assert_eq!(outcome, ResetOutcome::Partial);
        assert!(outcome.any_reset());
        assert!(
            !outcome.all_reset(),
            "one success must never speak for the whole set"
        );
        assert!(ResetOutcome::AllReset.all_reset());
        assert!(!ResetOutcome::NoneReset.any_reset());
        assert!(!ResetOutcome::ToolAbsent.any_reset());
        assert!(!ResetOutcome::NotAttempted.any_reset());

        // The warm-up follows only what actually reset.
        assert_eq!(folders_reset(&attempts), vec![Folder::Documents]);
        // And each failure yields one line, naming its folder and its status.
        assert_eq!(
            folders_declined(&attempts),
            vec![(Folder::Desktop, Some(1)), (Folder::Downloads, None)]
        );
    }

    #[test]
    fn an_absent_tool_is_never_reported_as_a_refusal() {
        let attempts = [
            ResetAttempt::tool_absent(Folder::Documents),
            ResetAttempt::tool_absent(Folder::Desktop),
        ];
        assert!(
            folders_declined(&attempts).is_empty(),
            "nothing was asked, so macOS declined nothing"
        );
        assert!(folders_reset(&attempts).is_empty());
        assert_eq!(reset_outcome(&attempts), ResetOutcome::ToolAbsent);
    }

    #[test]
    fn repair_tokens_render_for_every_state() {
        assert_eq!(ResetStatus::Reset.as_str(), "reset");
        assert_eq!(ResetStatus::Declined { code: Some(1) }.as_str(), "declined");
        assert_eq!(ResetStatus::Declined { code: None }.as_str(), "declined");
        assert_eq!(ResetStatus::ToolAbsent.as_str(), "tool-absent");
        assert_eq!(ResetOffer::Offer.as_str(), "offer");
        assert_eq!(ResetOffer::ExplainRebuild.as_str(), "explain-rebuild");
        assert_eq!(ResetOffer::HideNoTool.as_str(), "hide-no-tool");
        assert_eq!(ResetOffer::HideNoBundleId.as_str(), "hide-no-bundle-id");
    }

    // -- the posture join ----------------------------------------------------

    fn inputs(adoption: Attribution, fda: FdaState) -> PostureInputs {
        PostureInputs {
            adoption,
            fda,
            ..PostureInputs::default()
        }
    }

    #[test]
    fn posture_join_is_a_table_under_todays_evidence() {
        // Nothing is measured, so nothing is ever `covered` and a prompt is
        // always possible — even when the grant is held.
        let cases = [
            (Attribution::Live, FdaState::Granted),
            (Attribution::Live, FdaState::Denied),
            (Attribution::Live, FdaState::Unknown),
            (Attribution::Adopted, FdaState::Granted),
            (Attribution::Adopted, FdaState::Denied),
            (Attribution::Adopted, FdaState::Unknown),
            (Attribution::Unknown, FdaState::Granted),
            (Attribution::Unknown, FdaState::Denied),
            (Attribution::Unknown, FdaState::Unknown),
        ];
        for (adoption, fda) in cases {
            let got = ConsentPosture::join(inputs(adoption, fda));
            assert_eq!(got.attribution, adoption);
            assert_eq!(got.fda, fda);
            assert_eq!(
                got.fs_consent,
                FsConsent::Unknown,
                "unmeasured coverage must never report covered: {adoption:?}/{fda:?}"
            );
            assert!(
                got.prompt_possible,
                "unmeasured coverage leaves a prompt possible: {adoption:?}/{fda:?}"
            );
            assert_eq!(got.fda_scope, FdaScope::Unknown);
        }
    }

    #[test]
    fn an_observed_eperm_is_the_only_route_to_denied() {
        let mut base = inputs(Attribution::Live, FdaState::Granted);
        base.observed_eperm = true;
        assert_eq!(ConsentPosture::join(base).fs_consent, FsConsent::Denied);

        // Denied on the strength of the observation alone — the FDA state does
        // not matter.
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let mut i = inputs(Attribution::Live, fda);
            i.observed_eperm = true;
            assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
        }
        // And a denied FDA state alone is NOT a session denial.
        assert_eq!(
            ConsentPosture::join(inputs(Attribution::Live, FdaState::Denied)).fs_consent,
            FsConsent::Unknown
        );
    }

    #[test]
    fn an_adopted_session_is_never_denied_before_s2() {
        let mut i = inputs(Attribution::Adopted, FdaState::Denied);
        i.observed_eperm = true;
        let got = ConsentPosture::join(i);
        assert_eq!(got.attribution, Attribution::Adopted);
        assert_eq!(
            got.fs_consent,
            FsConsent::Unknown,
            "design §3.9 pt3: adopted reports unknown, never denied, until S2"
        );

        // With S2 measured, the observation is reportable.
        i.evidence.handoff_attribution_measured = true;
        assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
    }

    #[test]
    fn covered_requires_grant_plus_live_plus_measured_coverage() {
        let measured = SpikeEvidence {
            fda_coverage_measured: true,
            ..SpikeEvidence::UNMEASURED
        };
        let mut i = inputs(Attribution::Live, FdaState::Granted);
        i.evidence = measured;
        let got = ConsentPosture::join(i);
        assert_eq!(got.fs_consent, FsConsent::Covered);
        assert!(!got.prompt_possible);

        // Drop any one of the three legs and it is Unknown again.
        let mut not_live = i;
        not_live.adoption = Attribution::Unknown;
        assert_eq!(
            ConsentPosture::join(not_live).fs_consent,
            FsConsent::Unknown
        );

        let mut not_granted = i;
        not_granted.fda = FdaState::Unknown;
        assert_eq!(
            ConsentPosture::join(not_granted).fs_consent,
            FsConsent::Unknown
        );
        assert!(ConsentPosture::join(not_granted).prompt_possible);

        let mut not_measured = i;
        not_measured.evidence = SpikeEvidence::UNMEASURED;
        assert_eq!(
            ConsentPosture::join(not_measured).fs_consent,
            FsConsent::Unknown
        );
    }

    #[test]
    fn attribution_comes_from_the_adoption_record_not_the_spi() {
        // The SPI can say anything, including nothing at all; the record wins.
        for corroboration in [
            Responsible::Unknown,
            Responsible::SelfProcess,
            Responsible::Exited,
            Responsible::Other(4402),
        ] {
            for record in [
                Attribution::Live,
                Attribution::Adopted,
                Attribution::Unknown,
            ] {
                let mut i = inputs(record, FdaState::Unknown);
                i.responsible = corroboration;
                let got = ConsentPosture::join(i);
                assert_eq!(
                    got.attribution, record,
                    "record {record:?} must survive corroboration {corroboration:?}"
                );
                assert_eq!(got.responsible, corroboration, "corroboration is carried");
            }
        }
        // Specifically: responsible_pid() returning None (Unknown) does not
        // turn a live session into an unknown one.
        let mut live = inputs(Attribution::Live, FdaState::Unknown);
        live.responsible = Responsible::Unknown;
        assert_eq!(ConsentPosture::join(live).attribution, Attribution::Live);
    }

    #[test]
    fn fda_scope_is_carried_from_the_evidence_and_is_unknown_today() {
        assert_eq!(SpikeEvidence::UNMEASURED.fda_scope, FdaScope::Unknown);
        assert_eq!(SpikeEvidence::default(), SpikeEvidence::UNMEASURED);
        let mut i = inputs(Attribution::Live, FdaState::Granted);
        i.evidence.fda_scope = FdaScope::ThisProcess;
        assert_eq!(ConsentPosture::join(i).fda_scope, FdaScope::ThisProcess);
        assert_eq!(FdaScope::Unknown.as_str(), "unknown");
        assert_eq!(FdaScope::ThisProcess.as_str(), "this_process");
        assert_eq!(FdaScope::NewProcesses.as_str(), "new_processes");
    }

    #[test]
    fn tokens_render_for_every_state() {
        assert_eq!(FdaState::Granted.as_str(), "granted");
        assert_eq!(FdaState::Denied.as_str(), "denied");
        assert_eq!(FdaState::Unknown.as_str(), "unknown");
        assert_eq!(FdaState::default(), FdaState::Unknown);
        assert_eq!(Attribution::Live.as_str(), "live");
        assert_eq!(Attribution::Adopted.as_str(), "adopted");
        assert_eq!(Attribution::Unknown.as_str(), "unknown");
        assert_eq!(Attribution::default(), Attribution::Unknown);
        assert_eq!(FsConsent::Covered.as_str(), "covered");
        assert_eq!(FsConsent::Denied.as_str(), "denied");
        assert_eq!(FsConsent::Unknown.as_str(), "unknown");
        assert_eq!(FsConsent::default(), FsConsent::Unknown);
        assert_eq!(ProbeLabel::OpenEperm.as_str(), "open_eperm");
        assert_eq!(ProbeLabel::OpenErrno(9).as_str(), "open_errno");
        assert_eq!(
            ProbeLabel::RefusedOutOfBundle.as_str(),
            "refused_out_of_bundle"
        );
    }

    // -- cache ---------------------------------------------------------------

    fn key(bundle: &str, dr: &str) -> ConsentKey {
        ConsentKey::new(bundle, dr)
    }

    #[test]
    fn a_fresh_cache_is_empty_and_a_successor_inherits_nothing() {
        // A successor that adopts sessions across an in-place apply builds its
        // own cache; there is no process-global to inherit.
        let successor = ConsentCache::new();
        assert!(successor.is_empty());
        assert_eq!(
            successor.get(
                &key("/Applications/aterm.app", "identity"),
                Duration::from_secs(5)
            ),
            None
        );

        // And an explicit clear (app activation, post-handoff) empties one.
        let cache = ConsentCache::new();
        cache.store(
            key("/Applications/aterm.app", "identity"),
            FdaProbe {
                state: FdaState::Granted,
                label: ProbeLabel::OpenOk,
            },
        );
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn the_cache_expires_at_the_caller_supplied_interval() {
        let cache = ConsentCache::new();
        let k = key("/Applications/aterm.app", "identity");
        let probe = FdaProbe {
            state: FdaState::Denied,
            label: ProbeLabel::OpenEperm,
        };
        let t0 = Instant::now();
        cache.store_at(k.clone(), probe, t0);

        let interval = Duration::from_millis(5000);
        let hit = cache
            .get_at(&k, interval, t0 + Duration::from_millis(1840))
            .expect("fresh within the interval");
        assert_eq!(hit.probe, probe);
        assert_eq!(hit.age, Duration::from_millis(1840));

        assert!(
            cache.get_at(&k, interval, t0 + interval).is_none(),
            "exactly at the interval is stale"
        );
        assert!(
            cache
                .get_at(&k, interval, t0 + Duration::from_millis(9000))
                .is_none()
        );
    }

    #[test]
    fn a_changed_identity_misses_the_cache() {
        let cache = ConsentCache::new();
        let granted = FdaProbe {
            state: FdaState::Granted,
            label: ProbeLabel::OpenOk,
        };
        let t0 = Instant::now();
        cache.store_at(key("/Applications/aterm.app", "identity-A"), granted, t0);
        let interval = Duration::from_secs(60);

        // A rebuild changed the designated requirement: a stale `granted` must
        // not survive it.
        assert!(
            cache
                .get_at(&key("/Applications/aterm.app", "cdhash-B"), interval, t0)
                .is_none()
        );
        // A different bundle likewise.
        assert!(
            cache
                .get_at(
                    &key("/Applications/aterm (dev).app", "identity-A"),
                    interval,
                    t0
                )
                .is_none()
        );
        // The same identity still hits.
        assert!(
            cache
                .get_at(&key("/Applications/aterm.app", "identity-A"), interval, t0)
                .is_some()
        );
    }

    #[test]
    fn get_or_probe_calls_the_probe_only_on_a_miss() {
        let cache = ConsentCache::new();
        let k = key("/Applications/aterm.app", "identity");
        let interval = Duration::from_secs(5);
        let t0 = Instant::now();

        let mut calls = 0u32;
        let first = cache.get_or_probe_at(&k, interval, t0, || {
            calls += 1;
            FdaProbe {
                state: FdaState::Denied,
                label: ProbeLabel::OpenEperm,
            }
        });
        assert_eq!(calls, 1);
        assert_eq!(first.probe.state, FdaState::Denied);
        assert_eq!(first.age, Duration::ZERO);

        let second = cache.get_or_probe_at(&k, interval, t0 + Duration::from_secs(1), || {
            calls += 1;
            unreachable!("probed inside the interval");
        });
        assert_eq!(calls, 1);
        assert_eq!(second.probe.state, FdaState::Denied);
        assert_eq!(second.age, Duration::from_secs(1));

        // Past the interval it probes again.
        let third = cache.get_or_probe_at(&k, interval, t0 + Duration::from_secs(6), || {
            calls += 1;
            FdaProbe {
                state: FdaState::Granted,
                label: ProbeLabel::OpenOk,
            }
        });
        assert_eq!(calls, 2);
        assert_eq!(third.probe.state, FdaState::Granted);
    }

    #[test]
    fn cache_bundle_prefers_the_bundle_root() {
        assert_eq!(
            cache_bundle_for(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
            p("/Applications/aterm.app")
        );
        assert_eq!(
            cache_bundle_for(Path::new("/Users//a/aterm/target/debug/deps/x-1")),
            p("/Users//a/aterm/target/debug/deps/x-1")
        );
    }

    // -- macOS-only ----------------------------------------------------------

    /// The pinned errno literals must match the platform, or every
    /// classification silently answers the wrong thing.
    #[cfg(target_os = "macos")]
    #[test]
    fn errno_constants_match_the_platform() {
        assert_eq!(ERRNO_EPERM, libc::EPERM);
        assert_eq!(ERRNO_ESRCH, libc::ESRCH);
    }

    /// This test binary lives under `target/debug/deps`, so the real entry
    /// point must refuse. This is the 2026-08-17 fence, exercised end to end
    /// through the same function production calls.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_probe_refuses_from_a_test_binary() {
        let got = probe_fda(ProbeGate::on());
        assert_eq!(
            got.state,
            FdaState::Unknown,
            "a test binary must never learn the FDA state"
        );
        assert!(
            got.label.refused(),
            "a test binary must never reach the syscall: {:?}",
            got.label
        );
        assert_eq!(
            got.label,
            ProbeLabel::RefusedOutOfBundle,
            "the refusal reason must be the in-bundle guard"
        );
    }

    /// `tccutil` ships with macOS, and this is the one function in the §3.7
    /// section that asks the operating system anything. It stats a file in
    /// `/usr/bin` — no spawn, no `tccd`, no `WindowServer`, nothing under a
    /// protected root — so it is safe from this test binary.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_tccutil_presence_reads_the_mode_of_a_real_file() {
        let got = tccutil_presence();
        assert_eq!(
            got,
            TccutilPresence::Executable,
            "/usr/bin/tccutil is part of macOS; {got:?} means the mode read is wrong"
        );
        assert!(got.can_run());
    }

    /// The responsibility SPI is unprivileged and contacts neither tccd nor
    /// WindowServer, so it is safe from a test binary. Asserting only that the
    /// call is TOTAL — some answer, no panic, no hang.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_responsibility_spi_is_total_for_our_own_process() {
        let me = i32::try_from(std::process::id()).expect("pid fits in i32");
        let answer = responsible_pid_detailed(me);
        match answer {
            Ok(pid) => assert!(pid >= 0, "a non-negative pid or an error, never a raw -1"),
            Err(_) => {}
        }
        // And a pid that cannot exist is never mistaken for us.
        assert_ne!(
            classify_responsible(me, responsible_pid_detailed(-1)),
            Responsible::SelfProcess
        );
    }

    /// `responsible_app` for our own process must not panic and must not
    /// invent a name. It runs from a test binary; the only OS calls it makes
    /// are the responsibility SPI, `proc_pidpath`, and possibly one bounded
    /// read of an `Info.plist` that is NOT under a protected root.
    #[cfg(target_os = "macos")]
    #[test]
    fn responsible_app_is_total_and_names_something_real() {
        let me = i32::try_from(std::process::id()).expect("pid fits in i32");
        if let Some(app) = responsible_app(me) {
            assert!(app.pid > 0);
            assert!(
                app.path.is_absolute(),
                "proc_pidpath returns absolute paths"
            );
            assert!(!app.display_name.is_empty());
        }
    }
}
